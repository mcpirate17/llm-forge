#![cfg(target_os = "linux")]
//! Operator-granted end-to-end review with a real compiled slop_core guest.
#[path = "support/candidate_execution_fixture.rs"]
#[allow(dead_code)]
mod candidate_execution_fixture;
#[allow(dead_code)]
mod support;
use candidate_execution_fixture::Case;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

struct Harness {
    case: Case,
    guest: PathBuf,
    guest_hash: String,
    output: PathBuf,
}
fn env_path(name: &str) -> PathBuf {
    let value =
        PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is required")));
    assert!(value.is_absolute(), "{name} must be absolute");
    value
}
fn env_digest() -> String {
    let value =
        std::env::var("FORGE_TEST_SLOP_SHA256").expect("FORGE_TEST_SLOP_SHA256 is required");
    assert!(
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "expected lowercase SHA-256"
    );
    value
}
fn capture_engine(case: &Case) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dest = case.fixture.root.join("repo");
    for path in forge::source_fingerprint::files(&root).unwrap() {
        let target = dest.join(&path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(target, std::fs::read(root.join(&path)).unwrap()).unwrap();
        case.fixture.add(path.to_str().unwrap());
    }
    let repository = git2::Repository::open(&dest).unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let tree = repository.index().unwrap().write_tree().unwrap();
    let tree = repository.find_tree(tree).unwrap();
    let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
    repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "capture native engine and fixture",
            &tree,
            &[&parent],
        )
        .unwrap();
}
fn configure_policy(case: &Case) {
    case.staged_policy(|s| s
        .replace("[classes]\n", "[classes]\npython=['src/*.py']\n")
        .replace("[checks.zeta]\nkind='command'\ntimeout_seconds=2\nmemory_mb=128", "[checks.equivalence-probe]\nkind='builtin'\nalways=true\ntimeout_seconds=120\nmemory_mb=8192")
        .replace("\ncommand=['worker','echo','literal argument','{files}']", "")
        .replace("\nversion_command=['worker','echo','fixture-v1']", "")
        .replace("classes=['rust']", "classes=['python']"));
}
fn add_reads(case: &mut Case, python: &Path, guest: &Path) {
    let mut paths = Vec::new();
    for path in [
        python.parent().unwrap().parent().unwrap(),
        guest.parent().unwrap(),
        Path::new(support::forge_binary::path()).parent().unwrap(),
        Path::new("/usr/local"),
        Path::new("/etc"),
        Path::new("/proc"),
        Path::new("/sys"),
        Path::new("/dev"),
    ] {
        if let Ok(path) = path.canonicalize() {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    let reads = case.grants["read_only"].as_array_mut().unwrap();
    for path in paths {
        let value = json!(path);
        if !reads.contains(&value) {
            reads.push(value);
        }
    }
}
impl Harness {
    fn new(persist: bool, timeout: f64) -> Self {
        Self::with_driver(persist, timeout, false)
    }
    fn with_driver(persist: bool, timeout: f64, misses_negative_regime: bool) -> Self {
        let forge = env_path("FORGE_TEST_BIN");
        let python = env_path("FORGE_TEST_PYTHON");
        let guest = env_path("FORGE_TEST_SLOP_EXTENSION");
        let guest_hash = env_digest();
        assert_eq!(
            guest.canonicalize().unwrap(),
            guest,
            "guest must be a canonical non-symlink file"
        );
        assert_eq!(
            format!("{:x}", Sha256::digest(std::fs::read(&guest).unwrap())),
            guest_hash,
            "operator guest hash mismatch"
        );
        let mut case = Case::new(Path::new("/bin/true"));
        case.delegated();
        case.grants["programs"] = json!({"forge":forge,"python":python});
        case.grants["python"] = json!("python");
        case.grants["resources"] = json!({"cpu_millis":1000,"memory_mib":4096,"processes":128});
        case.grants["batch"] =
            json!({"max_workers":1,"cpu_millis":1000,"memory_mib":4096,"processes":128});
        case.grants["max_wall_seconds"] = json!(150);
        case.grants["max_cpu_seconds"] = json!(120);
        case.grants["max_address_space_mib"] = json!(16384);
        case.grants["environment"] = json!({"PYTEST_DISABLE_PLUGIN_AUTOLOAD":"1","OMP_NUM_THREADS":"1",
            "MKL_NUM_THREADS":"1","OPENBLAS_NUM_THREADS":"1","CUDA_VISIBLE_DEVICES":""});
        add_reads(&mut case, &python, &guest);
        configure_policy(&case);
        let output = case.fixture.root.join("equivalence-artifacts");
        std::fs::create_dir(&output).unwrap();
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o700)).unwrap();
        let persist_line = if persist {
            format!("run_artifacts = '{}'\n", output.display())
        } else {
            String::new()
        };
        case.fixture.stage("pyproject.toml",&format!("[tool.forge.equivalence_probe]\n{persist_line}per_module_timeout={timeout}\njobs=1\n"));
        case.fixture
            .stage("src/probe_target.py", "def clamp(x):\n    return x\n");
        let assertion = if misses_negative_regime {
            "    assert clamp(torch.tensor([2.0])).item() == 2.0\n"
        } else {
            "    assert clamp(torch.tensor([-2.0])).item() == 0.0\n"
        };
        case.fixture.stage("tests/test_probe_target.py", &format!("import torch\nfrom src.probe_target import clamp\n\ndef test_clamp():\n{assertion}"));
        capture_engine(&case);
        case.fixture.stage(
            "src/probe_target.py",
            "def clamp(x):\n    return x.clamp_min(0)\n",
        );
        Self {
            case,
            guest,
            guest_hash,
            output,
        }
    }
    fn command(&self) -> Command {
        let mut command = self.case.command_for("review", "index", &[]);
        command.args(["--surface", "manual", "--execute"]);
        command
    }
    fn guest_args(&self, command: &mut Command, hash: &str) {
        command
            .arg("--equivalence-guest")
            .arg(&self.guest)
            .arg("--equivalence-guest-sha256")
            .arg(hash);
    }
    fn output_arg(&self, command: &mut Command) {
        command.arg("--equivalence-artifacts").arg(&self.output);
    }
    fn run(&self, command: &mut Command) -> Value {
        self.run_with_status(command).0
    }
    fn run_with_status(&self, command: &mut Command) -> (Value, i32) {
        let output =
            forge::process_probe::run(command, Duration::from_secs(180), 1024 * 1024).unwrap();
        let status = output.status.code();
        assert!(
            matches!(status, Some(0 | 1)),
            "review failed to produce receipt: status={status:?}, stdout={}, stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "invalid review JSON: {e}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (report, status.unwrap())
    }
}
fn check<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["check_id"] == id)
        .unwrap()
}
fn evidence(report: &Value) -> &Value {
    &report["evidence"]["equivalence"]
}
fn assert_clean_stage(h: &Harness, report: &Value) {
    let observed = &evidence(report)["stage"]["report"];
    assert_eq!(observed["outcome"], "exited");
    assert_eq!(observed["exit_code"], 0);
    assert_eq!(observed["cleanup_complete"], true);
    assert_eq!(observed["resources"]["verified"], true);
    assert_eq!(observed["errors"], json!([]));
    let group = Path::new(observed["resources"]["path"].as_str().unwrap());
    assert!(!group.exists(), "cgroup still exists: {}", group.display());
    h.case.fixture.clean_scratch();
}
fn assert_receipt_artifact(h: &Harness, report: &Value) {
    let ev = evidence(report);
    let path = Path::new(ev["published_artifact"].as_str().unwrap());
    assert_eq!(path.parent(), Some(h.output.as_path()));
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        ev["summary_sha256"].as_str().unwrap()
    );
    let artifact: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(artifact, ev["summary"]);
    assert_eq!(ev["guest_sha256"], h.guest_hash);
    assert_eq!(ev["guest_output_authenticated"], false);
}
fn assert_real_blocking(report: &Value) {
    assert_eq!(check(report, "engine-integrity")["status"], "passed");
    let row = check(report, "equivalence-probe");
    assert_eq!(row["status"], "failed", "{report}");
    let summary = &evidence(report)["summary"];
    assert_eq!(summary["modules_probed"], 1, "{summary}");
    assert_eq!(summary["incomplete"], json!([]), "{summary}");
    assert!(
        summary["blocking"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| {
                finding["module"] == "src/probe_target.py"
                    && finding["qualname"] == "clamp"
                    && finding["rule"] == "drop_clamp_min"
                    && finding["verdict"] == "REACHABLE_BUT_UNTESTED"
            }),
        "expected measured negative-regime counterexample: {summary}"
    );
    assert!(
        row["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| { finding["rule_id"] == "drop_clamp_min" }),
        "real finding lost by coordinator: {row}"
    );
}
// Restore owner write access before the fixture's recursive cleanup, including
// during assertion unwinding. The binding is never changed during review.
struct RestoreOutputMode(PathBuf);
impl Drop for RestoreOutputMode {
    fn drop(&mut self) {
        std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700))
            .expect("restore owned fixture output directory");
    }
}
#[test]
#[ignore = "requires explicit FORGE_TEST_BIN, FORGE_TEST_CGROUP_PARENT, FORGE_TEST_PYTHON, FORGE_TEST_SLOP_EXTENSION and FORGE_TEST_SLOP_SHA256 with CPU PyTorch and pytest"]
fn real_torch_review_pins_guest_captures_summary_and_publishes_after_cleanup() {
    let h = Harness::new(true, 90.0);
    let before = h.case.source_state();
    let mut ungranted = h.command();
    h.guest_args(&mut ungranted, &h.guest_hash);
    let report = h.run(&mut ungranted);
    assert_eq!(check(&report, "equivalence-probe")["status"], "error");
    assert!(check(&report, "equivalence-probe")["findings"]
        .to_string()
        .contains("requires an explicit output directory grant"));
    assert!(evidence(&report)["stage"].is_null());
    assert_eq!(std::fs::read_dir(&h.output).unwrap().count(), 0);
    h.case.assert_preserved(&before);
    let mut missing = h.command();
    h.output_arg(&mut missing);
    let report = h.run(&mut missing);
    assert_eq!(check(&report, "equivalence-probe")["status"], "error");
    assert!(check(&report, "equivalence-probe")["findings"]
        .to_string()
        .contains("guest path not granted"));
    assert_eq!(std::fs::read_dir(&h.output).unwrap().count(), 0);
    h.case.assert_preserved(&before);
    let mut bad = h.command();
    h.output_arg(&mut bad);
    h.guest_args(&mut bad, &"0".repeat(64));
    let report = h.run(&mut bad);
    assert_eq!(check(&report, "equivalence-probe")["status"], "error");
    assert!(check(&report, "equivalence-probe")["findings"]
        .to_string()
        .contains("digest mismatch"));
    assert_eq!(std::fs::read_dir(&h.output).unwrap().count(), 0);
    h.case.assert_preserved(&before);
    let wrong = h.case.fixture.root.join("wrong-artifacts");
    std::fs::create_dir(&wrong).unwrap();
    std::fs::set_permissions(&wrong, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut mismatch = h.command();
    h.guest_args(&mut mismatch, &h.guest_hash);
    mismatch.arg("--equivalence-artifacts").arg(wrong);
    let report = h.run(&mut mismatch);
    assert_eq!(check(&report, "equivalence-probe")["status"], "error");
    assert!(check(&report, "equivalence-probe")["findings"]
        .to_string()
        .contains("artifact grant differs"));
    h.case.assert_preserved(&before);
    let mut command = h.command();
    h.output_arg(&mut command);
    h.guest_args(&mut command, &h.guest_hash);
    let report = h.run(&mut command);
    assert_eq!(check(&report, "engine-integrity")["status"], "passed");
    assert_eq!(check(&report, "equivalence-probe")["status"], "passed");
    let summary = &evidence(&report)["summary"];
    assert_eq!(summary["modules_probed"], 1);
    assert!(
        summary["live"].as_u64().unwrap() >= 1,
        "real tensor ablation not observed: {summary}"
    );
    assert_eq!(
        summary["modules_index_driven"],
        json!(["src/probe_target.py"])
    );
    assert_eq!(summary["incomplete"], json!([]));
    assert_clean_stage(&h, &report);
    assert_receipt_artifact(&h, &report);
    h.case.assert_preserved(&before);
}
#[test]
#[ignore = "requires explicit FORGE_TEST_BIN, FORGE_TEST_CGROUP_PARENT, FORGE_TEST_PYTHON, FORGE_TEST_SLOP_EXTENSION and FORGE_TEST_SLOP_SHA256 with CPU PyTorch and pytest"]
fn real_guest_timeout_is_incomplete_and_still_cleans_owned_resources() {
    let h = Harness::new(true, 0.001);
    let before = h.case.source_state();
    let mut command = h.command();
    h.output_arg(&mut command);
    h.guest_args(&mut command, &h.guest_hash);
    let report = h.run(&mut command);
    let summary = &evidence(&report)["summary"];
    assert!(
        summary["incomplete"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["verdict"] == "TIMEOUT"),
        "expected real child timeout: {summary}"
    );
    assert!(check(&report, "equivalence-probe")["findings"]
        .to_string()
        .contains("TIMEOUT"));
    assert_clean_stage(&h, &report);
    assert_receipt_artifact(&h, &report);
    h.case.assert_preserved(&before);
}

#[test]
#[ignore = "requires explicit FORGE_TEST_BIN, FORGE_TEST_CGROUP_PARENT, FORGE_TEST_PYTHON, FORGE_TEST_SLOP_EXTENSION and FORGE_TEST_SLOP_SHA256 with CPU PyTorch and pytest"]
fn real_review_publication_is_consumed_by_native_backlog() {
    let h = Harness::with_driver(true, 90.0, true);
    let before = h.case.source_state();
    assert_eq!(std::fs::read_dir(&h.output).unwrap().count(), 0);
    let ledger = h.case.fixture.root.join("backlog-ledger.json");
    let markdown = h.case.fixture.root.join("backlog.md");
    assert!(!ledger.exists() && !markdown.exists());
    let mut command = h.command();
    h.output_arg(&mut command);
    h.guest_args(&mut command, &h.guest_hash);
    let (report, status) = h.run_with_status(&mut command);
    assert_eq!(
        status, 1,
        "blocking review must retain failed CLI status: {report}"
    );
    assert_real_blocking(&report);
    assert_clean_stage(&h, &report);
    assert_receipt_artifact(&h, &report);
    let published = evidence(&report)["published_artifact"].as_str().unwrap();
    let entries: Vec<_> = std::fs::read_dir(&h.output)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries, vec![PathBuf::from(published)]);
    // Consume only the exact fresh, digest-checked publication; never regenerate
    // a summary from receipt fields or substitute a historical fixture artifact.
    let said = slop_core::backlog::backlog(&[published.into()], &ledger, &markdown, false).unwrap();
    assert!(
        said.starts_with("1 findings -> 1 items (1 new, 0 carried, 0 fixed)"),
        "{said}"
    );
    let stored: Value = serde_json::from_slice(&std::fs::read(&ledger).unwrap()).unwrap();
    assert_eq!(stored["scope"], json!(["src/probe_target.py"]));
    let items = stored["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{stored}");
    assert_eq!(items[0]["module"], "src/probe_target.py");
    assert_eq!(items[0]["qualname"], "clamp");
    assert_eq!(items[0]["verdict"], "REACHABLE_BUT_UNTESTED");
    assert_eq!(items[0]["rules"], json!(["drop_clamp_min"]));
    assert_eq!(items[0]["findings"], 1);
    let text = std::fs::read_to_string(&markdown).unwrap();
    assert!(
        text.contains("## New (1)") && text.contains("**clamp**"),
        "{text}"
    );
    assert_receipt_artifact(&h, &report);
    h.case.assert_preserved(&before);
}

