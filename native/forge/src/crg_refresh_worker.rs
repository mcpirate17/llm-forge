//! One bounded native index supervisor, owning the inherited flock across exec.
//! Pending -> inflight is durable before a batch starts. A killed worker leaves
//! its inflight marker for the next request/query to recover.
use super::{ensure_store, flock_exclusive, flock_unlock, marker_path, open_flock_target};
use anyhow::{ensure, Context, Result};
use serde_json::json;
use std::fs::{self, File};
use std::io::Write;
use std::os::fd::FromRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn worker_lock(store: &Path, inherited: Option<i32>) -> Result<Option<File>> {
    let path = marker_path(store, "refresh.lock");
    let Some(fd) = inherited else {
        let expected = open_flock_target(&path)?;
        return Ok(flock_exclusive(&expected, true)?.then_some(expected));
    };
    ensure!(fd >= 3, "invalid inherited graph worker descriptor");
    // SAFETY: F_GETFD proves the descriptor exists. The hidden worker command
    // receives ownership of this one descriptor from spawn_worker's pre_exec.
    ensure!(
        unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0,
        "missing inherited graph worker descriptor"
    );
    let inherited = unsafe { File::from_raw_fd(fd) };
    // Do not open anything before taking ownership: a missing supplied fd could
    // otherwise be reused by this open and create two File owners for one fd.
    let expected = open_flock_target(&path)?;
    let actual = inherited.metadata()?;
    let expected_meta = expected.metadata()?;
    ensure!(
        actual.dev() == expected_meta.dev() && actual.ino() == expected_meta.ino(),
        "graph worker lock identity mismatch"
    );
    ensure!(
        flock_exclusive(&inherited, true)?,
        "graph worker lock is held elsewhere"
    );
    ensure!(
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
        "set graph worker lock close-on-exec"
    );
    Ok(Some(inherited))
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn has_marker(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(
                meta.is_file()
                    && meta.uid() == unsafe { libc::geteuid() }
                    && meta.nlink() == 1
                    && meta.len() <= 262144,
                "invalid graph refresh marker"
            );
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn claim_pending(store: &Path) -> Result<bool> {
    let pending = marker_path(store, "refresh.pending");
    let inflight = marker_path(store, "refresh.inflight");
    let recovering = has_marker(&inflight)?;
    if has_marker(&pending)? {
        fs::rename(pending, inflight)?;
        File::open(store)?.sync_all()?;
        return Ok(true);
    }
    Ok(recovering)
}

fn record_failure(store: &Path, error: &str) -> Result<()> {
    use std::io::{Seek, SeekFrom};
    let bounded: String = error.chars().take(2000).collect();
    let bytes = serde_json::to_vec(&json!({"kind":"failure","paths":["*"],"text":bounded}))?;
    let mut notice = open_flock_target(&marker_path(store, "refresh.failed"))?;
    if notice.metadata()?.len() + bytes.len() as u64 > 65536 {
        notice.set_len(0)?;
    }
    notice.seek(SeekFrom::End(0))?;
    notice.write_all(&bytes)?;
    notice.write_all(b"\n")?;
    notice.sync_all()?;
    // Unlike the read-once notice, this remains until a successful batch.
    let mut stale = open_flock_target(&marker_path(store, "refresh.stale"))?;
    stale.set_len(0)?;
    stale.write_all(bounded.as_bytes())?;
    stale.sync_all()?;
    Ok(())
}

fn batch(host: &Path, remaining: Duration) -> Result<()> {
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["graph", "--host"]).arg(host).arg("index");
    let output = forge::process_probe::run(&mut command, remaining, 65536)?;
    ensure!(
        output.status.success(),
        "native graph index {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("native graph index report")?;
    ensure!(
        report["authority"] == "forge-native-structural-graph",
        "unexpected graph index report authority"
    );
    Ok(())
}

pub fn run(host: &Path, inherited: Option<i32>) -> Result<u8> {
    let host = host.canonicalize()?;
    let store = ensure_store(&host)?;
    let Some(worker) = worker_lock(&store, inherited)? else {
        return Ok(0);
    };
    let deadline = Instant::now() + Duration::from_secs(300);
    // This isolated supervisor has no other work during debounce; the kernel
    // timer sleeps once per batch, never in a child-completion polling loop.
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let queue = open_flock_target(&marker_path(&store, "refresh.queue.lock"))?;
        flock_exclusive(&queue, false)?;
        if !claim_pending(&store)? {
            // Release while the queue is locked: a new writer either reaches
            // this worker's next batch or observes it unlocked and starts one.
            flock_unlock(&worker);
            return Ok(0);
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            record_failure(&store,"native graph worker reached its 300-second lifetime; queued work retained for retry")?;
            flock_unlock(&worker);
            return Ok(1);
        };
        drop(queue);
        let result = batch(&host, remaining);
        let queue = open_flock_target(&marker_path(&store, "refresh.queue.lock"))?;
        flock_exclusive(&queue, false)?;
        if let Err(error) = &result {
            record_failure(&store, &format!("{error:#}"))?;
        } else {
            remove_if_present(&marker_path(&store, "refresh.stale"))?;
        }
        remove_if_present(&marker_path(&store, "refresh.inflight"))?;
        File::open(&store)?.sync_all()?;
        drop(queue);
    }
}
