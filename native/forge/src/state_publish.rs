//! Atomic publication of state bytes in an operator-controlled directory.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Seal an owned single-link regular file without following its final component.
/// Parent directories and subsequent same-UID replacement remain trusted.
pub(crate) fn seal_file(path: &Path) -> io::Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let before = file.metadata()?;
    if !before.is_file() || before.uid() != unsafe { libc::geteuid() } || before.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sealed file must be owned, regular and singly linked",
        ));
    }
    file.set_permissions(fs::Permissions::from_mode(0o400))?;
    file.sync_all()?;
    let current = fs::symlink_metadata(path)?;
    let after = file.metadata()?;
    if !current.is_file()
        || current.nlink() != 1
        || after.nlink() != 1
        || (current.dev(), current.ino()) != (after.dev(), after.ino())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "sealed file path changed",
        ));
    }
    Ok(())
}

/// Publish complete bytes with a private, exclusively created staging inode.
/// An error syncing the directory occurs after the destination is visible.
pub fn publish(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = crate::resource_id("state-pending").map_err(io::Error::other)?;
    let temporary = parent.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("state published but directory sync failed: {error}"),
                )
            })?;
        Ok(())
    })();
    if result.is_err() {
        if let Err(error) = fs::remove_file(&temporary) {
            if error.kind() != io::ErrorKind::NotFound {
                eprintln!("forge: state staging cleanup: {error}");
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    #[test]
    fn seal_rejects_symlinks_and_hardlinks_without_changing_sentinel_mode() {
        let root = std::env::temp_dir().join(crate::resource_id("seal-test").unwrap());
        fs::create_dir(&root).unwrap();
        let sentinel = root.join("sentinel");
        let target = root.join("target");
        fs::write(&sentinel, b"keep").unwrap();
        fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&sentinel, &target).unwrap();
        assert!(seal_file(&target).is_err());
        assert_eq!(fs::metadata(&sentinel).unwrap().mode() & 0o777, 0o600);
        fs::remove_file(&target).unwrap();
        fs::hard_link(&sentinel, &target).unwrap();
        assert!(seal_file(&target).is_err());
        assert_eq!(fs::metadata(&sentinel).unwrap().mode() & 0o777, 0o600);
        fs::remove_file(&target).unwrap();
        seal_file(&sentinel).unwrap();
        assert_eq!(fs::metadata(&sentinel).unwrap().mode() & 0o777, 0o400);
        fs::remove_dir_all(root).unwrap();
    }
}
