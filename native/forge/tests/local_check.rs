//! End-to-end current-checkout check/verify behavior in disposable Git fixtures.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture(check: &str, timeout: u64) -> PathBuf {
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("forge-local-check-{}-{serial}", std::process::id()));
    fs::create_dir_all(root.join(".forge")).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.name", "Fixture"]);
    git(&root, &["config", "user.email", "fixture@example.invalid"]);
    let policy = format!(
        r#"
base_ref = "origin/main"
allowed_untracked_prefixes = ["research/"]

[[setup]]
name = "prepare"
run = "true"
timeout_s = 2

[[check]]
name = "probe"
run = {check:?}
paths = ['^src/']
timeout_s = {timeout}
"#
    );
    fs::write(root.join(".forge/local-check.toml"), policy).unwrap();
    fs::write(root.join("source.txt"), "base\n").unwrap();
    git(&root, &["add", ".forge/local-check.toml", "source.txt"]);
    git(&root, &["commit", "-qm", "base"]);
    git(&root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    git(&root, &["checkout", "-qb", "feature"]);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/new.py"), "x = 1\n").unwrap();
    git(&root, &["add", "src/new.py"]);
    git(&root, &["commit", "-qm", "feature"]);
    root
}

fn forge(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_forge"))
        .args(args)
        .current_dir(dir)
        .env("GIT_DIR", "/definitely/wrong")
        .env("GIT_WORK_TREE", "/definitely/wrong")
        .env("PYO3_NO_PYTHON", "1")
        .output()
        .unwrap()
}

fn receipt(root: &Path) -> PathBuf {
    let base = root.join(".git/forge-checks");
    let run = fs::read_to_string(base.join("latest")).unwrap();
    base.join(run).join("receipt.json")
}

#[test]
fn full_pass_is_bound_to_head_and_log_bytes() {
    let root = fixture("echo checked", 2);
    fs::create_dir_all(root.join("research")).unwrap();
    fs::write(root.join("research/draft.txt"), "unrelated").unwrap();
    let check = forge(&root, &["check", "--all"]);
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(forge(&root, &["verify", "--require-all"]).status.success());
    let receipt_path = receipt(&root);
    let text = fs::read_to_string(&receipt_path).unwrap();
    assert!(text.contains("\"passed\": true"));
    let log = receipt_path.parent().unwrap().join("01-check.log");
    let original = fs::read(&log).unwrap();
    fs::write(&log, "tampered").unwrap();
    assert!(!forge(&root, &["verify", "--require-all"]).status.success());
    fs::write(&log, original).unwrap();
    fs::write(
        receipt_path.parent().unwrap().join("changed-python.txt"),
        "other.py\n",
    )
    .unwrap();
    assert!(!forge(&root, &["verify", "--require-all"]).status.success());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_and_timed_out_attempts_cannot_verify_old_pass() {
    let root = fixture("test ! -e research/fail", 2);
    assert!(forge(&root, &["check", "--all"]).status.success());
    let old = receipt(&root);
    fs::create_dir_all(root.join("research")).unwrap();
    fs::write(root.join("research/fail"), "x").unwrap();
    assert!(!forge(&root, &["check", "--all"]).status.success());
    assert!(!forge(&root, &["verify", "--require-all"]).status.success());
    assert!(!forge(
        &root,
        &[
            "verify",
            "--require-all",
            "--receipt",
            old.to_str().unwrap()
        ]
    )
    .status
    .success());
    fs::remove_dir_all(root).unwrap();
    let root = fixture("sleep 5", 1);
    assert!(!forge(&root, &["check", "--all"]).status.success());
    assert!(!forge(&root, &["verify", "--require-all"]).status.success());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_head_and_untracked_source_fail_closed() {
    let root = fixture("true", 2);
    assert!(forge(&root, &["check", "--all"]).status.success());
    fs::write(root.join("src/untracked.py"), "x = 2\n").unwrap();
    assert!(!forge(&root, &["verify", "--require-all"]).status.success());
    fs::remove_file(root.join("src/untracked.py")).unwrap();
    fs::write(root.join("source.txt"), "new\n").unwrap();
    git(&root, &["add", "source.txt"]);
    git(&root, &["commit", "-qm", "advance"]);
    assert!(!forge(&root, &["verify", "--require-all"]).status.success());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn optional_dag_runs_independent_checks_and_skips_failed_dependencies() {
    let root = fixture("true", 2);
    let policy = root.join(".forge/local-check.toml");
    let mut text = fs::read_to_string(&policy).unwrap();
    text.push_str(
        r#"
[[check]]
name = "dependency"
run = "false"
timeout_s = 2
[[check]]
name = "dependent"
run = "touch ran-dependent"
timeout_s = 2
[schedule.probe]
cpus = 1
resources = []
[schedule.dependency]
cpus = 1
resources = []
[schedule.dependent]
cpus = 1
resources = []
depends_on = ["dependency"]
"#,
    );
    fs::write(policy, text).unwrap();
    git(&root, &["add", ".forge/local-check.toml"]);
    git(&root, &["commit", "-qm", "schedule"]);
    git(&root, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let run = forge(&root, &["check", "--all", "--jobs", "2"]);
    assert!(!run.status.success());
    assert!(!root.join("ran-dependent").exists());
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(receipt(&root)).unwrap()).unwrap();
    assert_eq!(receipt["steps"][1]["verdict"], "passed");
    assert_eq!(receipt["steps"][2]["verdict"], "failed");
    assert_eq!(receipt["steps"][3]["verdict"], "skipped");
    assert!(receipt["steps"][1]["usage"]["wall_ms"].as_f64().unwrap() > 0.0);
    assert!(!forge(&root, &["verify", "--require-all"]).status.success());
    fs::remove_dir_all(root).unwrap();
}
