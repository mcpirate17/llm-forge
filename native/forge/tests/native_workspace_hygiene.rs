//! Full native report controls. No interpreter, network service or cleanup actor.
#[path = "support/forge_binary.rs"]
mod forge_binary;
use git2::{Repository, RepositoryInitOptions, Signature};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};
use tempfile::TempDir;
struct Case {
    root: TempDir,
}
impl Case {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut options = RepositoryInitOptions::new();
        options.initial_head("main");
        let repo = Repository::init_opts(root.path(), &options).unwrap();
        fs::create_dir(root.path().join("campaigns")).unwrap();
        fs::write(root.path().join("forge.toml"),"schema_version=1\n[conductor]\nintegration_branch='main'\nmutation_registry='campaigns/registry.json'\n").unwrap();
        fs::write(
            root.path().join("campaigns/registry.json"),
            "{\"campaigns\":[]}",
        )
        .unwrap();
        fs::write(root.path().join("source.py"), "VALUE=1\n").unwrap();
        let mut index = repo.index().unwrap();
        for path in ["forge.toml", "campaigns/registry.json", "source.py"] {
            index.add_path(Path::new(path)).unwrap();
        }
        let tree_id = index.write_tree().unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = Signature::now("fixture", "fixture@example.invalid").unwrap();
        let id = repo
            .commit(Some("HEAD"), &signature, &signature, "baseline", &tree, &[])
            .unwrap();
        repo.reference("refs/remotes/origin/main", id, false, "fixture")
            .unwrap();
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
            false,
            "fixture",
        )
        .unwrap();
        Self { root }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(forge_binary::path());
        command
            .args(["workspace-hygiene", "--repo"])
            .arg(self.root.path())
            .args(["--live-ref", "main"])
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("CONDUCTOR_MUTATION_REGISTRY")
            .env_remove("CONDUCTOR_INTEGRATION_BRANCH")
            .env("PYTHONPATH", "/must-not-import-python");
        command
    }
    fn index(&self) -> Vec<u8> {
        fs::read(self.root.path().join(".git/index")).unwrap()
    }
}
fn decoded(output: Output, status: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(status),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
#[test]
fn full_report_retains_all_rows_and_does_not_modify_source_or_index() {
    let case = Case::new();
    let index = case.index();
    let value = decoded(case.command().arg("--json").output().unwrap(), 0);
    for field in [
        "live_ref",
        "redundant_branches",
        "stale_worktrees",
        "dirty_worktrees",
        "landed_worktrees",
        "landed_ref",
        "expired_claims",
        "idle_claims",
        "unpushed_work",
        "orphan_manifests",
        "dangling_manifests",
        "untracked_imports",
        "stale_feature_branches",
        "gh_available",
        "local_only_commit_exposure",
        "stale_dirty_files",
        "worktree_leases",
        "checkout_drift",
    ] {
        assert!(value.get(field).is_some(), "missing {field}: {value}");
    }
    assert_eq!(value["live_ref"], "main");
    assert_eq!(value["landed_ref"], "origin/main");
    assert_eq!(value["checkout_drift"]["behind"], 0);
    assert_eq!(value["expired_claims"], json!([]));
    assert_eq!(value["untracked_imports"], json!([]));
    assert_eq!(case.index(), index);
    assert_eq!(
        fs::read_to_string(case.root.path().join("source.py")).unwrap(),
        "VALUE=1\n"
    );
}
#[test]
fn native_claim_rows_cannot_be_hidden_by_forged_forge_binary_environment() {
    let case = Case::new();
    let fields = json!({"owner":"expired-fixture","paths":["source.py"],"justification":"report control",
        "created_at":"2020-01-01T00:00:00+00:00","expires_at":"2020-01-01T01:00:00+00:00"});
    let sha = forge::ownership::sha256_json(&fields);
    let mut claim = fields;
    claim["claim_id"] = json!(format!("claim-{}", &sha[..20]));
    let governance = case.root.path().join(".git/governance");
    fs::create_dir(&governance).unwrap();
    fs::set_permissions(&governance, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        governance.join("ownership-claims.json"),
        json!({"schema_version":1,"claims":[claim]}).to_string(),
    )
    .unwrap();
    let operator = tempfile::tempdir().unwrap();
    let marker = operator.path().join("ran");
    let forged = operator.path().join("forge");
    fs::write(
        &forged,
        format!(
            "#!/bin/sh\nprintf invoked > '{}'\nprintf '{{\"claims\":[]}}'\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&forged, fs::Permissions::from_mode(0o755)).unwrap();
    let value = decoded(
        case.command()
            .arg("--json")
            .env("FORGE_BIN", &forged)
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(value["expired_claims"][0]["owner"], "expired-fixture");
    assert!(
        !marker.exists(),
        "native report must not delegate claim authority"
    );
}
#[test]
fn import_findings_remain_distinct_from_the_legacy_exposure_exit_flag() {
    let case = Case::new();
    // The import resolver uses tracked packages as first-party roots.
    fs::create_dir(case.root.path().join("pkg")).unwrap();
    fs::write(case.root.path().join("pkg/__init__.py"), "").unwrap();
    let repo = Repository::open(case.root.path()).unwrap();
    let mut tracked = repo.index().unwrap();
    tracked.add_path(Path::new("pkg/__init__.py")).unwrap();
    tracked.write().unwrap();
    fs::write(
        case.root.path().join("source.py"),
        "from pkg import untracked_helper\n",
    )
    .unwrap();
    fs::write(
        case.root.path().join("pkg/untracked_helper.py"),
        "VALUE=1\n",
    )
    .unwrap();
    let index = case.index();
    let value = decoded(
        case.command()
            .args(["--json", "--exit-nonzero-on-exposure"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(
        value["untracked_imports"][0]["module"],
        "pkg/untracked_helper.py"
    );
    assert_eq!(case.index(), index);
}
#[test]
fn malformed_registry_and_source_parse_errors_refuse_instead_of_partial_clean_reports() {
    let case = Case::new();
    fs::write(case.root.path().join("campaigns/registry.json"), "not JSON").unwrap();
    let output = case.command().arg("--json").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("JSON"));
    fs::write(
        case.root.path().join("campaigns/registry.json"),
        "{\"campaigns\":[]}",
    )
    .unwrap();
    fs::write(case.root.path().join("source.py"), "def broken(:\n").unwrap();
    let output = case.command().arg("--json").output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("pars"));
}

#[test]
fn read_only_git_probes_do_not_activate_external_diff_or_fsmonitor_programs() {
    let case = Case::new();
    let operator = tempfile::tempdir().unwrap();
    let marker = operator.path().join("invoked");
    let helper = operator.path().join("repo-helper");
    fs::write(
        &helper,
        "#!/bin/sh\nprintf invoked > \"$FORGE_TEST_HELPER_MARKER\"\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    let repo = Repository::open(case.root.path()).unwrap();
    repo.config()
        .unwrap()
        .set_str("diff.external", helper.to_str().unwrap())
        .unwrap();
    repo.config()
        .unwrap()
        .set_str("core.fsmonitor", helper.to_str().unwrap())
        .unwrap();
    fs::write(case.root.path().join("source.py"), "VALUE=2\n").unwrap();
    let before = case.index();
    let value = decoded(
        case.command()
            .arg("--json")
            .env("GIT_EXTERNAL_DIFF", &helper)
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.fsmonitor")
            .env("GIT_CONFIG_VALUE_0", &helper)
            .env("FORGE_TEST_HELPER_MARKER", &marker)
            .output()
            .unwrap(),
        0,
    );
    assert!(!value["dirty_worktrees"].as_array().unwrap().is_empty());
    assert!(!marker.exists());
    assert_eq!(case.index(), before);
}
