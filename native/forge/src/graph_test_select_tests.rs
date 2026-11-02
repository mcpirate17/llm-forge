use super::*;
use git2::{Repository, Signature};
use rusqlite::Connection;
use std::fs;

fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let repository = Repository::init(root).unwrap();
    fs::write(root.join("base.py"), "VALUE = 1\n").unwrap();
    let mut index = repository.index().unwrap();
    index.add_path(Path::new("base.py")).unwrap();
    index.write().unwrap();
    let oid = index.write_tree().unwrap();
    let tree = repository.find_tree(oid).unwrap();
    let signature = Signature::now("Graph tests", "graph@example.invalid").unwrap();
    repository
        .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .unwrap();
    directory
}

fn write(root: &Path, path: &str, content: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn graph(root: &Path) {
    fs::create_dir(root.join(".code-review-graph")).unwrap();
    Connection::open(root.join(".code-review-graph/graph.db"))
        .unwrap()
        .execute_batch(
            "CREATE TABLE nodes (qualified_name TEXT, file_path TEXT, is_test INTEGER);
         CREATE TABLE edges (source_qualified TEXT, target_qualified TEXT);",
        )
        .unwrap();
}

#[test]
fn git_discovery_keeps_delete_add_staged_unstaged_and_literal_names() {
    let directory = fixture();
    let root = directory.path().canonicalize().unwrap();
    fs::rename(root.join("base.py"), root.join("renamed.py")).unwrap();
    let repository = Repository::open(&root).unwrap();
    let mut index = repository.index().unwrap();
    index.remove_path(Path::new("base.py")).unwrap();
    index.add_path(Path::new("renamed.py")).unwrap();
    index.write().unwrap();
    write(&root, "untracked space\nfile.py", "x = 1\n");
    write(&root, "renamed.py", "VALUE = 2\n");
    assert_eq!(
        changed_files(&root).unwrap(),
        ["base.py", "renamed.py", "untracked space\nfile.py"]
    );
    assert_eq!(repository_root(&root.join(".git")).unwrap(), root);
}

#[test]
fn discovery_fails_for_unborn_head_and_enforces_work_limit() {
    let directory = tempfile::tempdir().unwrap();
    Repository::init(directory.path()).unwrap();
    assert!(changed_files(directory.path())
        .unwrap_err()
        .to_string()
        .contains("HEAD"));
    let directory = fixture();
    for index in 0..1025 {
        write(directory.path(), &format!("file_{index}.py"), "");
    }
    assert!(changed_files(directory.path())
        .unwrap_err()
        .to_string()
        .contains("1024"));
}

#[test]
fn missing_and_corrupt_graph_fail_visibly_without_convention_only_success() {
    let directory = fixture();
    let root = directory.path().canonicalize().unwrap();
    write(&root, "tests/test_base.py", "def test_base(): pass\n");
    assert!(select(&root, &["base.py".into()]).is_err());
    fs::create_dir(root.join(".forge")).unwrap();
    fs::write(root.join(".forge/graph.db"), "broken").unwrap();
    assert!(select(&root, &["base.py".into()]).is_err());
}

#[test]
fn incomplete_graph_preserves_all_languages_and_fallback_metadata() {
    let directory = fixture();
    let root = directory.path().canonicalize().unwrap();
    graph(&root);
    write(&root, "tests/test_base.py", "def test_base(): pass\n");
    write(&root, "tests/test_other.py", "def test_other(): pass\n");
    write(&root, "web/unrelated.spec.ts", "test('x', () => {});\n");
    let result = select(&root, &["base.py".into()]).unwrap();
    assert!(!result.complete);
    assert_eq!(result.scope, "full-test-inventory-fallback");
    assert!(!result.fallback_reasons.is_empty());
    assert_eq!(
        result.selected_tests,
        ["tests/test_base.py", "tests/test_other.py"]
    );
    assert_eq!(result.unsupported_tests, ["web/unrelated.spec.ts"]);
    assert_eq!(result.count, 3);
    let json = serde_json::to_value(result).unwrap();
    assert_eq!(json["selected_paths"].as_array().unwrap().len(), 3);
}

#[test]
fn no_changes_needs_no_graph_and_path_escape_is_rejected() {
    let directory = fixture();
    let root = directory.path().canonicalize().unwrap();
    let plan = select(&root, &[]).unwrap();
    assert!(plan.complete);
    assert_eq!(plan.scope, "no-source-changes");
    assert_eq!(plan.count, 0);
    assert!(select(&root, &["../outside.py".into()]).is_err());
    assert!(select(&root, &["/outside.py".into()]).is_err());
}

#[test]
fn conservative_registry_is_runtime_owned_and_selects_all_on_fallback() {
    let directory = fixture();
    let root = directory.path().canonicalize().unwrap();
    write(
        &root,
        "native/conductor-native/src/python_contract_targets.tsv",
        "src/a.py\tpython_contracts_a\nsrc/b.py\tpython_contracts_b\n",
    );
    write(
        &root,
        "native/conductor-native/tests/python_contracts_a.rs",
        "#[test] fn a() {}\n",
    );
    write(
        &root,
        "native/conductor-native/tests/python_contracts_b.rs",
        "#[test] fn b() {}\n",
    );
    let narrow = contracts::plan(&root, &["src/a.py".into()], true).unwrap();
    assert_eq!(narrow.targets, ["python_contracts_a"]);
    let fallback = contracts::plan(&root, &["src/a.py".into()], false).unwrap();
    assert_eq!(
        fallback.targets,
        ["python_contracts_a", "python_contracts_b"]
    );
    let native = contracts::plan(&root, &["native/forge/src/thing.rs".into()], true).unwrap();
    assert_eq!(native.targets, fallback.targets);
    fs::remove_file(root.join("native/conductor-native/tests/python_contracts_b.rs")).unwrap();
    assert!(contracts::plan(&root, &["src/a.py".into()], false).is_err());
}

#[test]
fn rust_inventory_is_executable_as_a_broader_crate_suite() {
    let directory = fixture();
    let root = directory.path().canonicalize().unwrap();
    graph(&root);
    write(
        &root,
        "core/Cargo.toml",
        "[package]\nname='core-test'\nversion='0.1.0'\n",
    );
    write(
        &root,
        "core/src/lib.rs",
        "#[cfg(test)] mod tests { #[test] fn passes() {} }\n",
    );
    let plan = select(&root, &["base.py".into()]).unwrap();
    assert_eq!(plan.native_manifests, [root.join("core/Cargo.toml")]);
    assert!(plan.unsupported_tests.is_empty());
}

#[test]
fn execution_refuses_unsupported_languages_and_missing_explicit_guest() {
    let directory = fixture();
    let root = directory.path().canonicalize().unwrap();
    graph(&root);
    write(&root, "tests/test_base.py", "def test_base(): pass\n");
    let mut plan = select(&root, &["base.py".into()]).unwrap();
    let args = SelectArgs {
        paths: vec![],
        repo: root.clone(),
        run: true,
        json: false,
        python: None,
        runtime_dir: None,
        pytest_args: vec![],
        timeout_seconds: 1,
    };
    assert!(execution::preflight(&root, &plan, &args)
        .unwrap_err()
        .to_string()
        .contains("--python"));
    plan.unsupported_tests.push("web/suite.spec.ts".into());
    assert!(execution::preflight(&root, &plan, &args)
        .unwrap_err()
        .to_string()
        .contains("lack a runner"));
}
