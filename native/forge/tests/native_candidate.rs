#![cfg(target_os = "linux")]
use git2::{Repository, RepositoryInitOptions, Signature};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    root: PathBuf,
    repository: Repository,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(forge::resource_id("forge-candidate-cli").unwrap());
        std::fs::create_dir(&root).unwrap();
        let mut options = RepositoryInitOptions::new();
        options.initial_head("main");
        let repository = Repository::init_opts(root.join("repo"), &options).unwrap();
        Self { root, repository }
    }
    fn stage(&self, content: &str) -> String {
        std::fs::write(self.root.join("repo/file"), content).unwrap();
        let mut index = self.repository.index().unwrap();
        index.add_path(Path::new("file")).unwrap();
        index.write().unwrap();
        index.write_tree().unwrap().to_string()
    }
    fn commit(&self) -> String {
        let tree = self.repository.index().unwrap().write_tree().unwrap();
        let tree = self.repository.find_tree(tree).unwrap();
        let sig = Signature::now("Fixture", "fixture@example.invalid").unwrap();
        self.repository
            .commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
            .unwrap()
            .to_string()
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
        command
            .arg("candidate")
            .current_dir(self.root.join("repo"))
            .env("PATH", "")
            .env("HOME", &self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("CONDUCTOR_INTEGRATION_REF")
            .env_remove("CONDUCTOR_INTEGRATION_BRANCH");
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn receipt(&self, tree: &str) -> PathBuf {
        let mut value = json!({"receipt_id":"", "receipt_digest":"", "decision":"fail", "candidate":{"tree_oid":tree}});
        value.sort_all_objects();
        value["receipt_digest"] = json!(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&value).unwrap())
        ));
        value["receipt_id"] = json!("display-only");
        let file = self.root.join("receipt.json");
        std::fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
        file
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
fn value(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn inspect_resolves_staged_tree_with_no_external_programs() {
    let fixture = Fixture::new();
    fixture.stage("base");
    let commit = fixture.commit();
    let staged = fixture.stage("staged");
    std::fs::write(fixture.root.join("repo/file"), "dirty").unwrap();
    let candidate = value(fixture.run(&["inspect", "--kind", "index"]));
    assert_eq!(candidate["tree_oid"], staged);
    assert_eq!(candidate["base_commit_oid"], commit);
    assert_eq!(candidate["changes"][0]["path"], "file");
    assert!(candidate["changes"][0]["classes"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(candidate["changes"][0]["status"], "M");
    assert!(!fixture
        .run(&["inspect", "--kind", "range"])
        .status
        .success());
}

#[test]
fn dry_run_creates_nothing_and_materialize_publishes_exact_objects() {
    let fixture = Fixture::new();
    let tree = fixture.stage("object bytes\n");
    std::fs::write(fixture.root.join("repo/file"), "untracked content").unwrap();
    let output = fixture.root.join("snapshot");
    let args = [
        "materialize",
        "--tree",
        &tree,
        "--output",
        output.to_str().unwrap(),
    ];
    let preview = value(
        fixture
            .command()
            .args(args)
            .arg("--dry-run")
            .output()
            .unwrap(),
    );
    assert_eq!(preview["published"], false);
    assert!(!output.exists());
    assert_eq!(std::fs::read_dir(&fixture.root).unwrap().count(), 1);
    let report = value(fixture.run(&args));
    assert_eq!(report["published"], true);
    assert_eq!(
        std::fs::read(output.join("file")).unwrap(),
        b"object bytes\n"
    );
    assert!(!output.join(".git").exists());
    assert!(!fixture.run(&args).status.success());
    assert_eq!(
        std::fs::read(output.join("file")).unwrap(),
        b"object bytes\n"
    );
}

#[test]
fn byte_limit_failure_leaves_no_snapshot_or_staging_directory() {
    let fixture = Fixture::new();
    let tree = fixture.stage("12345");
    let output = fixture.root.join("snapshot");
    let failed = fixture.run(&[
        "materialize",
        "--tree",
        &tree,
        "--output",
        output.to_str().unwrap(),
        "--max-blob-bytes",
        "4",
    ]);
    assert_eq!(failed.status.code(), Some(1));
    assert_eq!(std::fs::read_dir(&fixture.root).unwrap().count(), 1);
}

#[test]
fn digest_valid_failure_verifies_but_tampering_and_mismatched_binding_fail() {
    let fixture = Fixture::new();
    let tree = fixture.stage("bound");
    fixture.commit();
    let receipt = fixture.receipt(&tree);
    let file = receipt.to_str().unwrap();
    let standalone = value(fixture.run(&["verify-receipt", file, "--json"]));
    assert_eq!(standalone["digest_valid"], true);
    assert_eq!(standalone["decision"], "fail");
    assert!(standalone["tree_binding"].is_null());
    let bound = value(fixture.run(&[
        "verify-receipt",
        file,
        "--repo",
        ".",
        "--ref",
        "HEAD",
        "--json",
    ]));
    assert_eq!(bound["tree_binding"]["tree_oid"], tree);
    let different_tree = fixture.stage("different");
    fixture.receipt(&different_tree);
    assert_eq!(
        fixture
            .run(&["verify-receipt", file, "--repo", ".", "--ref", "HEAD"])
            .status
            .code(),
        Some(1)
    );
    let mut tampered: Value = serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    tampered["decision"] = json!("pass");
    std::fs::write(&receipt, serde_json::to_vec(&tampered).unwrap()).unwrap();
    assert_eq!(
        fixture.run(&["verify-receipt", file]).status.code(),
        Some(1)
    );
}

#[test]
fn refuses_partial_tree_binding_and_unsafe_receipt_inputs() {
    let fixture = Fixture::new();
    let receipt = fixture.receipt(&"0".repeat(40));
    let file = receipt.to_str().unwrap();
    assert_eq!(
        fixture
            .run(&["verify-receipt", file, "--repo", "."])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        fixture
            .run(&["verify-receipt", file, "--ref", "HEAD"])
            .status
            .code(),
        Some(2)
    );
    let link = fixture.root.join("link.json");
    std::os::unix::fs::symlink(&receipt, &link).unwrap();
    assert_eq!(
        fixture
            .run(&["verify-receipt", link.to_str().unwrap()])
            .status
            .code(),
        Some(1)
    );
    std::fs::write(&receipt, b"{\"decision\":1,\"decision\":2}").unwrap();
    assert_eq!(
        fixture.run(&["verify-receipt", file]).status.code(),
        Some(1)
    );
}

#[test]
fn independent_cpython_receipt_fixture_verifies_without_interpreter() {
    let fixture = Fixture::new();
    let reference: Value =
        serde_json::from_slice(include_bytes!("fixtures/candidate_receipt/reference.json"))
            .unwrap();
    let file = fixture.root.join("oracle.json");
    std::fs::write(
        &file,
        serde_json::to_vec(&reference["digest_valid_failed_receipt"]).unwrap(),
    )
    .unwrap();
    let result = value(fixture.run(&["verify-receipt", file.to_str().unwrap(), "--json"]));
    assert_eq!(result["decision"], "fail");
    assert_eq!(result["digest_valid"], true);
}

#[test]
fn alternate_index_is_resolved_relative_to_repository_and_neither_index_is_changed() {
    let fixture = Fixture::new();
    let alternate_tree = fixture.stage("alternate candidate");
    let alternate = fixture.root.join("repo/alternate.index");
    let default = fixture.root.join("repo/.git/index");
    std::fs::copy(&default, &alternate).unwrap();
    fixture.stage("default candidate");
    let default_bytes = std::fs::read(&default).unwrap();
    let alternate_bytes = std::fs::read(&alternate).unwrap();
    let candidate = value(
        fixture
            .command()
            .args(["inspect", "--kind", "index"])
            .env("GIT_INDEX_FILE", "alternate.index")
            .output()
            .unwrap(),
    );
    assert_eq!(candidate["tree_oid"], alternate_tree);
    assert_eq!(std::fs::read(default).unwrap(), default_bytes);
    assert_eq!(std::fs::read(alternate).unwrap(), alternate_bytes);
}
