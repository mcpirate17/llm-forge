use super::*;
use std::fs;
use std::io::Write;
use std::os::unix::{
    ffi::{OsStrExt, OsStringExt},
    fs::symlink,
};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-graph-files-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn error(root: &SourceRoot, path: &str, contains: &str) {
    let error = root.hash(path).unwrap_err().to_string();
    assert!(error.contains(contains), "{error}");
}

#[test]
fn literal_text_and_binary_sha_are_full_byte_hashes_for_relative_and_absolute_paths() {
    let fixture = Fixture::new();
    fixture.write("nested/text.py", b"abc");
    fixture.write("binary.rs", [0, 255, 128, b'a']);
    let root = SourceRoot::open(&fixture.0).unwrap();
    let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    assert_eq!(
        root.hash("nested/text.py").unwrap().as_deref(),
        Some(expected)
    );
    assert_eq!(
        root.hash(root.host.join("nested/text.py").to_str().unwrap())
            .unwrap()
            .as_deref(),
        Some(expected)
    );
    assert_eq!(
        root.hash("binary.rs").unwrap().as_deref(),
        Some("79301df919df82d717f591339d85235fb8bb8683b2c74680306959327d4a4464")
    );
    assert_eq!(root.hash("missing.py").unwrap(), None);
    assert_eq!(root.hash("absent-parent/missing.py").unwrap(), None);
    assert_eq!(
        root.normalize("nested/text.py").unwrap(),
        Path::new("nested/text.py")
    );
}

#[test]
fn paths_refuse_outside_traversal_git_nul_and_resource_excess() {
    let fixture = Fixture::new();
    let root = SourceRoot::open(&fixture.0).unwrap();
    for path in [
        "../outside.py",
        "safe/../outside.py",
        "./safe.py",
        "safe/./file.py",
        ".git/config",
        "safe/.git/file.py",
        "bad\0name",
        "",
    ] {
        assert!(root.normalize(path).is_err(), "{path:?}");
    }
    let outside = format!("{}-other/source.py", root.host.display());
    assert!(root.normalize(&outside).is_err());
    assert!(root.normalize(&"x".repeat(MAX_PATH_BYTES + 1)).is_err());
    let many = format!("{}file.py", "a/".repeat(MAX_COMPONENTS + 1));
    assert!(root
        .normalize(&many)
        .unwrap_err()
        .to_string()
        .contains("component limit"));
    assert!(root.normalize(root.host.to_str().unwrap()).is_err());
}

#[test]
fn symlink_parent_final_and_dangling_links_are_errors_even_inside_host() {
    let fixture = Fixture::new();
    let note = fixture.write("real/note.py", "facts");
    symlink(fixture.0.join("real"), fixture.0.join("alias-parent")).unwrap();
    symlink(note, fixture.0.join("alias.py")).unwrap();
    symlink("missing.py", fixture.0.join("dangling.py")).unwrap();
    let root = SourceRoot::open(&fixture.0).unwrap();
    for path in ["alias-parent/note.py", "alias.py", "dangling.py"] {
        error(&root, path, "without symlinks");
    }
}

#[test]
fn fifo_and_directory_endpoints_fail_as_nonregular_without_reading() {
    let fixture = Fixture::new();
    let fifo = fixture.0.join("pipe");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    fs::create_dir(fixture.0.join("directory")).unwrap();
    let root = SourceRoot::open(&fixture.0).unwrap();
    error(&root, "pipe", "regular file");
    error(&root, "directory", "regular file");
}

