use super::*;
use serde_json::json;
use std::{fs, os::unix::fs::symlink};

fn mutant(name: &str) -> Value {
    json!({"name":name,"package":"demo","file":"src/a.rs","function":null,
        "span":{"start":{"line":1,"column":1},"end":{"line":1,"column":2}},"replacement":"false","genre":"FnValue"})
}
fn report(rows: Vec<Value>) -> Value {
    let count = rows.len();
    let mut outcomes = vec![json!({"scenario":"Baseline","summary":"Success"})];
    outcomes.extend(rows.into_iter().map(|row| json!({"scenario":{"Mutant":row},"summary":"CaughtMutant",
        "phase_results":[{"phase":"Build","process_status":"Success"},{"phase":"Test","process_status":{"Failure":101}}]})));
    json!({"cargo_mutants_version":"27.1.0","total_mutants":count,"caught":count,"missed":0,"timeout":0,"unviable":0,"success":0,"outcomes":outcomes})
}
fn sources() -> BTreeSet<String> {
    BTreeSet::from(["src/a.rs".into()])
}

#[test]
fn partial_counts_only_reconciled_completed_scenarios() {
    let a = mutant("a");
    let b = mutant("b");
    let found = inventory(&json!([a, b]), &sources(), "demo").unwrap();
    let (counts, _) = outcomes(&report(vec![mutant("a")]), &found, &sources(), "demo").unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(counts["KILLED"], 1);
    assert_eq!(
        outcomes(&report(vec![]), &found, &sources(), "demo")
            .unwrap()
            .0["KILLED"],
        0
    );
    assert!(outcomes(&report(vec![mutant("c")]), &found, &sources(), "demo").is_err());
    assert!(outcomes(
        &report(vec![mutant("a"), mutant("a")]),
        &found,
        &sources(),
        "demo"
    )
    .is_err());
    let mut wrong = report(vec![mutant("a")]);
    wrong["caught"] = json!(0);
    assert!(outcomes(&wrong, &found, &sources(), "demo").is_err());
    let mut wrong = report(vec![mutant("a")]);
    wrong["outcomes"][0]["summary"] = json!("unknown");
    assert!(outcomes(&wrong, &found, &sources(), "demo").is_err());
    let mut wrong = report(vec![mutant("a")]);
    wrong["outcomes"].as_array_mut().unwrap().remove(0);
    assert!(outcomes(&wrong, &found, &sources(), "demo").is_err());
    let mut wrong = report(vec![mutant("a")]);
    wrong["outcomes"][0]["summary"] = json!("Failure");
    assert!(outcomes(&wrong, &found, &sources(), "demo").is_err());
    let mut wrong = report(vec![mutant("a")]);
    wrong["outcomes"]
        .as_array_mut()
        .unwrap()
        .insert(0, json!({"scenario":"Baseline","summary":"Success"}));
    assert!(outcomes(&wrong, &found, &sources(), "demo").is_err());
    let mut baseline_only = report(vec![]);
    baseline_only["outcomes"][0]["summary"] = json!("Failure");
    assert!(outcomes(&baseline_only, &found, &sources(), "demo").is_ok());
    baseline_only["outcomes"] = json!([]);
    assert!(outcomes(&baseline_only, &found, &sources(), "demo").is_ok());
    let mut wrong = report(vec![mutant("a")]);
    wrong["success"] = json!(1);
    assert!(outcomes(&wrong, &found, &sources(), "demo").is_err());
    let mut wrong = report(vec![mutant("a")]);
    wrong["outcomes"][1]["phase_results"][0]["process_status"] = json!({"Failure":101});
    assert!(outcomes(&wrong, &found, &sources(), "demo").is_err());
}

