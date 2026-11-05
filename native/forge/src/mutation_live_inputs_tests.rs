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

#[test]
fn native_inventory_matches_shared_fingerprint_and_detects_new_files() {
    use conductor_native::source_fingerprint::{self, EXTRA_FILES, SOURCE_TREES};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |path: &str| {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, b"source").unwrap();
    };
    for name in SOURCE_TREES {
        for suffix in ["Cargo.toml", "Cargo.lock", "src/lib.rs"] {
            write(&format!("native/{name}/{suffix}"));
        }
    }
    for path in EXTRA_FILES {
        write(path);
    }
    let files = source_fingerprint::files(root).unwrap();
    let expected = source_fingerprint::fingerprint(root, &files).unwrap();
    let mut held = LiveInputs::open(root).unwrap();
    assert_eq!(held.native_source_fingerprint().unwrap(), Some(expected));
    held.verify().unwrap();
    write("native/forge/src/new_module.rs");
    assert!(held.verify().is_err());
}

#[test]
fn installed_fallback_requires_held_absence_of_all_four_roots() {
    for name in ["forge", "forge-cli", "conductor-native", "slop-core"] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src/conductor")).unwrap();
        fs::write(root.join("src/conductor/memory_sources.toml"), b"host").unwrap();
        let mut held = LiveInputs::open(root).unwrap();
        assert!(held.native_inventory_absent().unwrap());
        held.verify().unwrap();
        fs::create_dir_all(root.join("native").join(name)).unwrap();
        assert!(held.verify().is_err());
        let mut partial = LiveInputs::open(root).unwrap();
        assert!(!partial.native_inventory_absent().unwrap());
        assert_eq!(partial.native_source_fingerprint().unwrap(), None);
    }
}

#[test]
fn symlinked_root_is_not_absent() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("native")).unwrap();
    symlink(dir.path(), dir.path().join("native/forge")).unwrap();
    let mut held = LiveInputs::open(dir.path()).unwrap();
    assert!(!held.native_inventory_absent().unwrap());
    assert!(held.native_source_fingerprint().unwrap().is_none());
}

#[test]
fn live_cargo_inventory_retains_files_and_detects_new_source() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |path: &str, text: &str| {
        let full = root.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, text).unwrap();
    };
    write("Cargo.toml", "[package]\nname='demo'\nversion='0.1.0'\n[[test]]\nname='contract'\npath='tests/contract.rs'\n");
    write(
        "Cargo.lock",
        "version=4\n[[package]]\nname='demo'\nversion='0.1.0'\n",
    );
    write("src/lib.rs", "pub mod a;");
    write("src/a.rs", "pub fn value() -> u8 { 1 }");
    write(
        "tests/contract.rs",
        "#[test] fn check() { assert_eq!(demo::a::value(), 1); }",
    );
    write("declaration.json", &serde_json::json!({"schema_version":1,"target":{"manifest_path":"Cargo.toml","package":"demo","test":"contract"},"baseline_repetitions":2,"required_contracts":[{"id":"c","criticality":"high","active_paths":["src/a.rs"]}],"ranked_tests":[{"rank":1,"nodeid":"tests/contract.rs::check","contract":"c","rationale":"behavior"}]}).to_string());
    let mut capture = LiveInputs::open(root).unwrap();
    let mut files = BTreeMap::new();
    let held = capture
        .cargo_inventory("declaration.json", &["src/a.rs".into()], &mut files)
        .unwrap();
    assert!(files.contains_key("tests/contract.rs"));
    capture.retain_cargo(held);
    for _ in 1..8 {
        let held = capture
            .cargo_inventory("declaration.json", &["src/a.rs".into()], &mut files)
            .unwrap();
        capture.retain_cargo(held);
    }
    assert!(capture
        .cargo_inventory("declaration.json", &["src/a.rs".into()], &mut files)
        .is_err());
    capture.verify().unwrap();
    write("src/new.rs", "fn new() {}");
    assert!(capture.verify().is_err());
}
