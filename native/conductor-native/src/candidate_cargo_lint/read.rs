use super::{directory, io_error, metadata, path_parts, Budget};
use cap_std::fs::{Dir, Metadata, MetadataExt, OpenOptions, OpenOptionsExt};
use std::io::Read;

/// Strict descriptor-relative optional read. None means only an initially missing
/// component; changes after admission are errors. No elapsed I/O deadline claim.
/// Maximum must be 1..=32MiB. Cargo config/manifest callers retain their 1MiB cap;
/// native snapshot consumers may request the larger bound. Decoding/schema
/// validation belong to the caller.
pub fn read_optional(root: &Dir, path: &str, maximum: usize) -> Result<Option<Vec<u8>>, String> {
    read_shared(root, path, maximum, &mut Budget::new())
}
pub(super) fn read_shared(
    root: &Dir,
    path: &str,
    maximum: usize,
    budget: &mut Budget,
) -> Result<Option<Vec<u8>>, String> {
    read_checked(root, path, maximum, budget, || {})
}
pub(super) fn parent(root: &Dir, path: &str, budget: &mut Budget) -> Result<Option<Dir>, String> {
    let parts = path_parts(path, false)?;
    budget.probe()?;
    let mut dir = root
        .try_clone()
        .map_err(|e| io_error("clone read root", &e))?;
    for name in &parts[..parts.len() - 1] {
        match metadata(&dir, name, budget)? {
            None => return Ok(None),
            Some(m) if m.is_dir() => dir = directory(&dir, name, budget)?,
            _ => return Err("lint read parent must be a directory".into()),
        }
    }
    Ok(Some(dir))
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    a.is_file()
        && b.is_file()
        && a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.nlink() == b.nlink()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn read_checked(
    root: &Dir,
    path: &str,
    maximum: usize,
    budget: &mut Budget,
    before_finish: impl FnOnce(),
) -> Result<Option<Vec<u8>>, String> {
    if !(1..=32 * 1024 * 1024).contains(&maximum) {
        return Err("lint read maximum must be 1..33554432 bytes".into());
    }
    let parts = path_parts(path, false)?;
    let name = parts.last().expect("validated nonempty path");
    let Some(dir) = parent(root, path, budget)? else {
        return Ok(None);
    };
    let Some(before) = metadata(&dir, name, budget)? else {
        return Ok(None);
    };
    if !before.is_file() {
        return Err("lint read input must be a regular file".into());
    }
    if before.len() > maximum as u64 {
        return Err("lint read input exceeds byte limit".into());
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    let mut file = dir
        .open_with(name, &options)
        .map_err(|e| io_error("open read input", &e))?;
    let opened = file
        .metadata()
        .map_err(|e| io_error("inspect read input", &e))?;
    if !same(&before, &opened) {
        return Err("lint read input changed before opening".into());
    }
    let size = usize::try_from(before.len()).map_err(|_| "lint read size overflow")?;
    let mut bytes = Vec::with_capacity(size);
    (&mut file)
        .take(size as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error("read input", &e))?;
    before_finish();
    let after = file
        .metadata()
        .map_err(|e| io_error("inspect completed read", &e))?;
    let current_parent = parent(root, path, budget)?.ok_or("lint read path disappeared")?;
    let current = metadata(&current_parent, name, budget)?.ok_or("lint read path disappeared")?;
    if bytes.len() != size || !same(&opened, &after) || !same(&opened, &current) {
        return Err("lint read input identity/bytes changed".into());
    }
    Ok(Some(bytes))
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("remove owned read fixture");
        }
    }
    #[test]
    fn snapshot_reader_accepts_large_bounded_blobs_without_relaxing_caller_caps() {
        let path = std::env::temp_dir().join(crate::native_id::new("snapshot-read-test").unwrap());
        std::fs::create_dir(&path).unwrap();
        let _cleanup = Cleanup(path.clone());
        let root = Dir::open_ambient_dir(&path, cap_std::ambient_authority()).unwrap();
        let bytes = vec![b'x'; 9 * 1024 * 1024];
        std::fs::write(path.join("source.cpp"), &bytes).unwrap();
        assert!(read_optional(&root, "source.cpp", 1024 * 1024).is_err());
        assert!(read_optional(&root, "source.cpp", 8 * 1024 * 1024).is_err());
        assert_eq!(
            read_optional(&root, "source.cpp", 32 * 1024 * 1024).unwrap(),
            Some(bytes)
        );
        assert!(read_optional(&root, "source.cpp", 32 * 1024 * 1024 + 1).is_err());
    }
    #[test]
    fn read_identity_rechecks_confined_path_without_timing_race() {
        let path = std::env::temp_dir().join(crate::native_id::new("lint-read-test").unwrap());
        std::fs::create_dir(&path).unwrap();
        let _cleanup = Cleanup(path.clone());
        let root = Dir::open_ambient_dir(&path, cap_std::ambient_authority()).unwrap();
        std::fs::write(path.join("value"), b"first").unwrap();
        let result = read_checked(&root, "value", 64, &mut Budget::new(), || {
            std::fs::write(path.join("replacement"), b"other").unwrap();
            std::fs::rename(path.join("replacement"), path.join("value")).unwrap();
        });
        assert!(result.unwrap_err().contains("changed"));
        assert_eq!(std::fs::read(path.join("value")).unwrap(), b"other");
    }
}