#[test]
#[ignore = "requires the real equivalence guest prerequisites and a non-root operator so an owned mode-0500 output directory is unwritable"]
fn real_review_publication_failure_retains_probe_and_names_medium_backlog_gap() {
    assert_ne!(
        unsafe { libc::geteuid() },
        0,
        "permission control requires a non-root operator"
    );
    let h = Harness::with_driver(true, 90.0, true);
    let before = h.case.source_state();
    let _restore = RestoreOutputMode(h.output.clone());
    std::fs::set_permissions(&h.output, std::fs::Permissions::from_mode(0o500)).unwrap();
    // Check the fixture's actual permission refusal before admission. No mode or
    // inode swap occurs after capture; this exercises publication, not drift.
    let error = std::fs::write(h.output.join("must-not-write"), b"control").unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    let mut command = h.command();
    h.output_arg(&mut command);
    h.guest_args(&mut command, &h.guest_hash);
    let (report, status) = h.run_with_status(&mut command);
    assert_eq!(
        status, 1,
        "completed failed review must remain a JSON result: {report}"
    );
    assert_real_blocking(&report);
    assert_clean_stage(&h, &report);
    assert!(evidence(&report)["published_artifact"].is_null());
    // The worker emits compact native Summary serialization, without a newline.
    let typed: slop_core::gate_types::Summary =
        serde_json::from_value(evidence(&report)["summary"].clone()).unwrap();
    let summary = serde_json::to_vec(&typed).unwrap();
    assert_eq!(
        format!("{:x}", Sha256::digest(&summary)),
        evidence(&report)["summary_sha256"]
    );
    let row = check(&report, "equivalence-probe");
    let warnings: Vec<_> = row["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|finding| finding["rule_id"] == "run-artifact-unwritten")
        .collect();
    assert_eq!(warnings.len(), 1, "{row}");
    assert_eq!(warnings[0]["severity"], "medium");
    let message = warnings[0]["message"].as_str().unwrap();
    assert!(
        message.contains("summary was not recorded") && message.contains("backlog will not see it"),
        "{message}"
    );
    assert_eq!(std::fs::read_dir(&h.output).unwrap().count(), 0);
    let ledger = h.case.fixture.root.join("empty-ledger.json");
    let markdown = h.case.fixture.root.join("empty-backlog.md");
    slop_core::backlog::backlog(&[], &ledger, &markdown, false).unwrap();
    let stored: Value = serde_json::from_slice(&std::fs::read(ledger).unwrap()).unwrap();
    assert_eq!(stored["items"], json!([]));
    assert_eq!(stored["scope"], json!([]));
    h.case.assert_preserved(&before);
}
