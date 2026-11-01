#![cfg(target_os = "linux")]
#[path = "support/candidate_policy_fixture.rs"]
#[allow(dead_code)]
mod candidate_policy_fixture;
#[allow(dead_code)]
mod support;

use candidate_policy_fixture::{policy, Fixture};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

fn bounded(mut command: Command) -> Output {
    forge::process_probe::run(&mut command, Duration::from_secs(10), 1024 * 1024)
        .expect("candidate assessment must finish within its finite process bound")
}

fn command(fixture: &Fixture, results: &Path) -> Command {
    let mut command = fixture.command_for("assess");
    command.arg("--results").arg(results);
    command
}

fn write_results(fixture: &Fixture, name: &str, value: &Value) -> PathBuf {
    let path = fixture.root.join(name);
    std::fs::write(&path, serde_json::to_vec(value).unwrap()).unwrap();
    path
}

fn finding(path: Option<&str>, message: &str) -> Value {
    json!({"check_id":"zeta","rule_id":"fixture-finding","severity":"high",
        "message":message,"path":path,"inherited":true,"exception_id":"spoofed"})
}

fn result(status: &str, findings: Vec<Value>) -> Value {
    json!({"check_id":"zeta","status":status,"duration_ms":1,"findings":findings})
}

fn report(output: &Output, expected_code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(expected_code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["assessment_only"], true);
    assert_eq!(report["plan"]["plan_only"], true);
    assert_eq!(report["assessment"]["scope"], "supplied_results");
    assert_eq!(report["assessment"]["review_complete"], false);
    assert_eq!(report["assessment"]["execution_verified"], false);
    report
}

fn diff_policy() -> String {
    format!(
        "{}\n{}",
        policy("zeta").replace("[checks.zeta]", "[checks.zeta]\nattribution='diff'"),
        r#"
[[exceptions]]
id='known'
check='zeta'
path='src/security/test_unit.rs'
owner='Tim'
justification='Specific known fixture finding with measured scope'
expires=2030-01-20
"#
    )
}

#[test]
fn assess_honors_staged_policy_and_reports_pass_fail_without_executing_checks() {
    let fixture = Fixture::new();
    fixture.configured();
    fixture.stage("src/other.rs", "fn changed() {}\n");
    let staged = diff_policy();
    fixture.stage("policies/main.toml", &staged);
    std::fs::write(
        fixture.root.join("repo/policies/main.toml"),
        "invalid dirty working policy",
    )
    .unwrap();
    let pass = write_results(
        &fixture,
        "pass.json",
        &json!([result(
            "failed",
            vec![
                finding(Some("src/security/test_unit.rs"), "known exception"),
                finding(Some("src/preexisting.rs"), "unrelated debt"),
            ]
        )]),
    );
    let passed = report(&bounded(command(&fixture, &pass)), 0);
    assert_eq!(
        passed["plan"]["policy"]["sha256"],
        format!("{:x}", Sha256::digest(staged.as_bytes()))
    );
    assert_eq!(
        passed["plan"]["policy"]["configuration"]["checks"][0]["attribution"],
        "diff"
    );
    assert_eq!(passed["assessment"]["decision"], "pass");
    assert_eq!(passed["assessment"]["findings"][0]["exception_id"], "known");
    assert_eq!(passed["assessment"]["findings"][0]["inherited"], false);
    assert_eq!(passed["assessment"]["findings"][1]["inherited"], true);
    assert!(passed["assessment"]["findings"][1]["exception_id"].is_null());
    let fail = write_results(
        &fixture,
        "fail.json",
        &json!([result(
            "failed",
            vec![finding(Some("src/other.rs"), "new regression")]
        )]),
    );
    let failed = report(&bounded(command(&fixture, &fail)), 1);
    assert_eq!(failed["assessment"]["decision"], "fail");
    assert_eq!(failed["assessment"]["findings"][0]["inherited"], false);
    assert!(failed["assessment"]["findings"][0]["exception_id"].is_null());
    fixture.clean_scratch();
}

#[test]
fn error_pathless_diff_results_cannot_be_suppressed_by_input_or_policy() {
    let fixture = Fixture::new();
    fixture.configured();
    let fingerprint = "a".repeat(24);
    let fingerprint_exception = format!(
        r#"
[[exceptions]]
id='fingerprint'
check='zeta'
path='src/unrelated.rs'
fingerprint='{fingerprint}'
owner='Tim'
justification='Exact fingerprint fixture that cannot suppress execution errors'
expires=2030-01-20
"#
    );
    let staged = format!("{}\n{fingerprint_exception}", diff_policy());
    fixture.stage("policies/main.toml", &staged);
    let mut error = finding(None, "execution failed");
    error["severity"] = json!("info");
    error["fingerprint"] = json!(fingerprint);
    let path = write_results(
        &fixture,
        "error.json",
        &json!([result("error", vec![error])]),
    );
    let report = report(&bounded(command(&fixture, &path)), 1);
    assert_eq!(report["assessment"]["decision"], "fail");
    assert_eq!(report["assessment"]["findings"][0]["inherited"], false);
    assert!(report["assessment"]["findings"][0]["exception_id"].is_null());
    fixture.clean_scratch();
}

