//! Bound child-controlled report and diagnostic reads before allocation.
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

fn regular(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("probe output must be a regular file"));
    }
    Ok(file)
}

pub(super) fn read(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
    let file = regular(path)?;
    if file.metadata()?.len() > maximum as u64 {
        return Err(io::Error::other("probe report exceeds byte limit"));
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(io::Error::other("probe report grew beyond byte limit"));
    }
    Ok(bytes)
}

pub(super) fn tail(path: &Path, maximum: usize) -> io::Result<Vec<u8>> {
    let mut file = regular(path)?;
    let start = file.metadata()?.len().saturating_sub(maximum as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(maximum as u64).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_reports_refuse_and_sparse_logs_read_only_the_tail() {
        let tree = crate::index_tests::Tree::new("gate-bounded-output");
        let path = tree.path().join("report");
        let mut file = File::create(&path).unwrap();
        use std::io::Write;
        file.set_len(1024 * 1024 * 1024).unwrap();
        file.seek(SeekFrom::End(-4)).unwrap();
        file.write_all(b"tail").unwrap();
        assert!(read(&path, 4096).is_err());
        assert_eq!(tail(&path, 4).unwrap(), b"tail");
        assert!(read(tree.path(), 4096).is_err());
        let link = tree.path().join("linked");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read(&link, 4096).is_err());
        assert!(tail(&link, 4).is_err());
    }
}
