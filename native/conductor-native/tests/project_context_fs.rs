#![cfg(unix)]

use conductor_native::project_context::evaluate;
use serde_json::{json, Value};
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
            "forge-context-fs-contract-{}-{}",
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

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 15) as usize] as char);
    }
    result
}

fn request(root: &Path, path: &Path) -> Value {
    json!({"root_hex": hex(root.as_os_str().as_bytes()),
        "path_hex": hex(path.as_os_str().as_bytes())})
}

fn read(root: &Path, path: &Path) -> Value {
    evaluate("read-config-bytes", &request(root, path))
}

#[test]
fn descriptor_read_preserves_raw_bytes_and_three_matching_signatures() {
    let scratch = Scratch::new();
    let root = scratch.path().join("repo");
    let holder = root.join(".conductor");
    fs::create_dir_all(&holder).unwrap();
    let path = holder.join("project.toml");
    let raw = b"schema_version = 1\n[project]\nid = 'probe'\n";
    fs::write(&path, raw).unwrap();
    let value = read(&root, &path)["value"].clone();
    assert_eq!(value["raw_hex"], hex(raw));
    let signatures = value["signatures"].as_array().unwrap();
    assert_eq!(signatures.len(), 3);
    assert_eq!(signatures[0], signatures[1]);
    assert_eq!(signatures[1], signatures[2]);
    assert_eq!(
        evaluate(
            "config-read",
            &json!({"size":raw.len(), "signatures":signatures})
        )["value"],
        json!({})
    );
}

#[test]
fn oversized_input_reads_only_cap_plus_one_and_reports_too_large() {
    let scratch = Scratch::new();
    let root = scratch.path().join("repo");
    fs::create_dir(&root).unwrap();
    let path = root.join("project.toml");
    fs::write(&path, vec![b'a'; 100_000]).unwrap();
    let value = read(&root, &path)["value"].clone();
    assert_eq!(value["raw_hex"].as_str().unwrap().len(), 2 * (65_536 + 1));
    let decision = evaluate(
        "config-read",
        &json!({"size":65_537, "signatures":value["signatures"]}),
    );
    assert_eq!(decision["error"]["code"], "CONFIG_TOO_LARGE");
    assert_eq!(decision["error"]["field"], "config");
}

#[test]
fn path_outside_root_and_symlinked_file_fail_closed() {
    let scratch = Scratch::new();
    let root = scratch.path().join("repo");
    fs::create_dir(&root).unwrap();
    let outside = scratch.path().join("outside.toml");
    fs::write(&outside, b"schema_version = 1\n").unwrap();
    assert_eq!(
        read(&root, &outside)["error"]["code"],
        "PATH_OUTSIDE_PROJECT"
    );
    let linked = root.join("project.toml");
    symlink(&outside, &linked).unwrap();
    assert_eq!(read(&root, &linked)["error"]["code"], "CONFIG_IO");
}

#[test]
fn symlinked_ancestor_and_fifo_refuse_without_following_or_blocking() {
    let scratch = Scratch::new();
    let root = scratch.path().join("repo");
    let outside = scratch.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("project.toml"), b"schema_version = 1\n").unwrap();
    let holder = root.join(".conductor");
    symlink(&outside, &holder).unwrap();
    assert_eq!(
        read(&root, &holder.join("project.toml"))["error"]["code"],
        "CONFIG_IO"
    );
    fs::remove_file(&holder).unwrap();
    fs::create_dir(&holder).unwrap();
    let fifo = holder.join("project.toml");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: the NUL-terminated path stays alive for the call.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let error = read(&root, &fifo);
    assert_eq!(error["error"]["code"], "CONFIG_IO");
    assert_eq!(
        error["error"]["message"],
        "opened configuration is not a regular file"
    );
}

#[test]
fn non_utf8_filename_is_read_as_raw_filesystem_bytes() {
    use std::os::unix::ffi::OsStringExt;

    let scratch = Scratch::new();
    let root = scratch.path().join("repo");
    fs::create_dir(&root).unwrap();
    let path = root.join(std::ffi::OsString::from_vec(b"project-\xff.toml".to_vec()));
    fs::write(&path, b"schema_version = 1\n").unwrap();
    assert_eq!(
        read(&root, &path)["value"]["raw_hex"],
        hex(b"schema_version = 1\n")
    );
}
