use super::*;
use crate::mutation_run_test_support::campaign;

const CRATE: &str = "tools/snapshot-retention";

fn mutant(summary: &str, line: u64, replacement: &str) -> Value {
    json!({
        "scenario": {"Mutant": {
            "name": format!("src/lib.rs:{line}:5: replace stale"),
            "package": "snapshot-retention",
            "file": "src/lib.rs",
            "function": {
                "function_name": "stale",
                "return_type": "-> Vec<u8>",
                "span": {"start": {"line": line, "column": 1}, "end": {"line": line + 9, "column": 2}},
            },
            "span": {"start": {"line": line, "column": 5}, "end": {"line": line + 1, "column": 40}},
            "replacement": replacement,
            "genre": "FnValue",
        }},
        "summary": summary,
        "phase_results": [{"phase": "Build", "duration": 1.5}, {"phase": "Test", "duration": 0.25}],
    })
}

fn baseline(summary: &str) -> Value {
    json!({"scenario": "Baseline", "summary": summary, "phase_results": []})
}

fn report_rows(outcomes: Vec<Value>) -> Result<Vec<Value>, String> {
    rows(&json!({"outcomes": outcomes}), CRATE)
}

fn bounded(dir: &Path, overrides: Value) -> Campaign {
    let mut subject = campaign(dir, overrides);
    subject.mutant_timeout_seconds = Some(11);
    subject
}

fn cargo_generator(extra: Value) -> Value {
    let mut generator = json!({
        "source": ["src/lib.rs"],
        "exclude": ["src/gen.rs"],
        "run_timeout_seconds": 60,
        "jobs": 2,
        "options": {"manifest_path": "tools/Cargo.toml"},
    });
    for (key, value) in extra.as_object().unwrap() {
        generator["options"][key] = value.clone();
    }
    generator
}

#[test]
fn every_summary_is_mapped_and_none_is_guessed() {
    for (summary, expected) in [
        ("CaughtMutant", KILLED),
        ("MissedMutant", SURVIVED),
        ("Timeout", TIMED_OUT),
        ("Unviable", UNVIABLE),
        ("Failure", ERROR),
    ] {
        let rows = report_rows(vec![baseline("Success"), mutant(summary, 3, "x")]).unwrap();
        assert_eq!(rows[0]["outcome"], expected);
    }
    let error = report_rows(vec![mutant("Skipped", 3, "x")]).unwrap_err();
    assert_eq!(error, "cargo-mutants reported unknown summary 'Skipped'");
}

#[test]
fn the_baseline_scenario_is_not_a_mutant_and_paths_are_crate_relative() {
    let rows = report_rows(vec![baseline("Success"), mutant("MissedMutant", 437, "x")]).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["path"], format!("{CRATE}/src/lib.rs"));
    assert_eq!(rows[0]["function"], "stale");
    let rooted = rows_at_repo_root();
    assert_eq!(rooted[0]["path"], "src/lib.rs");
}

fn rows_at_repo_root() -> Vec<Value> {
    rows(&json!({"outcomes": [mutant("CaughtMutant", 1, "x")]}), "").unwrap()
}

#[test]
fn every_field_a_row_carries_is_pinned() {
    let rows = report_rows(vec![mutant("CaughtMutant", 437, "vec![]")]).unwrap();
    let row = rows[0].as_object().unwrap();
    assert_eq!(
        row.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "id",
            "outcome",
            "path",
            "line",
            "operator",
            "original_text",
            "mutated_text",
            "function",
            "package",
            "duration_seconds",
        ]
    );
    assert_eq!(row["line"], 437);
    assert_eq!(row["operator"], "FnValue");
    assert_eq!(row["original_text"], "stale-> Vec<u8>");
    assert_eq!(row["mutated_text"], "vec![]");
    assert_eq!(row["package"], "snapshot-retention");
    assert_eq!(row["duration_seconds"], 1.75);
}

#[test]
fn mutants_are_ordered_and_named_independently_of_their_line() {
    let rows = report_rows(vec![
        mutant("MissedMutant", 90, "b"),
        mutant("CaughtMutant", 10, "a"),
    ])
    .unwrap();
    assert_eq!(rows[0]["mutated_text"], "a");
    let moved = report_rows(vec![mutant("CaughtMutant", 500, "a")]).unwrap();
    assert_eq!(rows[0]["id"], moved[0]["id"]);
    let repeats = report_rows(vec![
        mutant("CaughtMutant", 1, "a"),
        mutant("CaughtMutant", 2, "a"),
    ])
    .unwrap();
    assert_ne!(repeats[0]["id"], repeats[1]["id"]);
}

#[test]
fn a_mutant_outside_any_function_is_still_named() {
    let mut bare = mutant("CaughtMutant", 1, "x");
    bare["scenario"]["Mutant"]
        .as_object_mut()
        .unwrap()
        .remove("function");
    assert_eq!(
        report_rows(vec![bare]).unwrap()[0]["original_text"],
        "<file>"
    );
}

#[test]
fn a_scenario_that_is_neither_baseline_nor_mutant_is_refused() {
    let error = report_rows(vec![
        json!({"scenario": {"Other": 1}, "summary": "Success"}),
    ]);
    assert!(error.unwrap_err().contains("unknown scenario"));
    let error = report_rows(vec![json!({"summary": "Success"})]);
    assert!(error.unwrap_err().contains("unknown scenario"));
}

