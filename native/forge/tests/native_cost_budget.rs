#[allow(dead_code)]
mod support;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

fn run(host: &support::Host, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
    command
        .env_clear()
        .env("PATH", "")
        .env("HOME", &host.0)
        .env("LEDGER_ROOT", host.0.join("ledger-data"))
        .current_dir(&host.0)
        .args(["ledger", "budget", "--host"])
        .arg(&host.0)
        .args(args);
    forge::process_probe::run(&mut command, Duration::from_secs(10), 1024 * 1024).unwrap()
}
fn value(output: &Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn table(host: &support::Host, name: &str, row: Value) {
    let directory = host.0.join("ledger-data").join(name);
    fs::create_dir_all(&directory).unwrap();
    let today = forge::instant::isoformat_millis_utc(forge::instant::now());
    fs::write(
        directory.join(format!("{}.jsonl", &today[..10])),
        format!("{row}\n"),
    )
    .unwrap();
}
fn measurements(host: &support::Host, factor: f64, rework: bool) {
    table(
        host,
        "hook_rollup",
        json!({"n_calls":2,"p50_ms":10.0*factor}),
    );
    table(
        host,
        "session_rollup",
        json!({"resend_bytes":(1000.0*factor) as u64}),
    );
    table(
        host,
        "agent_rollup",
        json!({"total_tokens":(1000.0*factor) as u64,"n_landed_prs":1,"join_method":"session_id"}),
    );
    table(
        host,
        "task_dispatch",
        json!({"billed_tokens":10,"over_cap":rework,"tier":"haiku","required_rework":rework}),
    );
}

#[test]
fn empty_and_unbaselined_ledgers_keep_cli_failures_and_lenient_gate_evidence() {
    let host = support::Host::new();
    let empty = run(&host, &["audit"]);
    assert_eq!(empty.status.code(), Some(3));
    assert!(empty.stdout.is_empty());
    let gate = value(&run(&host, &["gate"]), 0);
    assert_eq!(gate["ok"], true);
    assert_eq!(gate["evidence"]["status"], "NO_DATA");
    measurements(&host, 1.0, true);
    let report = value(&run(&host, &["audit"]), 1);
    assert_eq!(report["status"], "NO_BASELINE");
    let gate = value(&run(&host, &["gate"]), 0);
    assert_eq!(gate["ok"], true);
    assert_eq!(gate["evidence"], report);
    assert!(!host.0.join("ledger/cost_budget_baseline.json").exists());
}

#[test]
fn explicit_record_ratchet_pass_regression_and_missing_metric_stay_distinct() {
    let host = support::Host::new();
    measurements(&host, 1.0, true);
    value(&run(&host, &["audit", "--record"]), 0);
    let baseline = host.0.join("ledger/cost_budget_baseline.json");
    let original = fs::read(&baseline).unwrap();
    assert_eq!(value(&run(&host, &["audit"]), 0)["status"], "RATCHET_HELD");
    measurements(&host, 0.5, false);
    assert_eq!(value(&run(&host, &["audit"]), 0)["status"], "PASS");
    assert_eq!(value(&run(&host, &["gate"]), 0)["ok"], true);
    measurements(&host, 2.0, true);
    let report = value(&run(&host, &["audit"]), 1);
    assert_eq!(report["status"], "REGRESSION");
    let gate = value(&run(&host, &["gate"]), 0);
    assert_eq!(gate["ok"], false);
    assert_eq!(gate["evidence"], report);
    measurements(&host, 1.0, true);
    fs::remove_dir_all(host.0.join("ledger-data/session_rollup")).unwrap();
    assert_eq!(value(&run(&host, &["audit"]), 1)["status"], "NO_DATA");
    let gate = value(&run(&host, &["gate"]), 0);
    assert_eq!(gate["ok"], true);
    assert!(gate["detail"]
        .as_str()
        .unwrap()
        .contains("resend_bytes_per_session=NO_DATA"));
    assert_eq!(fs::read(baseline).unwrap(), original);
}

#[test]
fn budget_report_is_a_cost_table_and_routing_report_retains_its_interface() {
    let host = support::Host::new();
    measurements(&host, 1.0, true);
    let output = run(&host, &["report"]);
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("cost-budget report  window "));
    assert!(text.contains("overall NO_BASELINE"));
    assert!(text.contains("tokens_per_landed_pr") && text.contains("1,000.0"));
    let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
    command
        .env_clear()
        .env("PATH", "")
        .args(["ledger", "report", "--ledger-root"])
        .arg(host.0.join("ledger-data"))
        .arg("--json");
    let output = forge::process_probe::run(&mut command, Duration::from_secs(10), 65536).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("cost-budget report"));
    let explicit_output = output.stdout;
    let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
    command
        .env_clear()
        .env("PATH", "")
        .env("LEDGER_ROOT", host.0.join("ledger-data"))
        .args(["ledger", "report", "--json"]);
    let output = forge::process_probe::run(&mut command, Duration::from_secs(10), 65536).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, explicit_output);
}

