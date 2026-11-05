use super::*;
use serde_json::json;
fn file(s: &str) -> Entry {
    Entry::Regular(Some(s.as_bytes().to_vec()))
}
fn fixture() -> (Vec<u8>, Vec<String>, BTreeMap<String, Entry>) {
    let declaration = json!({"schema_version":1,"target":{"manifest_path":"Cargo.toml","package":"demo","test":"contract"},"baseline_repetitions":2,
    "required_contracts":[{"id":"c","criticality":"high","active_paths":["src/a.rs","src/b.rs"]}],
    "ranked_tests":[{"rank":1,"nodeid":"tests/contract.rs::check","contract":"c","rationale":"covers behavior"}]}).to_string().into_bytes();
    let entries = BTreeMap::from([
        ("Cargo.toml".into(), file("[package]\nname='demo'\nversion='0.1.0'\nedition='2021'\n[[test]]\nname='contract'\npath='tests/contract.rs'\n")),
        ("Cargo.lock".into(), file("version = 4\n[[package]]\nname='demo'\nversion='0.1.0'\n")),
        ("src".into(), Entry::Directory), ("tests".into(), Entry::Directory),
        ("src/lib.rs".into(), file("pub mod a; pub mod b;")),
        ("src/a.rs".into(), file("pub fn value() -> u8 { 1 }")),
        ("src/b.rs".into(), file("pub fn value() -> u8 { 2 }")),
        ("tests/contract.rs".into(), file("#[test] fn check() { assert_eq!(demo::a::value(), 1); }")),
    ]);
    (
        declaration,
        vec!["src/a.rs".into(), "src/b.rs".into()],
        entries,
    )
}
#[test]
fn accepts_and_binds_relevant_files_but_ignores_output() {
    let (decl, selected, mut entries) = fixture();
    let original = build(&decl, &selected, &entries).unwrap();
    assert_eq!(original.target_path(), "tests/contract.rs");
    entries.insert("notes.json".into(), file("different"));
    assert_eq!(original, build(&decl, &selected, &entries).unwrap());
    entries.insert("src/new.rs".into(), file("fn added() {}"));
    assert_ne!(
        original.digest(),
        build(&decl, &selected, &entries).unwrap().digest()
    );
}
#[test]
fn refuses_forbidden_metadata_and_incomplete_or_symlink_inventory() {
    for (path, entry) in [
        ("build.rs", file("")),
        (".cargo", Entry::Directory),
        ("src/Cargo.toml", file("")),
        ("src/link", Entry::SymlinkOrSpecial),
    ] {
        let (decl, selected, mut entries) = fixture();
        entries.insert(path.into(), entry);
        assert!(build(&decl, &selected, &entries).is_err(), "{path}");
    }
    let (decl, selected, mut entries) = fixture();
    entries.remove("src/a.rs");
    assert!(build(&decl, &selected, &entries).is_err());
    let (decl, selected, mut entries) = fixture();
    entries.insert("src/a.rs".into(), Entry::Regular(None));
    assert!(build(&decl, &selected, &entries).is_err());
}
#[test]
fn refuses_deps_targets_and_ambiguous_test_discovery() {
    let (decl, selected, mut entries) = fixture();
    entries.insert("Cargo.toml".into(), file("[package]\nname='demo'\nversion='0.1.0'\n[dependencies]\nother='1'\n[[test]]\nname='contract'\npath='tests/contract.rs'"));
    assert!(build(&decl, &selected, &entries).is_err());
    for source in [
        "#[cfg(test)] #[test] fn check() {}",
        "#[test] fn check() { include!(\"bad.rs\"); }",
        "#[test] fn check() { assert!({ #[path=\"../../outside.rs\"] mod x; x::value() }); }",
        "#[test] fn check() {} #[test] fn extra() {}",
        "mod nested; #[test] fn check() {}",
    ] {
        let (decl, selected, mut entries) = fixture();
        entries.insert("tests/contract.rs".into(), file(source));
        assert!(build(&decl, &selected, &entries).is_err(), "{source}");
    }
}
#[test]
fn refuses_ambiguous_modules_and_unbound_target() {
    let (decl, selected, mut entries) = fixture();
    entries.insert("src/a".into(), Entry::Directory);
    entries.insert("src/a/mod.rs".into(), file(""));
    assert!(build(&decl, &selected, &entries).is_err());
    let (decl, selected, mut entries) = fixture();
    entries.remove("tests/contract.rs");
    assert!(build(&decl, &selected, &entries).is_err());
}

#[test]
fn refuses_test_or_unowned_files_as_selected_production_sources() {
    for source in ["tests/contract.rs", "src/unowned.rs"] {
        let (decl, _, mut entries) = fixture();
        entries.insert("src/unowned.rs".into(), file("pub fn unused() {}"));
        let mut declaration: serde_json::Value = serde_json::from_slice(&decl).unwrap();
        declaration["required_contracts"][0]["active_paths"] = json!([source]);
        let err = build(
            &serde_json::to_vec(&declaration).unwrap(),
            &[source.into()],
            &entries,
        )
        .unwrap_err();
        assert!(err.contains("library target"), "{err}");
    }
}
