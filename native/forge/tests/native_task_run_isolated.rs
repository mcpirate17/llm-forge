#![cfg(target_os = "linux")]
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_execution_fixture::{value, Case};
use git2::Repository;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
    time::Duration,
};

fn tree(case: &Case) -> String {
    Repository::open(case.fixture.root.join("repo"))
        .unwrap()
        .index()
        .unwrap()
        .write_tree()
        .unwrap()
        .to_string()
}
fn command(case: &Case, tree: &str, args: &[&str]) -> Command {
    // The shared fixture writes the exact operator grants with private mode.
    drop(case.command());
    let mut command = Command::new(support::forge_binary::path());
    command
        .current_dir(case.fixture.root.join("repo"))
        .args(["task", "run-isolated", "--repo"])
        .arg(case.fixture.root.join("repo"))
        .args(["--tree", tree, "--grants"])
        .arg(case.fixture.root.join("grants.json"))
        .env("PATH", "")
        .env("HOME", &case.fixture.root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args);
    command
}
fn bounded(command: &mut Command) -> Output {
    forge::process_probe::run(command, Duration::from_secs(20), 1024 * 1024).unwrap()
}
fn prepare(program: &Path) -> Case {
    let case = Case::new(program);
    fs::set_permissions(
        case.fixture.root.join("scratch"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    case
}
#[test]
fn preview_does_not_run_or_mutate_journal_and_requires_explicit_inputs() {
    let case = prepare(Path::new("/usr/bin/true"));
    let tree = tree(&case);
    let before = case.source_state();
    let output = bounded(&mut command(
        &case,
        &tree,
        &["--artifact", "out.bin", "--", "worker"],
    ));
    let result = value(output, 0);
    assert_eq!(result["executed"], false);
    assert_eq!(result["preimage_tree"], tree);
    assert_eq!(result["disposition"], "preflight_only");
    assert!(!case.fixture.root.join("repo/.agents").exists());
    case.assert_preserved(&before);
    let bad = bounded(&mut command(&case, "1234", &["--", "worker"]));
    assert!(!bad.status.success());
    let bad = bounded(&mut command(
        &case,
        &tree,
        &["--artifact", "../escape", "--", "worker"],
    ));
    assert!(!bad.status.success());
    let bad = bounded(&mut command(&case, &tree, &["--", "ungranted"]));
    assert!(!bad.status.success());
    let mut missing = Command::new(support::forge_binary::path());
    missing
        .args(["task", "run-isolated", "--repo"])
        .arg(case.fixture.root.join("repo"))
        .args(["--tree", &tree, "--", "worker"]);
    assert!(!bounded(&mut missing).status.success());
    case.assert_preserved(&before);
}
#[test]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
fn actual_success_retains_selected_binary_artifact_and_receipt() {
    let mut case = prepare(Path::new("/usr/bin/cp"));
    case.delegated();
    let bytes = "before\0binary\n";
    case.fixture.stage("input.bin", bytes);
    let tree = tree(&case);
    let before = case.source_state();
    let result = value(
        bounded(&mut command(
            &case,
            &tree,
            &[
                "--artifact",
                "out.bin",
                "--execute",
                "--",
                "worker",
                "input.bin",
                "out.bin",
            ],
        )),
        0,
    );
    assert_eq!(result["report"]["outcome"], "exited");
    assert_eq!(result["report"]["exit_code"], 0);
    assert_eq!(result["report"]["cleanup_complete"], true);
    let receipt = &result["disposition"];
    assert_eq!(receipt["status"], "retained_for_review");
    assert_eq!(receipt["preimage_tree"], tree);
    assert_eq!(receipt["artifacts"].as_array().unwrap().len(), 1);
    assert_eq!(receipt["artifacts"][0]["path"], "out.bin");
    assert_eq!(receipt["artifacts"][0]["bytes"], bytes.len());
    assert_eq!(
        receipt["artifacts"][0]["sha256"],
        format!("{:x}", Sha256::digest(bytes.as_bytes()))
    );
    let retained = Path::new(receipt["workspace"].as_str().unwrap());
    assert!(retained.starts_with(case.fixture.root.join("scratch")));
    assert_eq!(
        fs::read(retained.join("out.bin")).unwrap(),
        bytes.as_bytes()
    );
    let persisted: Value =
        serde_json::from_slice(&fs::read(result["receipt_path"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(&persisted, receipt);
    assert_eq!(result["source_published"], false);
    assert!(!case.fixture.root.join("repo/out.bin").exists());
    assert!(!case.fixture.root.join("repo/.agents").exists());
    assert_eq!(case.source_state(), before);
}

#[test]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
fn actual_timeout_discards_snapshot_and_preserves_host_and_index() {
    let mut case = prepare(support::task_worker());
    case.delegated();
    case.grants["max_wall_seconds"] = 1.into();
    case.fixture.stage("staged.txt", "staged bytes");
    case.fixture.stage("dirty.txt", "indexed bytes");
    let tree = tree(&case);
    let staged = case.fixture.root.join("repo/staged.txt");
    let dirty = case.fixture.root.join("repo/dirty.txt");
    let sentinel = case.fixture.root.join("repo/user.txt");
    fs::write(&dirty, b"unstaged bytes").unwrap();
    fs::write(&sentinel, b"untouched user bytes").unwrap();
    let before = case.source_state();
    let result: Value = value(
        bounded(&mut command(
            &case,
            &tree,
            &["--execute", "--", "worker", "pid-sleep", "20000", "pid.txt"],
        )),
        1,
    );
    assert_eq!(result["report"]["outcome"], "timed_out");
    assert_eq!(result["report"]["cleanup_complete"], true);
    assert_eq!(result["disposition"]["status"], "rolled_back_by_discard");
    assert_eq!(result["source_published"], false);
    assert!(Path::new(result["receipt_path"].as_str().unwrap()).is_file());
    assert_eq!(fs::read(sentinel).unwrap(), b"untouched user bytes");
    assert_eq!(fs::read(staged).unwrap(), b"staged bytes");
    assert_eq!(fs::read(dirty).unwrap(), b"unstaged bytes");
    assert_eq!(case.source_state(), before);
}
