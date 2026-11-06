#![cfg(target_os = "linux")]
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_execution_fixture::{value, Case};
use serde_json::json;
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};
fn fixture() -> Case {
    let mut case = Case::new(Path::new("/bin/true"));
    case.grants["resources"] = json!({"cpu_millis":1000,"memory_mib":128,"processes":16});
    case.grants["batch"] =
        json!({"max_workers":2,"cpu_millis":2000,"memory_mib":256,"processes":32});
    case
}
fn capture_engine(case: &Case) {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repository_root = case.fixture.root.join("repo");
    for path in forge::source_fingerprint::files(&root).unwrap() {
        let dst = repository_root.join(&path);
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::write(dst, std::fs::read(root.join(&path)).unwrap()).unwrap();
        case.fixture.add(path.to_str().unwrap());
    }
    case.staged_policy(|p| p.replace("[checks.zeta]", "[checks.zeta]\nalways=true"));
    let repo = git2::Repository::open(&repository_root).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let tree = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree).unwrap();
    let sig = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        "captured engine",
        &tree,
        &[&parent],
    )
    .unwrap();
    case.fixture.stage("readme.md", "documentation candidate\n");
}
#[test]
fn attest_requires_execution_before_any_message_change() {
    let case = fixture();
    let message = case.fixture.root.join("message");
    std::fs::write(&message, b"subject\n").unwrap();
    std::fs::set_permissions(&message, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut cmd = case.command_for("attest-message", "index", &[]);
    cmd.arg(&message).args(["--surface", "pre-commit"]);
    let output = forge::process_probe::run(&mut cmd, Duration::from_secs(10), 1024 * 1024).unwrap();
    assert!(!output.status.success());
    assert_eq!(std::fs::read(message).unwrap(), b"subject\n");
}
#[test]
fn token_only_and_closed_descriptor_fail_before_review() {
    for (fd, token, expected) in [
        (None, Some("a".repeat(64)), "token-only"),
        (
            Some("999999999".to_string()),
            Some("a".repeat(64)),
            "duplicate inherited",
        ),
    ] {
        let case = fixture();
        let message = case.fixture.root.join("message");
        std::fs::write(&message, b"subject\n").unwrap();
        std::fs::set_permissions(&message, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut cmd = case.command_for("attest-message", "index", &[]);
        cmd.arg(&message)
            .args(["--surface", "pre-commit", "--execute"]);
        match fd {
            Some(fd) => {
                cmd.env("LLM_GOVERNANCE_COMMIT_LOCK_FD", fd);
            }
            None => {
                cmd.env_remove("LLM_GOVERNANCE_COMMIT_LOCK_FD");
            }
        }
        cmd.env("LLM_GOVERNANCE_COMMIT_LOCK_TOKEN", token.unwrap());
        let output =
            forge::process_probe::run(&mut cmd, Duration::from_secs(10), 1024 * 1024).unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read(message).unwrap(), b"subject\n");
    }
}
fn inherited_lock(case: &Case) -> std::fs::File {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    let dir = case.fixture.root.join("repo/.git/governance");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join("commit-review.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    file
}
fn pass_lock(command: &mut std::process::Command, lock: &std::fs::File) {
    use std::os::{fd::AsRawFd, unix::process::CommandExt};
    let fd = lock.as_raw_fd();
    command
        .env("LLM_GOVERNANCE_COMMIT_LOCK_FD", fd.to_string())
        .env("LLM_GOVERNANCE_COMMIT_LOCK_TOKEN", "a".repeat(64));
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}
#[test]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT and current source-matching FORGE_TEST_BIN"]
fn fresh_review_publishes_canonical_receipt_then_attests_message() {
    let mut case = fixture();
    case.delegated();
    capture_engine(&case);
    let before = case.source_state();
    let message = case.fixture.root.join("message");
    std::fs::write(&message, b"subject\n").unwrap();
    std::fs::set_permissions(&message, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut cmd = case.command_for("attest-message", "index", &[]);
    cmd.arg(&message)
        .args(["--surface", "pre-commit", "--execute"]);
    let result = value(
        forge::process_probe::run(&mut cmd, Duration::from_secs(80), 1024 * 1024).unwrap(),
        0,
    );
    assert_eq!(result["attestation"]["reused"], false);
    let report = &result["review"];
    assert_eq!(report["decision"], "pass");
    assert_eq!(report["review_complete"], true);
    assert_eq!(report["execution_verified"], true);
    assert_eq!(report["approval"], false);
    let tree = report["candidate"]["tree_oid"].as_str().unwrap();
    let receipt = case
        .fixture
        .root
        .join("repo/.git/governance/receipts/pre-commit")
        .join(format!("{tree}-fast.json"));
    let receipt_bytes = std::fs::read(&receipt).unwrap();
    let sealed = forge::candidate_receipt::parse_and_verify(&receipt_bytes).unwrap();
    assert_eq!(sealed.payload(), report);
    let text = std::fs::read_to_string(&message).unwrap();
    assert!(text.contains(&format!("Governance-Tree: {tree}\n")));
    assert!(text.contains(&format!(
        "Governance-Policy: {}\n",
        report["policy"]["sha256"].as_str().unwrap()
    )));
    assert!(text.contains(&format!("Governance-Receipt: {}\n", sealed.digest())));
    case.assert_preserved(&before);
    let mut repeated = case.command_for("attest-message", "index", &[]);
    repeated
        .arg(&message)
        .args(["--surface", "pre-commit", "--execute"]);
    let fresh = value(
        forge::process_probe::run(&mut repeated, Duration::from_secs(80), 1024 * 1024).unwrap(),
        0,
    );
    assert_eq!(fresh["review"]["review_complete"], true);
    assert_eq!(fresh["review"]["decision"], "pass");
    assert_eq!(fresh["attestation"]["reused"], true);
    assert_eq!(fresh["attestation"]["receipt_digest"], sealed.digest());
    assert_eq!(std::fs::read(&receipt).unwrap(), receipt_bytes);
    assert_eq!(std::fs::read_to_string(&message).unwrap(), text);
    std::fs::write(&message, b"subject\nGovernance-Tree: conflicting\n").unwrap();
    std::fs::set_permissions(&message, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut cmd = case.command_for("attest-message", "index", &[]);
    cmd.arg(&message)
        .args(["--surface", "pre-commit", "--execute"]);
    let output = forge::process_probe::run(&mut cmd, Duration::from_secs(80), 1024 * 1024).unwrap();
    assert!(!output.status.success());
    assert_eq!(
        std::fs::read(&message).unwrap(),
        b"subject\nGovernance-Tree: conflicting\n"
    );
    assert_eq!(std::fs::read(&receipt).unwrap(), receipt_bytes);
    case.assert_preserved(&before);
}
#[test]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT and current source-matching FORGE_TEST_BIN"]
fn inherited_exclusive_descriptor_survives_fresh_review() {
    use std::os::fd::AsRawFd;
    let mut case = fixture();
    case.delegated();
    capture_engine(&case);
    let before = case.source_state();
    let lock = inherited_lock(&case);
    let message = case.fixture.root.join("message");
    std::fs::write(&message, b"subject\n").unwrap();
    std::fs::set_permissions(&message, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut cmd = case.command_for("attest-message", "index", &[]);
    cmd.arg(&message)
        .args(["--surface", "pre-commit", "--execute"]);
    pass_lock(&mut cmd, &lock);
    let report = value(
        forge::process_probe::run(&mut cmd, Duration::from_secs(80), 1024 * 1024).unwrap(),
        0,
    );
    assert_eq!(report["review"]["review_complete"], true);
    let other = std::fs::File::open(
        case.fixture
            .root
            .join("repo/.git/governance/commit-review.lock"),
    )
    .unwrap();
    assert_ne!(
        unsafe { libc::flock(other.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) },
        0
    );
    drop(lock);
    case.assert_preserved(&before);
}
fn stage_ready(case: &Case) -> bool {
    for (i, entry) in std::fs::read_dir(case.fixture.root.join("scratch"))
        .unwrap()
        .enumerate()
    {
        assert!(i < 32);
        let entry = entry.unwrap();
        if !entry.file_type().unwrap().is_dir() {
            continue;
        }
        for (j, child) in std::fs::read_dir(entry.path()).unwrap().enumerate() {
            assert!(j < 32);
            let child = child.unwrap();
            if child.file_type().unwrap().is_dir() && child.path().join("ready.pid").is_file() {
                return true;
            }
        }
    }
    false
}
#[test]
#[ignore = "requires delegated FORGE_TEST_CGROUP_PARENT and current source-matching FORGE_TEST_BIN"]
fn index_drift_during_real_stage_refuses_before_canonical_publication() {
    let mut case = Case::new(support::task_worker());
    case.grants["resources"] = json!({"cpu_millis":1000,"memory_mib":128,"processes":16});
    case.grants["batch"] =
        json!({"max_workers":2,"cpu_millis":2000,"memory_mib":256,"processes":32});
    case.staged_policy(|p| {
        p.replace("timeout_seconds=2", "timeout_seconds=10")
            .replace(
                "command=['worker','echo','literal argument','{files}']",
                "command=['worker','pid-sleep','3500','ready.pid']",
            )
    });
    case.delegated();
    capture_engine(&case);
    let message = case.fixture.root.join("message");
    std::fs::write(&message, b"subject\n").unwrap();
    std::fs::set_permissions(&message, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut cmd = case.command_for("attest-message", "index", &[]);
    cmd.arg(&message)
        .args(["--surface", "pre-commit", "--execute"]);
    let worker = std::thread::spawn(move || {
        forge::process_probe::run(&mut cmd, Duration::from_secs(80), 1024 * 1024).unwrap()
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !stage_ready(&case) {
        assert!(
            std::time::Instant::now() < deadline,
            "Stage did not reach owned readiness marker"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    case.fixture
        .stage("late.txt", "index changed after Stage launch\n");
    let changed = case.source_state();
    let output = worker.join().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("index, policy or operator grants changed"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(&message).unwrap(), b"subject\n");
    assert!(!case
        .fixture
        .root
        .join("repo/.git/governance/receipts/pre-commit")
        .exists());
    case.assert_preserved(&changed);
}
