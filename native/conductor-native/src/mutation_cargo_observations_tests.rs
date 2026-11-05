//! Synthetic protocol/parser unit fixtures only; never campaign or PASS evidence.
use super::*;
const NODE: &str = "tests/contract.rs::values_are_true";
const ID: &str = "caller-forge-stable-id-0";
fn target() -> Target {
    Target {
        cargo_executable: "/toolchain/bin/cargo".into(),
        package: "fixture".into(),
        version: "0.1.0".into(),
        test: "contract".into(),
        test_path: "tests/contract.rs".into(),
    }
}
fn ranked() -> Vec<String> {
    vec![NODE.into()]
}
fn stdout(killed: bool) -> String {
    if killed {
        "\nrunning 1 test\ntest values_are_true ... FAILED\n\nfailures:\n\n---- values_are_true stdout ----\nassertion failed\n\nfailures:\n    values_are_true\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\nerror: test failed, to rerun pass `--test contract`\n".into()
    } else {
        "\nrunning 1 test\ntest values_are_true ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n\n".into()
    }
}
fn scenario() -> Value {
    json!({"Mutant":{"name":"src/a.rs:1: replace value -> bool with false","package":"fixture","file":"src/a.rs","function":{"function_name":"value","return_type":"bool"},"span":{"start":{"line":1,"column":25},"end":{"line":1,"column":29}},"genre":"FnValue","replacement":"false"}})
}
fn outcome(scenario: Value, path: &str, killed: bool) -> Value {
    let target = target();
    json!({"scenario":scenario,"summary":if killed {"CaughtMutant"} else {"Success"},"log_path":path,"diff_path":if killed {json!("diff/a.diff")} else {Value::Null},
        "phase_results":[{"phase":"Build","duration":0.1,"process_status":"Success","argv":expected_argv(&target,true)},
            {"phase":"Test","duration":0.1,"process_status":if killed {json!({"Failure":101})} else {json!("Success")},"argv":expected_argv(&target,false)}]})
}
fn log(path: &str, killed: bool) -> ScenarioLog {
    let target = target();
    ScenarioLog {path:path.into(),text:format!("\n*** scenario\n*** mutation diff:\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n\n*** {}\ncompile diagnostics\n*** result: Success\n\n*** {}\n{}\n*** result: {}\n",expected_argv(&target,true).join(" "),expected_argv(&target,false).join(" "),stdout(killed),if killed {"Failure(101)"} else {"Success"})}
}
struct Fixture {
    report: Value,
    logs: Vec<ScenarioLog>,
    expected: Vec<ExpectedMutant>,
    baselines: Vec<Value>,
}
impl Fixture {
    fn new() -> Self {
        let baseline = parse_baseline(&stdout(false), &target(), &ranked()).unwrap();
        Self {
            report: json!({"cargo_mutants_version":"27.1.0","end_time":"2026-11-05T00:00:00Z","total_mutants":1,"caught":1,"missed":0,"timeout":0,"unviable":0,"success":0,
                "outcomes":[outcome(json!("Baseline"),"log/baseline.log",false),outcome(scenario(),"log/a.log",true)]}),
            logs: vec![log("log/baseline.log", false), log("log/a.log", true)],
            expected: vec![ExpectedMutant {
                id: ID.into(),
                scenario: scenario(),
            }],
            baselines: vec![baseline.clone(), baseline],
        }
    }
    fn collect(&self) -> Result<Observations, String> {
        collect(
            &self.report.to_string(),
            &self.logs,
            &target(),
            &ranked(),
            &self.baselines,
            &self.expected,
        )
    }
    fn fails(&self, message: &str) {
        let error = self.collect().unwrap_err();
        assert!(
            error.contains(message),
            "{error:?} did not contain {message:?}"
        );
    }
}
#[test]
fn original_engine_reports_supply_measured_scorer_inputs_without_mutating_anything() {
    let fixture = Fixture::new();
    let before = fixture.report.clone();
    let observed = fixture.collect().unwrap();
    assert_eq!(fixture.report, before);
    assert_eq!(observed.mutants[0].id, ID);
    assert_eq!(
        observed.mutant_reports[ID]["tests"][NODE]["outcome"],
        "FAILED"
    );
    assert_eq!(
        observed.engine_baseline["report"]["tests"][NODE]["outcome"],
        "PASSED"
    );
    assert_eq!(
        observed.mutants[0].log_sha256,
        format!("{:x}", Sha256::digest(fixture.logs[1].text.as_bytes()))
    );
    let value = observed
        .analyze(json!({"adapter":"cargo-libtest","baseline_repetitions":2,
        "contracts":[{"id":"operator-contract","criticality":"high","active_paths":["src/a.rs"]}],
        "tests":[{"nodeid":NODE,"contract_id":"operator-contract","intentional_redundancy":false}],
        "mutation_contracts":[[ID,"operator-contract"]]}))
        .unwrap();
    assert_eq!(value["status"], "PASS");
    assert_eq!(value["tests"][0]["classification"], "CORE");
}
#[test]
fn exact_scenario_identity_not_order_controls_stable_id_reconciliation() {
    let mut fixture = Fixture::new();
    fixture.report["outcomes"].as_array_mut().unwrap().reverse();
    assert_eq!(fixture.collect().unwrap().mutants[0].id, ID);
    for field in ["replacement", "function", "span", "package"] {
        let mut changed = Fixture::new();
        changed.expected[0].scenario["Mutant"][field] = json!("different");
        assert!(changed.collect().is_err(), "accepted drifted {field}");
    }
}
#[test]
fn duplicate_or_extra_scenarios_and_logs_refuse() {
    let mut fixture = Fixture::new();
    fixture.logs[1].path = "log/baseline.log".into();
    fixture.fails("duplicate supplied");
    let mut fixture = Fixture::new();
    fixture.report["outcomes"][1]["log_path"] = json!("log/baseline.log");
    fixture.fails("absent/reused");
    let mut fixture = Fixture::new();
    fixture.expected.push(fixture.expected[0].clone());
    fixture.fails("duplicate/invalid expected");
    let mut fixture = Fixture::new();
    fixture.logs.push(log("log/extra.log", true));
    fixture.fails("inventory incomplete or extra");
}
#[test]
fn unsafe_paths_truncated_and_excessive_bytes_refuse_before_observation() {
    for path in [
        "/tmp/a.log",
        "log/../a.log",
        "log//a.log",
        "log/./a.log",
        "log/a\n.log",
    ] {
        let mut fixture = Fixture::new();
        fixture.logs[1].path = path.into();
        fixture.fails("confined scenario log path");
    }
    let mut fixture = Fixture::new();
    fixture.logs[1].text.truncate(20);
    fixture.fails("command markers");
    let mut fixture = Fixture::new();
    fixture.logs[1].text = "x".repeat(MAX_LOG_BYTES + 1);
    fixture.fails("bytes exceed");
    let fixture = Fixture::new();
    assert!(collect(
        &"x".repeat(MAX_REPORT_BYTES + 1),
        &fixture.logs,
        &target(),
        &ranked(),
        &fixture.baselines,
        &fixture.expected
    )
    .unwrap_err()
    .contains("byte bound"));
}
#[test]
fn strict_json_rejects_nested_duplicate_identity_keys() {
    let fixture = Fixture::new();
    let report = fixture.report.to_string().replace(
        "\"replacement\":\"false\"",
        "\"replacement\":\"false\",\"replacement\":\"true\"",
    );
    assert!(collect(
        &report,
        &fixture.logs,
        &target(),
        &ranked(),
        &fixture.baselines,
        &fixture.expected
    )
    .unwrap_err()
    .contains("duplicate JSON key"));
}
#[test]
fn phases_reject_different_targets_versions_tools_filters_and_signals() {
    for argv in [
        json!(["cargo", "test"]),
        json!(["/toolchain/bin/cargo", "nextest", "run"]),
        json!([
            "/toolchain/bin/cargo",
            "test",
            "--verbose",
            "--package=fixture@0.2.0",
            "--test=contract"
        ]),
        json!([
            "/toolchain/bin/cargo",
            "test",
            "--verbose",
            "--package=fixture@0.1.0",
            "--test=other"
        ]),
        json!([
            "/toolchain/bin/cargo",
            "test",
            "--verbose",
            "--package=fixture@0.1.0",
            "--test=contract",
            "--",
            "--nocapture"
        ]),
    ] {
        let mut fixture = Fixture::new();
        fixture.report["outcomes"][1]["phase_results"][1]["argv"] = argv;
        fixture.fails("argv differs");
    }
    for status in [
        json!({"Signalled":11}),
        json!({"Failure":1}),
        json!("Timeout"),
        json!("Success"),
    ] {
        let mut fixture = Fixture::new();
        fixture.report["outcomes"][1]["phase_results"][1]["process_status"] = status;
        fixture.fails("ordinary expected test exit");
    }
    let mut fixture = Fixture::new();
    fixture.report["outcomes"][1]["phase_results"][0]["process_status"] = json!({"Failure":101});
    fixture.fails("build did not succeed");
}
#[test]
fn case_lines_in_failure_stdout_cannot_complete_missing_ranked_evidence() {
    let mut fixture = Fixture::new();
    fixture.logs[1].text = fixture.logs[1]
        .text
        .replace("test values_are_true ... FAILED\n", "")
        .replace("assertion failed", "test values_are_true ... FAILED");
    fixture.fails("case-like line in captured failure");
    let mut fixture = Fixture::new();
    fixture.logs[1].text = fixture.logs[1]
        .text
        .replace("assertion failed", "test values_are_true ... ok");
    fixture.fails("case-like line in captured failure");
}
#[test]
fn complete_libtest_framing_totals_and_case_inventory_are_required() {
    for (old, new, message) in [
        ("running 1 test", "running 2 tests", "running count"),
        ("0 passed; 1 failed", "1 passed; 0 failed", "summary counts"),
        ("0 ignored", "1 ignored", "summary counts"),
        (
            "test values_are_true ... FAILED",
            "test stranger ... FAILED",
            "unranked libtest case",
        ),
        (
            "test values_are_true ... FAILED",
            "test values_are_true ... ignored",
            "ignored/unknown",
        ),
    ] {
        let mut fixture = Fixture::new();
        fixture.logs[1].text = fixture.logs[1].text.replace(old, new);
        fixture.fails(message);
    }
    let mut fixture = Fixture::new();
    fixture.logs[1].text = fixture.logs[1].text.replace(
        "test values_are_true ... FAILED",
        "test values_are_true ... FAILED\ntest values_are_true ... FAILED",
    );
    fixture.fails("duplicate libtest case");
    let mut fixture = Fixture::new();
    fixture.logs[1].text = fixture.logs[1]
        .text
        .replace("assertion failed", "test result: ok. fake");
    fixture.fails("one completed libtest summary");
}
#[test]
fn incomplete_baselines_and_nonpass_engine_campaigns_refuse_value_evidence() {
    let mut fixture = Fixture::new();
    fixture.baselines.pop();
    fixture.fails("2..5");
    let mut fixture = Fixture::new();
    fixture.baselines[1]["tests"][NODE]["outcome"] = json!("FAILED");
    fixture.fails("passing observation");
    let mut fixture = Fixture::new();
    fixture.report["outcomes"][0]["summary"] = json!("Failure");
    fixture.fails("expected outcome");
    for key in ["missed", "timeout", "unviable"] {
        let mut fixture = Fixture::new();
        fixture.report[key] = json!(1);
        fixture.fails("totals");
    }
    let mut fixture = Fixture::new();
    fixture.report["cargo_mutants_version"] = json!("28.0.0");
    fixture.fails("unsupported");
    let mut fixture = Fixture::new();
    fixture.report["end_time"] = Value::Null;
    fixture.fails("completion marker");
}
#[test]
fn direct_baseline_parser_refuses_multiple_targets_filtered_cases_and_failure_output() {
    let success = stdout(false);
    assert!(parse_baseline(&(success.clone() + &success), &target(), &ranked()).is_err());
    assert!(parse_baseline(
        &success.replace("0 filtered out", "1 filtered out"),
        &target(),
        &ranked()
    )
    .is_err());
    assert!(parse_baseline(&stdout(true), &target(), &ranked()).is_err());
    assert!(parse_baseline(
        &success,
        &target(),
        &["tests/other.rs::values_are_true".into()]
    )
    .is_err());
}

