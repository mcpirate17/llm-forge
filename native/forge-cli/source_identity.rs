//! Build-time observation: prove the exact bounded worktree input inventory
//! matches the declared Git commit. This is not an authenticated build.
use crate::source_fingerprint::Fingerprint;
use sha1::{Digest, Sha1};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
const PATHS: &[&str] = &[
    "native/forge/Cargo.toml",
    "native/forge/Cargo.lock",
    "native/forge/src",
    "native/forge/build.rs",
    "native/conductor-native/Cargo.toml",
    "native/conductor-native/Cargo.lock",
    "native/conductor-native/src",
    "native/conductor-native/build.rs",
    "native/slop-core/Cargo.toml",
    "native/slop-core/Cargo.lock",
    "native/slop-core/src",
    "native/slop-core/build.rs",
];
const MAX_LIST: u64 = 32 * 1024 * 1024;
fn command(root: &Path) -> Command {
    let mut command = Command::new("git");
    let path = std::env::var_os("PATH");
    command
        .env_clear()
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_ALLOW_PROTOCOL", "file")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("LC_ALL", "C")
        .stderr(Stdio::null());
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command
}
fn git_output(root: &Path, args: &[&str], maximum: u64) -> Option<Vec<u8>> {
    let mut child = command(root)
        .args(args)
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut output = Vec::new();
    let read = child
        .stdout
        .take()?
        .take(maximum + 1)
        .read_to_end(&mut output);
    if read.is_err() || output.len() as u64 > maximum {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    if !child.wait().ok()?.success() {
        return None;
    }
    Some(output)
}
fn git_line(root: &Path, args: &[&str]) -> Option<String> {
    let raw = git_output(root, args, 4096)?;
    Some(std::str::from_utf8(&raw).ok()?.trim().to_owned())
}
pub fn revision(root: &Path) -> Option<String> {
    let top = git_line(root, &["rev-parse", "--show-toplevel"])?;
    if fs::canonicalize(top).ok()? != fs::canonicalize(root).ok()? {
        return None;
    }
    let head = git_line(root, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if head.len() != 40
        || !head
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    Some(head)
}
fn listing(root: &Path, head: &str) -> Option<BTreeMap<PathBuf, String>> {
    let mut args = vec!["ls-tree", "-r", "-z", "--full-tree", head, "--"];
    args.extend_from_slice(PATHS);
    args.extend_from_slice(&crate::source_fingerprint::EXTRA_FILES);
    let raw = git_output(root, &args, MAX_LIST)?;
    let mut files = BTreeMap::new();
    for row in raw.split(|byte| *byte == 0).filter(|row| !row.is_empty()) {
        let tab = row.iter().position(|byte| *byte == b'\t')?;
        let (header, path) = (&row[..tab], &row[tab + 1..]);
        let text = std::str::from_utf8(header).ok()?;
        let mut fields = text.split(' ');
        let (mode, kind, oid) = (fields.next()?, fields.next()?, fields.next()?);
        if fields.next().is_some()
            || !matches!(mode, "100644" | "100755")
            || kind != "blob"
            || oid.len() != 40
        {
            return None;
        }
        let path = std::str::from_utf8(path).ok()?;
        if path.len() > 4096
            || files.insert(PathBuf::from(path), oid.to_owned()).is_some()
            || files.len() > 4096
        {
            return None;
        }
    }
    Some(files)
}
#[cfg(target_os = "linux")]
fn read(root: &Path, path: &Path) -> Option<Vec<u8>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(root.join(path))
        .ok()?;
    let before = file.metadata().ok()?;
    if !before.is_file() || before.len() > 2 * 1024 * 1024 {
        return None;
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    let after = file.metadata().ok()?;
    if bytes.len() as u64 != before.len()
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return None;
    }
    Some(bytes)
}
#[cfg(not(target_os = "linux"))]
fn read(_root: &Path, _path: &Path) -> Option<Vec<u8>> {
    None
}
pub fn matches_head(root: &Path, head: &str, files: &[PathBuf], expected: &str) -> bool {
    let Some(tracked) = listing(root, head) else {
        return false;
    };
    if tracked.len() != files.len() || files.iter().any(|path| !tracked.contains_key(path)) {
        return false;
    }
    let mut fingerprint = Fingerprint::new();
    for path in files {
        let Some(bytes) = read(root, path) else {
            return false;
        };
        let mut blob = Sha1::new();
        blob.update(format!("blob {}\0", bytes.len()).as_bytes());
        blob.update(&bytes);
        if format!("{:x}", blob.finalize()) != tracked[path]
            || fingerprint.add(path, &bytes).is_err()
        {
            return false;
        }
    }
    fingerprint.finish() == expected && revision(root).as_deref() == Some(head)
}
