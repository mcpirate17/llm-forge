use super::*;

const ZERO: &str = "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
const PASS: &str = "running 2 tests\ntest unit::works ... ok\ntest doc path - example (line 42) ... ignored, platform unavailable\n\ntest result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n";
#[test]
fn accepts_multiple_complete_frames_with_a_pass_and_spaced_doctest_names() {
    let value = parse_package_baseline(&format!("\n{ZERO}\n{PASS}\n{ZERO}")).unwrap();
    assert_eq!(value["frames"], 3);
    assert_eq!(value["passed"], 1);
    assert_eq!(value["ignored"], 1);
}
#[test]
fn zero_or_only_ignored_do_not_count_as_execution() {
    assert!(parse_package_baseline(ZERO).is_err());
    let ignored = "running 1 test\ntest one ... ignored\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s\n";
    assert!(parse_package_baseline(ignored).is_err());
}
#[test]
fn incomplete_duplicate_unmatched_or_forged_counts_refuse() {
    for text in [
        "running 1 test\ntest one ... ok\n",
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
        "running 2 tests\ntest one ... ok\ntest one ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
        "running 1 test\ntest one ... ok\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
        "running 1 test\nrunning 1 test\n",
        "running 1 test\ntest one ... FAILED\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
    ] { assert!(parse_package_baseline(text).is_err(), "accepted: {text}"); }
}
#[test]
fn rejects_unexpected_output_and_byte_budget_overflow() {
    assert!(parse_package_baseline(&format!("{PASS}unrelated\n")).is_err());
    assert!(parse_package_baseline(&"x".repeat(MAX_LOG_BYTES + 1)).is_err());
}

fn engine(body: &str) -> (Value, String) {
    let build = vec![
        "/bin/cargo",
        "test",
        "--no-run",
        "--verbose",
        "--package=demo@0.1.0",
        "--locked",
        "--offline",
    ];
    let test = vec![
        "/bin/cargo",
        "test",
        "--verbose",
        "--package=demo@0.1.0",
        "--locked",
        "--offline",
    ];
    let outcome = json!({"scenario":"Baseline", "summary":"Success", "phase_results":[
        {"phase":"Build","duration":1.0,"process_status":"Success","argv":build},
        {"phase":"Test","duration":2.0,"process_status":"Success","argv":test}
    ]});
    let log = format!("*** {}\nFresh demo\n*** result: Success\n\n*** {}\nFinished test profile\nRunning unittests\n{body}\nDoc-tests demo\n*** result: Success\n", build.join(" "), test.join(" "));
    (outcome, log)
}
#[test]
fn engine_baseline_accepts_complete_output_and_diagnostics() {
    let (outcome, log) = engine(PASS);
    let value =
        parse_package_engine_baseline(&outcome, &log, "/bin/cargo", "demo", "0.1.0").unwrap();
    assert_eq!(value["passed"], 1);
}
#[test]
fn engine_baseline_refuses_zero_incomplete_or_mismatched_protocol() {
    for body in [ZERO, "running 1 test\ntest one ... ok\n", "running 1 test\ntest one ... ignored\ntest result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"] {
        let (outcome, log) = engine(body);
        assert!(
            parse_package_engine_baseline(&outcome, &log, "/bin/cargo", "demo", "0.1.0").is_err()
        );
    }
    let (mut outcome, log) = engine(PASS);
    outcome["phase_results"][1]["argv"][3] = json!("--package=other@0.1.0");
    assert!(parse_package_engine_baseline(&outcome, &log, "/bin/cargo", "demo", "0.1.0").is_err());
    let (outcome, log) = engine(PASS);
    assert!(
        parse_package_engine_baseline(&outcome, &log, "/other/cargo", "demo", "0.1.0").is_err()
    );
    assert!(parse_package_engine_baseline(
        &outcome,
        &log.replace("*** result: Success", "*** result: Failure(1)"),
        "/bin/cargo",
        "demo",
        "0.1.0"
    )
    .is_err());
    let (outcome, log) = engine("test unmatched ... ok\n");
    assert!(parse_package_engine_baseline(&outcome, &log, "/bin/cargo", "demo", "0.1.0").is_err());
}
