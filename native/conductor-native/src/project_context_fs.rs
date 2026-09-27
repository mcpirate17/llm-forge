//! Bounded, no-follow descriptor reads for project configuration.

use serde::Serialize;
use serde_json::{json, Value};
use std::ffi::CString;
use std::fs::{self, File, Metadata};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

const CONFIG_CAP: usize = 64 * 1024;

#[derive(Debug, Serialize)]
struct FsError {
    code: &'static str,
    field: &'static str,
    message: &'static str,
}

impl FsError {
    fn io() -> Self {
        Self {
            code: "CONFIG_IO",
            field: "config",
            message: "configuration could not be read",
        }
    }

    fn not_regular() -> Self {
        Self {
            code: "CONFIG_IO",
            field: "config",
            message: "opened configuration is not a regular file",
        }
    }

    fn outside() -> Self {
        Self {
            code: "PATH_OUTSIDE_PROJECT",
            field: "config",
            message: "config must remain inside the selected project",
        }
    }
}

type FsResult<T> = Result<T, FsError>;

fn decode_path(payload: &Value, field: &str) -> FsResult<PathBuf> {
    let text = payload[field].as_str().ok_or_else(FsError::io)?;
    let pairs = text.as_bytes().chunks_exact(2);
    if !pairs.remainder().is_empty() {
        return Err(FsError::io());
    }
    let bytes = pairs
        .map(|pair| {
            let digit = |byte: u8| (byte as char).to_digit(16);
            match (digit(pair[0]), digit(pair[1])) {
                (Some(high), Some(low)) => Ok(((high << 4) | low) as u8),
                _ => Err(FsError::io()),
            }
        })
        .collect::<FsResult<Vec<_>>>()?;
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 15) as usize] as char);
    }
    text
}

fn signature(metadata: &Metadata) -> [i128; 4] {
    [
        i128::from(metadata.dev()),
        i128::from(metadata.ino()),
        i128::from(metadata.len()),
        i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec()),
    ]
}

fn open_fd(path: &Path, flags: libc::c_int, parent: Option<&OwnedFd>) -> FsResult<OwnedFd> {
    let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| FsError::io())?;
    let fd = match parent {
        Some(directory) => unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) },
        None => unsafe { libc::open(name.as_ptr(), flags) },
    };
    if fd < 0 {
        return Err(FsError::io());
    }
    // SAFETY: open/openat returned a new owned descriptor above.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn open_contained_regular(root: &Path, path: &Path) -> FsResult<OwnedFd> {
    let relative = path.strip_prefix(root).map_err(|_| FsError::outside())?;
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name),
            _ => Err(FsError::outside()),
        })
        .collect::<FsResult<Vec<_>>>()?;
    let (file, directories) = components.split_last().ok_or_else(FsError::io)?;
    let directory_flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let file_flags = libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let mut directory = open_fd(root, directory_flags, None)?;
    for component in directories {
        directory = open_fd(Path::new(component), directory_flags, Some(&directory))?;
    }
    open_fd(Path::new(file), file_flags, Some(&directory))
}

fn read_after_before_stat(root: &Path, path: &Path, before: &Metadata) -> FsResult<Value> {
    let descriptor = open_contained_regular(root, path)?;
    let mut file = File::from(descriptor);
    let during = file.metadata().map_err(|_| FsError::io())?;
    if !during.is_file() {
        return Err(FsError::not_regular());
    }
    let mut raw = Vec::new();
    file.by_ref()
        .take((CONFIG_CAP + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|_| FsError::io())?;
    drop(file);
    let after = fs::metadata(path).map_err(|_| FsError::io())?;
    Ok(json!({"raw_hex": encode_hex(&raw),
        "signatures": [signature(before), signature(&during), signature(&after)]}))
}

fn read_config_bytes(root: &Path, path: &Path) -> FsResult<Value> {
    let before = fs::metadata(path).map_err(|_| FsError::io())?;
    read_after_before_stat(root, path, &before)
}

/// Expected I/O refusals are data, preserving Python's `ContextError` shape.
pub(super) fn evaluate(payload: &Value) -> Value {
    let result = decode_path(payload, "root_hex").and_then(|root| {
        decode_path(payload, "path_hex").and_then(|path| read_config_bytes(&root, &path))
    });
    match result {
        Ok(value) => json!({"value": value}),
        Err(error) => json!({"error": error}),
    }
}

#[cfg(test)]
mod tests {
    use super::{read_after_before_stat, FsError};
    use crate::project_context::evaluate;
    use serde_json::json;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "forge-context-fs-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn ancestor_swap_after_stat_cannot_escape_by_symlink() {
        let temporary = Scratch::new();
        let root = temporary.path().join("repo");
        let holder = root.join(".conductor");
        let outside = temporary.path().join("outside");
        fs::create_dir_all(&holder).unwrap();
        fs::create_dir(&outside).unwrap();
        let path = holder.join("project.toml");
        fs::write(&path, b"schema_version = 1\n").unwrap();
        fs::write(outside.join("project.toml"), b"schema_version = 2\n").unwrap();
        let before = fs::metadata(&path).unwrap();
        fs::rename(&holder, root.join("old-holder")).unwrap();
        symlink(&outside, &holder).unwrap();
        let error = read_after_before_stat(&root, &path, &before).unwrap_err();
        assert_eq!(error.code, FsError::io().code);
        assert_eq!(error.message, "configuration could not be read");
    }

    #[test]
    fn fifo_replacement_after_stat_refuses_without_reading() {
        let temporary = Scratch::new();
        let root = temporary.path().join("repo");
        fs::create_dir(&root).unwrap();
        let path = root.join("project.toml");
        fs::write(&path, b"schema_version = 1\n").unwrap();
        let before = fs::metadata(&path).unwrap();
        fs::remove_file(&path).unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: the NUL-terminated path stays alive for the call.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let error = read_after_before_stat(&root, &path, &before).unwrap_err();
        assert_eq!(error.message, "opened configuration is not a regular file");
    }

    #[test]
    fn replacement_between_stat_and_open_keeps_mismatched_signatures() {
        let temporary = Scratch::new();
        let root = temporary.path().join("repo");
        fs::create_dir(&root).unwrap();
        let path = root.join("project.toml");
        fs::write(&path, b"schema_version = 1\n").unwrap();
        let before = fs::metadata(&path).unwrap();
        fs::write(&path, b"schema_version = 2\nnew = 'changed'\n").unwrap();
        let result = read_after_before_stat(&root, &path, &before).unwrap();
        assert_ne!(result["signatures"][0], result["signatures"][1]);
        let decision = evaluate(
            "config-read",
            &json!({"size": result["raw_hex"].as_str().unwrap().len() / 2,
                "signatures": result["signatures"]}),
        );
        assert_eq!(decision["error"]["code"], "INPUT_CHANGED");
    }
}