#[test]
fn discovery_refuses_duplicate_and_outside_sources_and_package() {
    let a = mutant("a");
    assert!(inventory(&json!([a, a]), &sources(), "demo").is_err());
    let mut outside = mutant("a");
    outside["file"] = json!("../a.rs");
    assert!(inventory(&json!([outside]), &sources(), "demo").is_err());
    let mut package = mutant("a");
    package["package"] = json!("other");
    assert!(inventory(&json!([package]), &sources(), "demo").is_err());
    let mut malformed = mutant("a");
    malformed["span"]["start"]["line"] = json!(0);
    assert!(inventory(&json!([malformed]), &sources(), "demo").is_err());
}

#[test]
fn bounded_capture_refuses_duplicate_keys_symlink_and_oversize_and_detects_drift() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("report.json");
    fs::write(&path, b"{\"a\":1,\"a\":2}").unwrap();
    assert!(capture(&path).is_err());
    fs::remove_file(&path).unwrap();
    symlink("outside", &path).unwrap();
    assert!(capture(&path).is_err());
    fs::remove_file(&path).unwrap();
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_BYTES as u64 + 1).unwrap();
    assert!(capture(&path).is_err());
    fs::write(&path, b"{}").unwrap();
    let (held, _, _) = capture(&path).unwrap();
    fs::write(&path, b"{\"changed\":true}").unwrap();
    assert!(held.verify().is_err());
}

#[test]
fn engine_hook_records_diagnostic_only_after_matching_owned_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("out");
    fs::create_dir_all(output.join("mutants.out")).unwrap();
    fs::write(
        output.join("mutants.out/mutants.json"),
        serde_json::to_vec(&json!([mutant("a"), mutant("b")])).unwrap(),
    )
    .unwrap();
    fs::write(
        output.join("mutants.out/outcomes.json"),
        serde_json::to_vec(&report(vec![mutant("a")])).unwrap(),
    )
    .unwrap();
    let overrides = json!({"language":"rust","mutation_engine":"cargo-mutants",
        "generator":{"source":["src/a.rs"],"run_timeout_seconds":60,"options":{"package":"demo"}},
        "test_argv":["cargo","test"],"source_sha256":{"src/a.rs":"e".repeat(64)}});
    crate::mutation_run_test_support::with_run(root.path(), None, overrides, |run| {
        let selected = run.campaign.clone();
        let argv = vec!["cargo-mutants".to_owned(), "mutants".to_owned()];
        run.receipt.insert("status".into(), json!("RUNNING"));
        run.receipt
            .insert("process_cleanup".into(), json!({"complete":false}));
        super::super::record_partial(run, &output, &selected, &argv, "actor_error");
        assert!(run.receipt["engine_partial_progress_error"]
            .as_str()
            .unwrap()
            .contains("not verified"));
        assert!(!run.receipt.contains_key("engine_partial_progress"));
        run.receipt.remove("engine_partial_progress_error");
        run.receipt
            .insert("process_cleanup".into(), json!({"complete":true}));
        run.receipt.insert(
            "process_commands".into(),
            json!([{"state":"observed","argv":argv,"confinement":{"cleanup_complete":true,"outcome":"exited"}}]),
        );
        super::super::record_partial(run, &output, &selected, &argv, "actor_error");
        assert_eq!(
            run.receipt["engine_partial_progress"]["reason"],
            "actor_error"
        );
        run.receipt["process_commands"][0]["confinement"]["outcome"] = json!("timed_out");
        super::super::record_partial(run, &output, &selected, &argv, "actor_error");
        assert_eq!(run.receipt["engine_partial_progress"]["reason"], "timeout");
        assert_eq!(
            run.receipt["engine_partial_progress"]["inventory"]["discovered"],
            2
        );
        assert_eq!(
            run.receipt["engine_partial_progress"]["outcomes"]["observed"],
            1
        );
        assert!(run.receipt["engine_partial_progress"]["remaining_or_interrupted"].is_null());
        assert_eq!(run.receipt["status"], "RUNNING");
        assert!(
            !run.receipt.contains_key("mutants") && !run.receipt.contains_key("source_coverage")
        );
        assert!(run.planned.is_none());
    });
}