#[test]
fn a_mutant_with_no_span_or_field_is_refused() {
    let mut unplaced = mutant("CaughtMutant", 1, "x");
    unplaced["scenario"]["Mutant"]["span"] = json!({});
    assert_eq!(
        report_rows(vec![unplaced]).unwrap_err(),
        "cargo-mutants reported a mutant with no span"
    );
    let mut unnamed = mutant("CaughtMutant", 1, "x");
    unnamed["scenario"]["Mutant"]
        .as_object_mut()
        .unwrap()
        .remove("genre");
    assert_eq!(
        report_rows(vec![unnamed]).unwrap_err(),
        "cargo-mutants reported a mutant with no genre"
    );
}

#[test]
fn a_red_or_absent_baseline_stops_the_run() {
    let clean = json!({"outcomes": [baseline("Success"), mutant("CaughtMutant", 1, "x")]});
    require_baseline(&clean).unwrap();
    let red = json!({"outcomes": [baseline("Failure")]});
    assert_eq!(
        require_baseline(&red).unwrap_err(),
        "cargo-mutants' unmutated baseline failed: Failure"
    );
    let silent = json!({"outcomes": [{"scenario": "Baseline"}]});
    assert_eq!(
        require_baseline(&silent).unwrap_err(),
        "cargo-mutants' unmutated baseline failed: None"
    );
    let absent = json!({"outcomes": [mutant("CaughtMutant", 1, "x")]});
    assert_eq!(
        require_baseline(&absent).unwrap_err(),
        "cargo-mutants reported no baseline scenario"
    );
}

fn exited(returncode: Option<i32>) -> CommandResult {
    CommandResult {
        returncode,
        timed_out: false,
        duration_seconds: 0.0,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
    }
}

#[test]
fn only_a_verdict_exit_is_a_measurement() {
    for code in [0, 2, 3] {
        require_verdict_exit(&exited(Some(code))).unwrap();
    }
    for code in [1, 4, 70, -9] {
        assert_eq!(
            require_verdict_exit(&exited(Some(code))).unwrap_err(),
            format!("cargo-mutants failed (rc={code})")
        );
    }
    assert_eq!(
        require_verdict_exit(&exited(None)).unwrap_err(),
        "cargo-mutants failed (rc=None)"
    );
}

#[test]
fn the_invocation_pins_every_bound_from_the_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let subject = bounded(
        dir.path(),
        json!({"generator": cargo_generator(json!({"package": "retention"}))}),
    );
    let argv = engine_argv(&subject, "/bin/cargo-mutants", Path::new("/out")).unwrap();
    assert_eq!(
        argv,
        [
            "/bin/cargo-mutants",
            "mutants",
            "--manifest-path",
            "tools/Cargo.toml",
            "--jobs",
            "2",
            "--timeout",
            "11",
            "--output",
            "/out",
            "--package",
            "retention",
            "--file",
            "src/lib.rs",
            "--exclude",
            "src/gen.rs",
        ]
    );
}

#[test]
fn a_crate_without_a_package_is_not_narrowed_to_one() {
    let dir = tempfile::tempdir().unwrap();
    let subject = bounded(dir.path(), json!({"generator": cargo_generator(json!({}))}));
    let argv = engine_argv(&subject, "/bin/cargo-mutants", Path::new("/out")).unwrap();
    assert!(!argv.iter().any(|arg| arg == "--package"));
}

#[test]
fn an_unresolved_bound_or_absent_manifest_path_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let unbounded = campaign(dir.path(), json!({"generator": cargo_generator(json!({}))}));
    assert!(engine_argv(&unbounded, "/bin/cargo-mutants", Path::new("/out")).is_err());
    let unnamed = bounded(dir.path(), json!({}));
    let error = engine_argv(&unnamed, "/bin/cargo-mutants", Path::new("/out")).unwrap_err();
    assert!(error.contains("manifest_path"), "{error}");
}

#[test]
fn parallel_workers_may_not_share_one_build_directory() {
    let dir = tempfile::tempdir().unwrap();
    let shared = bounded(
        dir.path(),
        json!({
            "generator": cargo_generator(json!({})),
            "environment": {"CARGO_TARGET_DIR": "/shared"},
        }),
    );
    let error = engine_argv(&shared, "/bin/cargo-mutants", Path::new("/out")).unwrap_err();
    assert!(error.contains("CARGO_TARGET_DIR"), "{error}");
    let mut serial = shared;
    serial.jobs = 1;
    engine_argv(&serial, "/bin/cargo-mutants", Path::new("/out")).unwrap();
}

#[test]
fn an_absent_or_malformed_report_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let error = read_report(dir.path()).unwrap_err();
    assert!(error.contains("wrote no outcomes.json"), "{error}");
    let outcomes = dir.path().join("mutants.out/outcomes.json");
    fs::create_dir_all(outcomes.parent().unwrap()).unwrap();
    fs::write(&outcomes, "not json").unwrap();
    assert!(read_report(dir.path()).unwrap_err().contains("is not JSON"));
    fs::write(&outcomes, "[]").unwrap();
    assert!(read_report(dir.path())
        .unwrap_err()
        .contains("root must be a JSON object"));
    fs::write(&outcomes, "{\"outcomes\": []}").unwrap();
    assert_eq!(read_report(dir.path()).unwrap(), json!({"outcomes": []}));
}

#[test]
fn the_summary_carries_what_the_completion_check_reads() {
    let report = json!({"total_mutants": 3, "caught": 1, "missed": 1, "timeout": 0, "unviable": 0, "noise": 1});
    let rows = [json!({"outcome": ERROR}), json!({"outcome": KILLED})];
    assert_eq!(
        summary(&report, &rows),
        json!({
            "total_mutants": 3,
            "caught": 1,
            "missed": 1,
            "timeout": 0,
            "unviable": 0,
            "mutants_generated": 3,
            "errors": 1,
        })
    );
}