#[test]
fn byte_limit_accepts_boundary_and_refuses_initial_or_concurrent_growth() {
    let fixture = Fixture::new();
    let path = fixture.write("note.py", b"1234");
    let root = SourceRoot::with_limit(&fixture.0, 4).unwrap();
    assert!(root.hash("note.py").unwrap().is_some());
    fs::write(&path, b"12345").unwrap();
    error(&root, "note.py", "byte limit");
    fs::write(&path, b"1234").unwrap();
    let result = root.hash_checked("note.py", |at, _| {
        if at == Boundary::BeforeRead {
            fs::OpenOptions::new()
                .append(true)
                .open(&path)?
                .write_all(b"5")?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("byte limit"));
}

#[test]
fn same_length_content_change_after_read_invalidates_descriptor_evidence() {
    let fixture = Fixture::new();
    let path = fixture.write("note.py", b"original");
    let root = SourceRoot::open(&fixture.0).unwrap();
    let result = root.hash_checked("note.py", |at, _| {
        if at == Boundary::Read {
            fs::write(&path, b"modified")?;
        }
        Ok(())
    });
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("changed while hashing"));
}

#[test]
fn atomic_replacement_after_read_invalidates_unchanged_file_descriptor() {
    let fixture = Fixture::new();
    let path = fixture.write("note.py", b"original");
    let replacement = fixture.write("replacement.py", b"original");
    let root = SourceRoot::open(&fixture.0).unwrap();
    let result = root.hash_checked("note.py", |at, _| {
        if at == Boundary::Read {
            fs::rename(&replacement, &path)?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("path changed"));
}

#[test]
fn disappearing_endpoint_after_read_is_changed_not_missing() {
    let fixture = Fixture::new();
    let path = fixture.write("note.py", b"facts");
    let root = SourceRoot::open(&fixture.0).unwrap();
    let result = root.hash_checked("note.py", |at, _| {
        if at == Boundary::Read {
            fs::remove_file(&path)?;
        }
        Ok(())
    });
    // Unlink may first change held-file ctime; either refusal is acceptable.
    let message = result.unwrap_err().to_string();
    assert!(
        message.contains("changed") || message.contains("disappeared"),
        "{message}"
    );
}

#[test]
fn root_rename_symlink_before_open_cannot_redirect_held_capability() {
    let fixture = Fixture::new();
    let host = fixture.0.join("host");
    let external = fixture.0.join("outside");
    fs::create_dir(&host).unwrap();
    fs::create_dir(&external).unwrap();
    fs::write(host.join("note.py"), b"facts").unwrap();
    fs::write(external.join("note.py"), b"outside bytes").unwrap();
    let root = SourceRoot::open(&host).unwrap();
    let moved = fixture.0.join("moved");
    let result = root.hash_checked("note.py", |at, _| {
        if at == Boundary::Admitted {
            fs::rename(&host, &moved)?;
            symlink(&external, &host)?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("root changed"));
}

#[test]
fn root_inode_replacement_after_read_refuses_even_identical_bytes() {
    let fixture = Fixture::new();
    let host = fixture.0.join("host");
    let replacement = fixture.0.join("replacement");
    fs::create_dir(&host).unwrap();
    fs::create_dir(&replacement).unwrap();
    fs::write(host.join("note.py"), b"facts").unwrap();
    fs::write(replacement.join("note.py"), b"facts").unwrap();
    let root = SourceRoot::open(&host).unwrap();
    let moved = fixture.0.join("moved");
    let result = root.hash_checked("note.py", |at, _| {
        if at == Boundary::Read {
            fs::rename(&host, &moved)?;
            fs::rename(&replacement, &host)?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("root changed"));
}

#[test]
fn missing_or_nonutf8_host_is_refused() {
    let fixture = Fixture::new();
    assert!(SourceRoot::open(&fixture.0.join("absent")).is_err());
    let path = fixture.0.join(std::ffi::OsString::from_vec(vec![255]));
    fs::create_dir(&path).unwrap();
    assert!(SourceRoot::open(&path)
        .err()
        .unwrap()
        .to_string()
        .contains("UTF-8"));
}

#[test]
fn text_snapshot_hashes_exact_unicode_across_chunk_boundary() {
    let fixture = Fixture::new();
    let text = format!("{}界tail", "x".repeat(64 * 1024 - 1));
    fixture.write("note.py", text.as_bytes());
    let root = SourceRoot::open(&fixture.0).unwrap();
    let snapshot = root.read_text("note.py", text.len()).unwrap().unwrap();
    assert_eq!(snapshot.text, text);
    assert_eq!(snapshot.sha256, root.hash("note.py").unwrap().unwrap());
    assert!(root.read_text("missing", 1).unwrap().is_none());
}

#[test]
fn text_snapshot_refuses_limits_nul_and_invalid_utf8_without_changing_binary_hash() {
    let fixture = Fixture::new();
    let path = fixture.write("note.py", b"ab\0c");
    let root = SourceRoot::open(&fixture.0).unwrap();
    assert!(root.hash("note.py").unwrap().is_some());
    assert!(root
        .read_text("note.py", 4)
        .unwrap_err()
        .to_string()
        .contains("NUL"));
    fs::write(&path, [0xff]).unwrap();
    assert!(root
        .read_text("note.py", 1)
        .unwrap_err()
        .to_string()
        .contains("UTF-8"));
    fs::write(&path, "é").unwrap();
    assert!(root
        .read_text("note.py", 1)
        .unwrap_err()
        .to_string()
        .contains("byte limit"));
    assert_eq!(root.read_text("note.py", 2).unwrap().unwrap().text, "é");
    assert!(root.read_text("note.py", 0).is_err());
    assert!(root.read_text("note.py", MAX_SOURCE_BYTES + 1).is_err());
}

#[test]
fn text_snapshot_rejects_changed_path_contents_and_root() {
    let fixture = Fixture::new();
    let path = fixture.write("note.py", "original");
    let root = SourceRoot::open(&fixture.0).unwrap();
    let result = root.read_text_checked("note.py", 64, |at, _| {
        if at == Boundary::Read {
            fs::write(&path, "modified")?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("changed"));
    let replacement = fixture.write("replace.py", "modified");
    let result = root.read_text_checked("note.py", 64, |at, _| {
        if at == Boundary::Read {
            fs::rename(&replacement, &path)?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("path changed"));
    let host = fixture.0.join("host");
    let moved = fixture.0.join("moved");
    fs::create_dir(&host).unwrap();
    fs::write(host.join("note.py"), "facts").unwrap();
    let pinned = SourceRoot::open(&host).unwrap();
    let result = pinned.read_text_checked("note.py", 64, |at, _| {
        if at == Boundary::Read {
            fs::rename(&host, &moved)?;
            fs::create_dir(&host)?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("root changed"));
}
