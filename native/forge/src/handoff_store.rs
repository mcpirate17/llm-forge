//! Durable single-envelope storage. Cooperating writers lock across read/modify/
//! publish; unconfined same-UID writers remain trusted. SHA-256 detects corruption,
//! not a malicious writer able to replace the envelope and recompute its hash.
use super::Envelope;
use anyhow::{ensure, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const MAX_BYTES: usize = 1024 * 1024;

fn parent(path: &Path) -> Result<&Path> {
    ensure!(path.file_name().is_some(), "handoff path needs a file name");
    Ok(path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new(".")))
}

fn validate_file(file: &File) -> Result<()> {
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.nlink() == 1
            && meta.mode() & 0o077 == 0,
        "handoff file must be private, owned and singly linked"
    );
    Ok(())
}

pub(super) fn lock(path: &Path) -> Result<File> {
    let parent = parent(path)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(PathBuf::from(name))?;
    validate_file(&file)?;
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "handoff writer is busy; retry"
    );
    Ok(file)
}

pub(super) fn load(path: &Path) -> Result<Envelope> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    validate_file(&file)?;
    ensure!(
        file.metadata()?.len() <= MAX_BYTES as u64,
        "handoff exceeds 1 MiB"
    );
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_BYTES, "handoff exceeds 1 MiB");
    Envelope::from_value(forge::tool_schema::parse_document(std::str::from_utf8(
        &bytes,
    )?)?)
}

/// Caller holds the sibling lock across any dependent load and this publish.
pub(super) fn save(path: &Path, envelope: &Envelope) -> Result<()> {
    let payload = serde_json::to_vec_pretty(&envelope.payload()?)?;
    ensure!(payload.len() <= MAX_BYTES, "handoff exceeds 1 MiB");
    // Refuse to replace a symlink/device/shared file even though rename itself
    // would not follow the final symlink. Parent directories are operator-owned.
    match fs::symlink_metadata(path) {
        Ok(_) => {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)?;
            validate_file(&file)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    let dir = parent(path)?;
    let temporary = dir.join(forge::resource_id("handoff-pending")?);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let outcome = (|| -> Result<()> {
        file.write_all(&payload)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(dir)?
            .sync_all()
            .context("handoff published, directory sync failed")?;
        Ok(())
    })();
    if outcome.is_err() {
        if let Err(error) = fs::remove_file(&temporary) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("forge: handoff staging cleanup: {error}");
            }
        }
    }
    outcome
}
