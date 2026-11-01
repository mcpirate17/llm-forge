//! Serialized, bounded telemetry append and rotation. Parent directories are trusted.
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const INVENTORY_LIMIT: usize = 4096;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}
fn validate(file: &File, private: bool) -> io::Result<()> {
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.nlink() != 1
        || meta.mode() & if private { 0o077 } else { 0o022 } != 0
    {
        return Err(invalid(
            "telemetry file must be owned, regular, singly linked and appropriately private",
        ));
    }
    Ok(())
}
struct Lease(File);
impl Drop for Lease {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
fn lock(path: &Path) -> io::Result<Lease> {
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(PathBuf::from(name))?;
    validate(&file, true)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Lease(file));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::WouldBlock && error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "telemetry writer lock timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn open_log(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    validate(&file, false)?;
    Ok(file)
}
fn prefix(path: &Path) -> io::Result<String> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| invalid("telemetry filename must be UTF-8"))?;
    Ok(format!("{name}.forge-rotation-"))
}
fn rotation_name_matches(name: &str, prefix: &str) -> bool {
    let Some(tail) = name.strip_prefix(prefix) else {
        return false;
    };
    let Some((pid, nonce)) = tail.split_once('-') else {
        return false;
    };
    !pid.is_empty()
        && pid.bytes().all(|b| b.is_ascii_digit())
        && nonce.len() == 32
        && nonce
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn prune(path: &Path, keep: usize) -> io::Result<()> {
    let prefix = prefix(path)?;
    let mut found = Vec::new();
    for (count, entry) in fs::read_dir(parent(path))?.enumerate() {
        if count >= INVENTORY_LIMIT {
            return Err(invalid("telemetry rotation inventory exceeds 4096 entries"));
        }
        let entry = entry?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|s| rotation_name_matches(s, &prefix))
        {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.mode() & 0o022 != 0
        {
            return Err(invalid("unsafe telemetry rotation entry"));
        }
        found.push((metadata.modified()?, entry.path()));
    }
    found.sort();
    let remove = found.len().saturating_sub(keep);
    for (_, stale) in found.into_iter().take(remove) {
        fs::remove_file(stale)?;
    }
    Ok(())
}
fn rotate(path: &Path) -> io::Result<()> {
    let id = forge::resource_id("rotation").map_err(io::Error::other)?;
    let tail = id
        .strip_prefix("rotation-")
        .ok_or_else(|| invalid("invalid rotation identity"))?;
    let target = parent(path).join(format!("{}{tail}", prefix(path)?));
    let source = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| invalid("telemetry path contains NUL"))?;
    let destination = CString::new(target.as_os_str().as_bytes())
        .map_err(|_| invalid("telemetry path contains NUL"))?;
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
pub(super) fn record(path: &Path, encoded: &[u8], max_bytes: u64, keep: usize) -> io::Result<()> {
    if encoded.len() as u64 > max_bytes {
        return Err(invalid("telemetry record exceeds log budget"));
    }
    fs::create_dir_all(parent(path))?;
    let _lease = lock(path)?;
    let mut file = open_log(path)?;
    if file.metadata()?.len().saturating_add(encoded.len() as u64) > max_bytes {
        // Validate the bounded inventory before changing the active log.
        prune(path, keep.saturating_sub(1))?;
        drop(file);
        rotate(path)?;
        file = open_log(path)?;
    }
    file.write_all(encoded)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::Arc;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(forge::resource_id("telemetry-test").unwrap());
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn unsafe_sink_types_and_oversized_records_fail_without_modification() {
        let dir = Scratch::new();
        let sentinel = dir.0.join("sentinel");
        let path = dir.0.join("events.jsonl");
        fs::write(&sentinel, b"original").unwrap();
        symlink(&sentinel, &path).unwrap();
        assert!(record(&path, b"x\n", 16, 5).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"original");
        fs::remove_file(&path).unwrap();
        fs::hard_link(&sentinel, &path).unwrap();
        assert!(record(&path, b"x\n", 16, 5).is_err());
        fs::remove_file(&path).unwrap();
        let raw = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        assert!(record(&path, b"x\n", 16, 5).is_err());
        fs::remove_file(&path).unwrap();
        assert!(record(&path, b"this record exceeds", 3, 5).is_err());
        assert!(!path.exists());
        assert_eq!(fs::read(&sentinel).unwrap(), b"original");
    }

    #[test]
    fn rotation_prunes_only_exact_generated_names_and_is_private() {
        let dir = Scratch::new();
        let path = dir.0.join("events.jsonl");
        let foreign = dir.0.join("events.jsonl.forge-rotation-important");
        fs::write(&foreign, b"preserve").unwrap();
        for i in 0..8 {
            record(&path, format!("{i}\n").as_bytes(), 2, 2).unwrap();
        }
        assert_eq!(fs::read(&foreign).unwrap(), b"preserve");
        let count = fs::read_dir(&dir.0)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|name| {
                name.to_str()
                    .is_some_and(|s| rotation_name_matches(s, &prefix(&path).unwrap()))
            })
            .count();
        assert_eq!(count, 2);
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn concurrent_writers_rotate_without_losing_or_duplicating_events() {
        let dir = Scratch::new();
        let path = Arc::new(dir.0.join("events.jsonl"));
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for j in 0..30 {
                        record(&path, format!("{i}:{j}\n").as_bytes(), 64, 100).unwrap();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let mut rows = Vec::new();
        for entry in fs::read_dir(&dir.0).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if entry.path() == *path
                || name
                    .to_str()
                    .is_some_and(|s| rotation_name_matches(s, &prefix(&path).unwrap()))
            {
                rows.extend(
                    fs::read_to_string(entry.path())
                        .unwrap()
                        .lines()
                        .map(str::to_owned),
                );
            }
        }
        rows.sort();
        rows.dedup();
        assert_eq!(rows.len(), 240);
    }

    #[test]
    fn inventory_limit_fails_before_rotation() {
        let dir = Scratch::new();
        let path = dir.0.join("events.jsonl");
        fs::write(&path, b"old\n").unwrap();
        for i in 0..INVENTORY_LIMIT {
            fs::write(dir.0.join(format!("foreign-{i}")), b"").unwrap();
        }
        assert!(record(&path, b"new\n", 4, 5).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old\n");
    }
}
