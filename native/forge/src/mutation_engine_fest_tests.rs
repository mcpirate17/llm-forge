use super::*;
use crate::mutation_run_test_support::{campaign, write};

const WORKTREE: &str = "/tmp/fixture-worktree";

fn mutant(offset: u64, line: u64) -> Value {
    json!({
        "file_path": format!("{WORKTREE}/src/a.py"),
        "line": line,
        "column": 1,
        "byte_offset": offset,
        "byte_length": 4,
        "original_text": "\"gh\"",
        "mutated_text": "\"\"",
        "mutator_name": "constant_replace",
    })
}

fn result(status: &str, offset: u64, line: u64) -> Value {
    json!({
        "mutant": mutant(offset, line),
        "status": status,
        "tests_run": [],
        "duration": {"secs": 0, "nanos": 1_000_000},
    })
}

fn report_rows(results: Vec<Value>) -> Result<Vec<Value>, String> {
    rows(&json!({"results": results}), Path::new(WORKTREE))
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn rows_are_ordered_by_position_so_repeats_name_stably() {
    let rows = report_rows(vec![result("Survived", 200, 9), result("Killed", 100, 4)]).unwrap();
    assert_eq!(rows[0]["outcome"], KILLED);
    assert_eq!(rows[1]["outcome"], SURVIVED);
    assert_ne!(rows[0]["id"], rows[1]["id"]);
    let moved = report_rows(vec![result("Killed", 150, 40), result("Survived", 250, 90)]).unwrap();
    assert_eq!(
        rows[0]["id"], moved[0]["id"],
        "an id does not move with the line"
    );
    assert_eq!(rows[1]["id"], moved[1]["id"]);
}

#[test]
fn every_field_a_row_carries_is_pinned() {
    let mut reported = result("Killed", 100, 4);
    reported["tests_run"] = json!(["tests/test_a.py::test_x"]);
    let rows = report_rows(vec![reported]).unwrap();
    let row = rows[0].as_object().unwrap();
    assert_eq!(
        row.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "id",
            "outcome",
            "path",
            "line",
            "byte_offset",
            "byte_length",
            "operator",
            "original_text",
            "mutated_text",
            "tests_run",
            "duration_seconds",
        ]
    );
    assert_eq!(row["path"], "src/a.py");
    assert_eq!(row["line"], 4);
    assert_eq!(row["byte_offset"], 100);
    assert_eq!(row["byte_length"], 4);
    assert_eq!(row["operator"], "constant_replace");
    assert_eq!(row["original_text"], "\"gh\"");
    assert_eq!(row["mutated_text"], "\"\"");
    assert_eq!(row["tests_run"], json!(["tests/test_a.py::test_x"]));
    assert_eq!(row["duration_seconds"], 0.001);
}

#[test]
fn every_status_is_mapped_and_none_is_guessed() {
    for (status, expected) in [
        ("Killed", KILLED),
        ("Survived", SURVIVED),
        ("Timeout", TIMED_OUT),
        ("NoCoverage", NO_COVERAGE),
        ("Error", ERROR),
    ] {
        assert_eq!(
            report_rows(vec![result(status, 1, 1)]).unwrap()[0]["outcome"],
            expected
        );
    }
    let error = report_rows(vec![result("Skipped", 1, 1)]).unwrap_err();
    assert_eq!(error, "fest reported unknown status 'Skipped'");
    let mut silent = result("Killed", 1, 1);
    silent.as_object_mut().unwrap().remove("status");
    assert_eq!(
        report_rows(vec![silent]).unwrap_err(),
        "fest reported unknown status ''"
    );
}

#[test]
fn a_mutant_missing_a_field_is_refused() {
    for key in [
        "line",
        "byte_length",
        "original_text",
        "mutated_text",
        "mutator_name",
    ] {
        let mut reported = result("Killed", 1, 1);
        reported["mutant"].as_object_mut().unwrap().remove(key);
        let error = report_rows(vec![reported]).unwrap_err();
        assert_eq!(error, format!("fest reported a mutant with no {key}"));
    }
    let mut unplaced = result("Killed", 1, 1);
    unplaced["mutant"]["byte_offset"] = json!("one");
    assert_eq!(
        report_rows(vec![unplaced]).unwrap_err(),
        "fest reported a non-integer byte_offset"
    );
}

#[test]
fn null_tests_read_as_none_and_a_report_without_results_has_no_rows() {
    let mut reported = result("NoCoverage", 1, 1);
    reported["tests_run"] = Value::Null;
    assert_eq!(
        report_rows(vec![reported]).unwrap()[0]["tests_run"],
        json!([])
    );
    assert_eq!(
        rows(&json!({}), Path::new(WORKTREE)).unwrap(),
        Vec::<Value>::new()
    );
}

