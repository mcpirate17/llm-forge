use super::*;
use std::{fs, os::unix::fs::symlink};
#[test]
fn reads_ignored_and_empty_files_and_detects_bytes_and_absence_drift() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(root.join(".gitignore"), "ignored.json\n").unwrap();
    fs::write(root.join("ignored.json"), b"").unwrap();
    let mut held = LiveInputs::open(&root).unwrap();
    assert_eq!(held.read("ignored.json").unwrap(), Some(&b""[..]));
    assert_eq!(held.read("missing.json").unwrap(), None);
    held.verify().unwrap();
    fs::write(root.join("missing.json"), b"new").unwrap();
    assert!(held.verify().is_err());
    fs::remove_file(root.join("missing.json")).unwrap();
    fs::write(root.join("ignored.json"), b"changed").unwrap();
    assert!(held.verify().is_err());
}
#[test]
fn detects_inventory_and_named_root_replacement() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("receipts")).unwrap();
    fs::write(root.join("receipts/a.json"), b"{}").unwrap();
    let mut held = LiveInputs::open(&root).unwrap();
    assert_eq!(
        held.json_files("receipts").unwrap(),
        vec!["receipts/a.json"]
    );
    fs::write(root.join("receipts/b.json"), b"{}").unwrap();
    assert!(held.verify().is_err());
    fs::remove_file(root.join("receipts/b.json")).unwrap();
    fs::rename(&root, tmp.path().join("old")).unwrap();
    fs::create_dir(&root).unwrap();
    assert!(held.verify().is_err());
}
#[test]
fn refuses_symlink_files_and_ancestor_escapes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(tmp.path().join("outside"), b"private").unwrap();
    symlink(tmp.path().join("outside"), root.join("leak.json")).unwrap();
    symlink(tmp.path(), root.join("link")).unwrap();
    let mut held = LiveInputs::open(&root).unwrap();
    assert!(held.read("leak.json").is_err());
    assert!(held.read("link/outside").is_err());
}