#[test]
fn arbitrary_precision_decimal_durations_remain_numeric_observations() {
    let mut fixture = Fixture::new();
    fixture.report["outcomes"][1]["phase_results"][1]["duration"] = json!(0.125);
    let value = fixture.collect().unwrap();
    assert_eq!(value.mutants[0].phases[1]["duration"].as_f64(), Some(0.125));
    assert!(value.mutants[0].phases[1]["duration"].is_number());
}

#[test]
fn normalization_charges_incrementally_and_caps_label_amplification() {
    let mut charged = MAX_OUTPUT_BYTES - 4;
    charge("a", &mut charged).unwrap();
    assert_eq!(charged, MAX_OUTPUT_BYTES - 1);
    assert!(charge("a", &mut charged)
        .unwrap_err()
        .contains("incremental output bound"));
    let mut fixed = target();
    fixed.test_path = format!("tests/{}contract.rs", "p/".repeat(500));
    let nodes: Vec<_> = (0..1024)
        .map(|i| format!("{}::case_{i}", fixed.test_path))
        .collect();
    let mut output = "running 1024 tests\n".to_owned();
    for i in 0..1024 {
        output.push_str(&format!("test case_{i} ... ok\n"));
    }
    output.push_str("test result: ok. 1024 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n");
    assert!(output.len() < MAX_LOG_BYTES);
    assert!(parse_baseline(&output, &fixed, &nodes)
        .unwrap_err()
        .contains("normalized case inventory exceeds byte bound"));
}
