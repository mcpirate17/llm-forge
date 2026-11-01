use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

fn fixture(bytes: &[u8], mode: u32) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let tool = dir.path().join("clang-format");
    fs::write(&tool, bytes).unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(mode)).unwrap();
    (dir, tool)
}

#[test]
fn executable_admission_rejects_scripts_writable_files_and_aliases() {
    let (_dir, script) = fixture(b"#!/bin/sh\nexit 0\n", 0o755);
    assert!(inspect(&script).is_err());
    fs::write(&script, b"\x7fELFfixture").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(inspect(&script).is_err());
    fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(inspect(&script).is_err());
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let alias = script.with_file_name("alias");
    symlink(&script, &alias).unwrap();
    assert!(inspect(&alias).is_err());
}

#[test]
fn stable_executable_identity_detects_replacement_and_content_edits() {
    let (_dir, path) = fixture(b"\x7fELFfixture", 0o755);
    let tool = Tool {
        stamp: inspect(&path).unwrap(),
        path,
    };
    tool.verify().unwrap();
    fs::write(&tool.path, b"\x7fELFchanged").unwrap();
    assert!(tool.verify().is_err());
    let current = inspect(&tool.path).unwrap();
    fs::rename(&tool.path, tool.path.with_file_name("old")).unwrap();
    fs::write(&tool.path, b"\x7fELFchanged").unwrap();
    fs::set_permissions(&tool.path, fs::Permissions::from_mode(0o755)).unwrap();
    assert_ne!(inspect(&tool.path).unwrap(), current);
}