#[test]
fn malformed_baseline_refuses_gate_and_override_selects_only_the_named_baseline() {
    let host = support::Host::new();
    measurements(&host, 1.0, true);
    let directory = host.0.join("ledger");
    fs::create_dir(&directory).unwrap();
    let default = directory.join("cost_budget_baseline.json");
    fs::write(&default, b"{malformed").unwrap();
    let output = run(&host, &["gate"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let output = run(&host, &["audit", "--baseline", "missing-baseline.json"]);
    assert_eq!(value(&output, 1)["status"], "NO_BASELINE");
    assert_eq!(fs::read(default).unwrap(), b"{malformed");
    assert!(!host.0.join("missing-baseline.json").exists());
}

fn repository(path: &Path) {
    let repo = git2::Repository::init(path).unwrap();
    let signature = git2::Signature::now("fixture", "fixture@example.test").unwrap();
    let mut index = repo.index().unwrap();
    let tree_oid = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    repo.commit(
        Some("refs/heads/main"),
        &signature,
        &signature,
        "fixture (#1)",
        &tree,
        &[],
    )
    .unwrap();
    repo.set_head("refs/heads/main").unwrap();
}
fn transcript(path: &Path, session: &str) {
    fs::create_dir_all(path).unwrap();
    let timestamp = forge::instant::isoformat_millis_utc(forge::instant::now());
    let row = json!({"type":"assistant","uuid":"turn","session_id":session,"timestamp":timestamp,
        "message":{"role":"assistant","model":"fixture","usage":{"input_tokens":1,"output_tokens":1},
            "content":[{"type":"text","text":"PRIVATE_TRANSCRIPT_CONTENT_🦀"}]}});
    fs::write(path.join("session.jsonl"), format!("{row}\n")).unwrap();
}

#[test]
fn default_rollup_is_offline_native_and_never_retains_transcript_content() {
    let host = support::Host::new();
    repository(&host.0);
    let canonical = host.0.canonicalize().unwrap();
    let name = canonical.to_str().unwrap().replace('/', "-");
    let input = host.0.join(".claude/projects").join(name);
    transcript(&input, "default-session");
    let output = run(&host, &["rollup", "--dry-run"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("default-session"));
    assert!(!text.contains("PRIVATE_TRANSCRIPT_CONTENT"));
    assert!(!host.0.join("ledger-data").exists());
    let explicit = host.0.join("override");
    transcript(&explicit, "override-session");
    let output = run(
        &host,
        &[
            "rollup",
            "--transcripts",
            explicit.to_str().unwrap(),
            "--dry-run",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("override-session") && !text.contains("default-session"));
    assert!(!text.contains("PRIVATE_TRANSCRIPT_CONTENT"));
}