#[test]
fn empty_results_are_assessment_only_and_do_not_claim_review_or_execution() {
    let fixture = Fixture::new();
    fixture.configured();
    let path = write_results(&fixture, "empty.json", &json!([]));
    let report = report(&bounded(command(&fixture, &path)), 0);
    assert_eq!(report["assessment"]["decision"], "pass");
    assert_eq!(report["assessment"]["findings"], json!([]));
    assert!(report["plan"]["checks"][0]["scheduled"].as_bool().unwrap());
    assert_eq!(
        report["plan"]["checks"][0]["check"]["command"],
        json!(["/must-never-be-executed"])
    );
    fixture.clean_scratch();
}

#[test]
fn malformed_duplicate_unknown_and_wrong_typed_result_inputs_fail_closed() {
    let fixture = Fixture::new();
    fixture.configured();
    let path = fixture.root.join("malformed.json");
    for input in [
        &b"{"[..],
        &b"{}"[..],
        &b"\xff"[..],
        &br#"[{"check_id":"zeta","check_id":"alpha","status":"passed","duration_ms":0}]"#[..],
        &br#"[{"check_id":"zeta","status":"passed","duration_ms":0,"execution_verified":true}]"#[..],
        &br#"[{"check_id":"zeta","status":7,"duration_ms":0}]"#[..],
        &br#"[{"check_id":"zeta","status":"passed","duration_ms":"zero"}]"#[..],
        &br#"[{"check_id":"zeta","status":"success","duration_ms":0}]"#[..],
        &br#"[{"check_id":"zeta","status":"failed","duration_ms":0,"findings":[{"check_id":"zeta","rule_id":"x","severity":"urgent","message":"x"}]}]"#[..],
    ] {
        std::fs::write(&path, input).unwrap();
        let output = bounded(command(&fixture, &path));
        assert_eq!(output.status.code(), Some(1), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
        fixture.clean_scratch();
    }
    std::fs::File::create(&path)
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    let output = bounded(command(&fixture, &path));
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("byte limit"));
    fixture.clean_scratch();
}

#[test]
fn fifo_and_final_symlink_results_are_refused_without_hanging() {
    use std::os::unix::{ffi::OsStrExt, fs::symlink};
    let fixture = Fixture::new();
    fixture.configured();
    let regular = write_results(&fixture, "regular.json", &json!([]));
    let link = fixture.root.join("linked.json");
    symlink(&regular, &link).unwrap();
    let fifo = fixture.root.join("fifo.json");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: this creates only a private fixture FIFO at the initialized path.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    for path in [&fifo, &link] {
        let mut command = command(&fixture, path);
        let output = forge::process_probe::run(&mut command, Duration::from_secs(2), 64 * 1024)
            .expect("unsafe result inputs must be refused before the finite timeout");
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
        fixture.clean_scratch();
    }
}

#[test]
fn semantic_assessment_errors_clean_the_prepared_candidate_snapshot() {
    let fixture = Fixture::new();
    fixture.configured();
    for (name, value, message) in [
        (
            "unknown.json",
            json!([{"check_id":"unknown","status":"passed","duration_ms":0}]),
            "unknown check",
        ),
        (
            "empty-failure.json",
            json!([result("failed", vec![])]),
            "no findings",
        ),
    ] {
        let path = write_results(&fixture, name, &value);
        let output = bounded(command(&fixture, &path));
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        fixture.clean_scratch();
    }
}

#[test]
fn concurrent_pass_and_semantic_failure_assessments_leave_no_snapshots() {
    let fixture = Fixture::new();
    fixture.configured();
    let pass = write_results(&fixture, "pass.json", &json!([]));
    let invalid = write_results(
        &fixture,
        "invalid.json",
        &json!([{"check_id":"unknown","status":"passed","duration_ms":0}]),
    );
    let workers: Vec<_> = (0..8)
        .map(|index| {
            let success = index % 2 == 0;
            let command = command(&fixture, if success { &pass } else { &invalid });
            std::thread::spawn(move || (bounded(command), success))
        })
        .collect();
    for worker in workers {
        let (output, success) = worker.join().unwrap();
        if success {
            assert_eq!(report(&output, 0)["assessment"]["decision"], "pass");
        } else {
            assert_eq!(output.status.code(), Some(1));
            assert!(String::from_utf8_lossy(&output.stderr).contains("unknown check"));
        }
    }
    fixture.clean_scratch();
}