#[test]
fn a_duration_is_recorded_in_seconds_to_microseconds() {
    assert_eq!(
        seconds(Some(&json!({"secs": 2, "nanos": 500_000_000}))),
        2.5
    );
    assert_eq!(seconds(Some(&json!({"nanos": 500_000_000}))), 0.5);
    assert_eq!(seconds(Some(&json!({"secs": 2}))), 2.0);
    assert_eq!(seconds(None), 0.0);
    let mut reported = result("Killed", 1, 1);
    reported["duration"] = json!({"secs": 1, "nanos": 123_456_789});
    assert_eq!(
        report_rows(vec![reported]).unwrap()[0]["duration_seconds"],
        1.123_457
    );
}

#[test]
fn coverage_is_measured_over_directories_never_a_single_file() {
    assert_eq!(
        coverage_targets(&strings(&[
            "src/pkg/a.py",
            "src/pkg/b.py",
            "src/other/**/*.py",
            "c.py"
        ])),
        ["src/pkg", "src/other", "."]
    );
    assert_eq!(
        coverage_targets(&strings(&["a.py", "b.py"])),
        ["."],
        "one root, once"
    );
}

#[test]
fn the_baseline_is_the_campaigns_own_command_instrumented() {
    let dir = tempfile::tempdir().unwrap();
    let subject = campaign(dir.path(), json!({}));
    assert_eq!(
        coverage_argv(&subject, "/venv/bin/python"),
        strings(&[
            "/venv/bin/python",
            "-m",
            "pytest",
            "tests/test_a.py",
            "--cov=src",
            "--cov-context=test",
            "--cov-fail-under=0",
            "--cov-report=",
        ])
    );
}

#[test]
fn the_invocation_uses_our_coverage_map_and_the_declared_operators() {
    let dir = tempfile::tempdir().unwrap();
    let subject = campaign(
        dir.path(),
        json!({"generator": {
            "source": ["src/a.py"],
            "run_timeout_seconds": 60,
            "operators": ["arith", "compare"],
        }}),
    );
    assert_eq!(
        engine_argv(&subject, "/venv/bin/fest", Path::new("/w/.coverage")),
        strings(&[
            "/venv/bin/fest",
            "run",
            "--progress",
            "quiet",
            "--output",
            "json",
            "--filter-operators",
            "arith",
            "--filter-operators",
            "compare",
            "--coverage-from",
            "/w/.coverage",
        ])
    );
}

#[test]
fn the_config_carries_the_bound_the_run_resolved() {
    let dir = tempfile::tempdir().unwrap();
    let mut subject = campaign(dir.path(), json!({}));
    assert!(
        config(&subject, "/venv/bin/python").is_err(),
        "no bound resolved yet"
    );
    subject.mutant_timeout_seconds = Some(7);
    let body = config(&subject, "/venv/bin/python").unwrap();
    let parsed: toml::Value = toml::from_str(&body).unwrap();
    let fest = &parsed["fest"];
    assert_eq!(fest["timeout"].as_integer(), Some(7));
    assert_eq!(fest["seed"].as_integer(), Some(subject.seed));
    assert_eq!(fest["workers"].as_integer(), Some(subject.jobs));
    assert_eq!(fest["output"].as_str(), Some("json"));
    assert_eq!(fest["backend"].as_str(), Some("subprocess"));
    assert_eq!(fest["source"][0].as_str(), Some("src/a.py"));
    assert_eq!(fest["test_command"][0].as_str(), Some("/venv/bin/python"));
}

#[test]
fn the_environment_binds_the_run_to_this_venv_and_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = dir.path().join("worktree");
    write(&worktree, "src/a.py", "");
    let subject = campaign(
        dir.path(),
        json!({"environment": {
            "PYTHONPATH": "/declared",
            "PYTEST_ADDOPTS": " -q ",
            "KEPT": "1",
        }}),
    );
    let bound = environment(&subject, &worktree, "/venv/bin/python");
    let root = worktree.to_string_lossy();
    assert_eq!(bound["PYTHONPATH"], format!("{root}:{root}/src:/declared"));
    assert_eq!(bound["VIRTUAL_ENV"], "/venv");
    assert!(bound["PATH"].starts_with("/venv/bin:"));
    assert_eq!(bound["PYTEST_ADDOPTS"], format!("-q -p {PLUGIN_NAME}"));
    assert_eq!(bound[SCRATCH_ENV], format!("{root}/.bytecode-isolation"));
    assert_eq!(bound[SOURCES_ENV], format!("{root}/src/a.py"));
    assert_eq!(bound["KEPT"], "1");
}

#[test]
fn every_child_loads_the_eviction_plugin_without_a_declared_addopts() {
    let dir = tempfile::tempdir().unwrap();
    let subject = campaign(dir.path(), json!({}));
    let bound = environment(&subject, dir.path(), "/venv/bin/python");
    assert_eq!(bound["PYTEST_ADDOPTS"], format!("-p {PLUGIN_NAME}"));
}

#[test]
fn fest_is_found_beside_the_interpreter_first() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let fest = dir.path().join("fest");
    fs::write(&fest, "").unwrap();
    fs::set_permissions(&fest, fs::Permissions::from_mode(0o755)).unwrap();
    let interpreter = dir.path().join("python");
    assert_eq!(
        binary(&interpreter.to_string_lossy()).unwrap(),
        fest.to_string_lossy()
    );
}

