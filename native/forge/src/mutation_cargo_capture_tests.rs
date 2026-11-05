use super::*;
use serde_json::json;

fn fixture() -> (tempfile::TempDir, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::create_dir(dir.path().join("tests")).unwrap();
    for (path, text) in [
        ("Cargo.toml", "[package]\nname='demo'\nversion='0.1.0'\nedition='2021'\n[[test]]\nname='contract'\npath='tests/contract.rs'\n"),
        ("Cargo.lock", "version=4\n[[package]]\nname='demo'\nversion='0.1.0'\n"),
        ("src/lib.rs", "pub fn value() -> bool { true }"),
        ("tests/contract.rs", "#[test] fn check() { assert!(demo::value()); }"),
    ] { fs::write(dir.path().join(path), text).unwrap(); }
    let decl = json!({"schema_version":1,"target":{"manifest_path":"Cargo.toml","package":"demo","test":"contract"},"baseline_repetitions":2,
        "required_contracts":[{"id":"value","criticality":"high","active_paths":["src/lib.rs"]}],
        "ranked_tests":[{"rank":1,"nodeid":"tests/contract.rs::check","contract":"value","rationale":"value remains true"}]});
    fs::write(dir.path().join("contract.json"), decl.to_string()).unwrap();
    (dir, vec!["src/lib.rs".into()])
}

#[test]
fn held_inventory_detects_added_rust_input_but_allows_receipt_output() {
    let (dir, selected) = fixture();
    let held = Held::capture(dir.path(), "contract.json", &selected).unwrap();
    fs::write(dir.path().join("receipt.json"), "{}").unwrap();
    held.verify().unwrap();
    fs::write(dir.path().join("src/new.rs"), "fn new_input() {}").unwrap();
    assert!(held
        .verify()
        .unwrap_err()
        .to_string()
        .contains("inputs changed"));
}

#[test]
fn held_inventory_rejects_changed_declaration_and_same_byte_replacement() {
    let (dir, selected) = fixture();
    let held = Held::capture(dir.path(), "contract.json", &selected).unwrap();
    let source = dir.path().join("src/lib.rs");
    let bytes = fs::read(&source).unwrap();
    fs::remove_file(&source).unwrap();
    fs::write(source, bytes).unwrap();
    assert!(held.verify().is_err());
    let held = Held::capture(dir.path(), "contract.json", &selected).unwrap();
    fs::write(dir.path().join("contract.json"), "{}").unwrap();
    assert!(held.verify().is_err());
}

#[test]
fn held_inventory_refuses_symlink_and_new_cargo_configuration() {
    let (dir, selected) = fixture();
    let held = Held::capture(dir.path(), "contract.json", &selected).unwrap();
    fs::create_dir(dir.path().join(".cargo")).unwrap();
    assert!(held.verify().is_err());
    fs::remove_dir(dir.path().join(".cargo")).unwrap();
    std::os::unix::fs::symlink("lib.rs", dir.path().join("src/link.rs")).unwrap();
    assert!(Held::capture(dir.path(), "contract.json", &selected).is_err());
}

#[test]
fn generated_output_cannot_alias_a_held_declaration() {
    let (dir, selected) = fixture();
    let held = Held::capture(dir.path(), "contract.json", &selected).unwrap();
    let declaration = dir.path().join("contract.json");
    assert!(held.check_output(&declaration).is_err());
    let alias = dir.path().join("alias.json");
    let original = fs::read(&declaration).unwrap();
    fs::hard_link(&declaration, &alias).unwrap();
    assert!(held.verify().is_err());
    assert!(held.check_output(&alias).is_err());
    assert_eq!(fs::read(&declaration).unwrap(), original);
    fs::remove_file(alias).unwrap();
    let held = Held::capture(dir.path(), "contract.json", &selected).unwrap();
    held.check_output(&dir.path().join("unrelated.json"))
        .unwrap();
}

#[test]
fn planning_refuses_source_hashes_from_before_capture() {
    let (dir, _) = fixture();
    let bytes = fs::read(dir.path().join("contract.json")).unwrap();
    let mut plan = json!({"language":"rust","manifests":[{"mutation_engine":"cargo-mutants",
        "generator":{"jobs":1},"source_sha256":{"src/lib.rs":"stale-source-hash"}}]});
    let error = crate::mutation_cargo_plan::attach(
        &mut plan,
        dir.path(),
        "contract.json",
        &crate::mutation_campaign::sha256_hex(&bytes),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("planned source changed"));
    assert!(plan["manifests"][0].get("generated_value").is_none());
}

#[test]
fn capture_enforces_remaining_file_budget_during_capture_and_reverification() {
    let (dir, selected) = fixture();
    assert!(Held::capture_with_limit(dir.path(), "contract.json", &selected, 4).is_err());
    let held = Held::capture_with_limit(dir.path(), "contract.json", &selected, 5).unwrap();
    assert_eq!(held.file_count(), 5);
    fs::write(dir.path().join("src/added.rs"), "fn added() {}").unwrap();
    assert!(held
        .verify()
        .unwrap_err()
        .to_string()
        .contains("file budget"));
}
