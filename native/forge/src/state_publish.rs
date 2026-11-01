//! Atomic publication of state bytes in an operator-controlled directory.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

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