#[test]
fn the_summary_keeps_only_the_totals_a_receipt_reads() {
    let picked = summary(&json!({"mutants_generated": 3, "killed": 2, "noise": true}));
    assert_eq!(picked, json!({"mutants_generated": 3, "killed": 2}));
}

fn exited(returncode: Option<i32>, timed_out: bool) -> CommandResult {
    CommandResult {
        returncode,
        timed_out,
        duration_seconds: 0.0,
        stdout_tail: String::new(),
        stderr_tail: "boom".to_string(),
    }
}

#[test]
fn only_a_clean_exit_is_a_measurement() {
    assert_eq!(require_clean_exit(&exited(Some(0), false)), Ok(()));
    assert_eq!(
        require_clean_exit(&exited(Some(1), false)).unwrap_err(),
        "fest failed (rc=1): boom"
    );
    assert_eq!(
        require_clean_exit(&exited(None, false)).unwrap_err(),
        "fest failed (rc=None): boom"
    );
    assert!(require_clean_exit(&exited(Some(0), true)).is_err());
}

#[test]
fn fest_requires_a_guest_and_keeps_pinned_python_commands_and_environment() {
    let dir = tempfile::tempdir().unwrap();
    crate::mutation_run_test_support::with_run(dir.path(), None, json!({}), |run| {
        assert!(
            matches!(execute(run, "/no/fest"), Err(crate::mutation_campaign::Stop::Refused(message))
            if message.contains("requires a Python guest"))
        );
    });
    crate::mutation_run_test_support::with_run(
        dir.path(),
        Some("/selected/bin/python"),
        json!({}),
        |run| {
            let python = run.require_python().unwrap();
            assert_eq!(coverage_argv(run.campaign, python)[0], python);
            let bound = run.guest_environment(&environment(run.campaign, dir.path(), python));
            assert_eq!(bound["CONDUCTOR_SNAPSHOT_PYTHON"], python);
            assert_eq!(bound["VIRTUAL_ENV"], "/selected");
            assert!(bound["PATH"].starts_with("/selected/bin:"));
            assert!(bound["PYTEST_ADDOPTS"].contains(PLUGIN_NAME));
        },
    );
}

/// Synthetic shell protocol exercises the adapter through the confined actor;
/// these fixture results are not mutation campaign evidence.
fn delegated_fest_failure(coverage: bool) {
    let fixture = crate::mutation_run_test_support::Delegated::new(Some("/venv/bin/python"));
    let root = fixture.path();
    write(root, "src/a.py", "x = 1\n");
    write(root, "tests/test_a.py", "def test_a(): pass\n");
    // `fest` is /bin/sh; its first generated argument is `run`, so the shell
    // reads this file in the owned worktree. No external test plugin is needed.
    write(
        root,
        "run",
        "printf 'not-json'\nprintf ran > engine-marker\n",
    );
    let source = crate::mutation_campaign::sha256_path(&root.join("src/a.py")).unwrap();
    let test = crate::mutation_campaign::sha256_path(&root.join("tests/test_a.py")).unwrap();
    let baseline = if coverage { ": > .coverage" } else { ":" };
    fixture.with_run(
        Some("/venv/bin/python"),
        json!({
            "source_sha256": {"src/a.py": source},
            "test_sha256": {"tests/test_a.py": test},
            "test_argv": ["/bin/sh", "-c", baseline],
        }),
        |run| {
            let error = execute(run, "/bin/sh").unwrap_err();
            let detail = match error {
                crate::mutation_campaign::Stop::Refused(detail) => detail,
                other => panic!("expected explicit refusal: {other:?}"),
            };
            assert_eq!(run.receipt["baseline"]["returncode"], 0);
            assert!(!run.receipt.contains_key("mutants"));
            assert!(!run.receipt.contains_key("engine_summary"));
            let commands = run.receipt["process_commands"].as_array().unwrap();
            if coverage {
                assert!(detail.contains("fest produced no JSON report"), "{detail}");
                assert_eq!(commands.len(), 2);
                assert_eq!(
                    fs::read_to_string(root.join("engine-marker")).unwrap(),
                    "ran"
                );
                assert_eq!(run.receipt["engine_result"]["returncode"], 0);
            } else {
                assert!(
                    detail.contains("baseline produced no coverage database"),
                    "{detail}"
                );
                assert_eq!(commands.len(), 1);
                assert!(!root.join("engine-marker").exists());
                assert!(!run.receipt.contains_key("engine_result"));
            }
        },
    );
}

#[test]
#[ignore = "requires strict sandbox and explicitly delegated FORGE_TEST_CGROUP_PARENT"]
fn successful_baseline_without_coverage_refuses_before_fest() {
    delegated_fest_failure(false);
}

#[test]
#[ignore = "requires strict sandbox and explicitly delegated FORGE_TEST_CGROUP_PARENT"]
fn non_json_fest_output_refuses_after_clean_baseline() {
    delegated_fest_failure(true);
}
