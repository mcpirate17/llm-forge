//! Explicit baseline publication over an unchanged checked-out baseline.
use anyhow::{ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn current(path: &Path) -> Result<Option<String>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.nlink() == 1 && before.len() <= 2 * 1024 * 1024,
        "unsafe baseline file"
    );
    let mut bytes = Vec::new();
    (&file).take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    let named = fs::symlink_metadata(path)?;
    ensure!(
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
            && named.dev() == before.dev()
            && named.ino() == before.ino()
            && bytes.len() <= 2 * 1024 * 1024,
        "baseline changed during reading"
    );
    Ok(Some(format!("{:x}", Sha256::digest(bytes))))
}
pub(crate) fn write(
    root: &Path,
    relative: &str,
    expected: Option<&str>,
    value: &serde_json::Value,
    mut verify: impl FnMut() -> Result<()>,
) -> Result<()> {
    crate::candidate_source::safe_path(relative.as_bytes())?;
    let path = root.join(relative);
    let parent = path.parent().context("baseline parent missing")?;
    ensure!(
        parent.canonicalize()? == parent,
        "baseline parent must have no symlink components"
    );
    let identity = fs::metadata(parent)?;
    ensure!(
        identity.is_dir()
            && identity.uid() == unsafe { libc::geteuid() }
            && identity.mode() & 0o002 == 0,
        "unsafe baseline parent"
    );
    ensure!(
        current(&path)?.as_deref() == expected,
        "checkout baseline differs from audited candidate"
    );
    let existing_mode = if expected.is_some() {
        Some(fs::symlink_metadata(&path)?.mode() & 0o777)
    } else {
        None
    };
    let temporary = Temporary(parent.join(format!(
        ".{}",
        crate::native_id::new("forge-corpus-baseline")?
    )));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&temporary.0)?;
    let data = serde_json::to_vec_pretty(value)?;
    ensure!(
        data.len() <= 2 * 1024 * 1024,
        "baseline output exceeds 2 MiB"
    );
    file.write_all(&data)?;
    file.write_all(b"\n")?;
    if let Some(mode) = existing_mode {
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    file.sync_all()?;
    let now = fs::metadata(parent)?;
    ensure!(
        identity.dev() == now.dev() && identity.ino() == now.ino(),
        "baseline parent changed"
    );
    ensure!(
        current(&path)?.as_deref() == expected,
        "checkout baseline changed during audit"
    );
    verify()?;
    fs::rename(&temporary.0, &path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn refuses_drift_and_callback_failure_without_publishing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let path = root.join("baseline.json");
        fs::write(&path, b"old").unwrap();
        let sha = format!("{:x}", Sha256::digest(b"old"));
        assert!(write(
            &root,
            "baseline.json",
            Some(&"0".repeat(64)),
            &json!({}),
            || Ok(())
        )
        .is_err());
        assert!(write(
            &root,
            "baseline.json",
            Some(&sha),
            &json!({}),
            || anyhow::bail!("drift")
        )
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        write(
            &root,
            "baseline.json",
            Some(&sha),
            &json!({"status":"RECORDED"}),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap()
                ["status"],
            "RECORDED"
        );
    }
    #[test]
    fn interrupt_at_publication_checkpoint_preserves_baseline_and_cleans_temp() {
        if std::env::var_os("FORGE_CORPUS_PUBLISH_CANCEL_CHILD").is_some() {
            let owner = crate::process_signals::Interrupts::install().unwrap();
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().canonicalize().unwrap();
            let path = root.join("baseline.json");
            fs::write(&path, b"old").unwrap();
            let sha = format!("{:x}", Sha256::digest(b"old"));
            let result = write(&root, "baseline.json", Some(&sha), &json!({}), || {
                let result = unsafe { libc::pthread_kill(libc::pthread_self(), libc::SIGTERM) };
                ensure!(result == 0, "could not signal checkpoint");
                ensure!(!owner.interrupted()?, "corpus interrupted");
                Ok(())
            });
            assert!(result.is_err());
            assert_eq!(fs::read(&path).unwrap(), b"old");
            assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
            return;
        }
        let output = crate::process_probe::run(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "mutation_corpus_record::tests::interrupt_at_publication_checkpoint_preserves_baseline_and_cleans_temp"])
                .env("FORGE_CORPUS_PUBLISH_CANCEL_CHILD", "1"),
            std::time::Duration::from_secs(5), 65536,
        ).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
