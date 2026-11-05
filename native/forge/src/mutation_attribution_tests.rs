use super::*;
use crate::mutation_run_test_support::{campaign, with_session, write, Delegated};
use crate::mutation_score::Receipt;

const NODE: &str = "tests/test_a.py::test_x";
const PASSING: &str = "<testcase classname=\"tests.test_a\" name=\"test_x\"/>";
const FAILING: &str =
    "<testcase classname=\"tests.test_a\" name=\"test_x\"><failure message=\"no\"/></testcase>";

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn killed(id: &str, tests: &[&str]) -> Value {
    json!({
        "id": id,
        "outcome": KILLED,
        "path": "src/a.py",
        "byte_offset": 4,
        "byte_length": 1,
        "original_text": "1",
        "mutated_text": "2",
        "tests_run": tests,
    })
}

/// A `pytest` read by `/bin/sh`: it reports `mutated` while the mutant is applied
/// and `clean` otherwise. Shell fragments, so a case can also hang or crash.
fn stub(dir: &Path, clean: &str, mutated: &str) -> String {
    let script = format!(
        "for arg; do case \"$arg\" in --junitxml=*) junit=${{arg#--junitxml=}};; esac; done\n\
         report() {{ printf '<testsuites><testsuite>%s</testsuite></testsuites>' \"$1\" > \"$junit\"; }}\n\
         if grep -q 'x = 2' src/a.py; then {mutated}; else {clean}; fi\n"
    );
    write(dir, "stub/pytest", &script);
    dir.join("stub/pytest").to_string_lossy().into_owned()
}

fn green() -> String {
    format!("report '{PASSING}'")
}

fn red() -> String {
    format!("report '{FAILING}'")
}

/// `attribute` inside a real test-owned sandbox and delegated cgroup session.
fn attributed(
    fixture: &Delegated,
    pytest: &str,
    timeout: i64,
    mutants: Vec<Value>,
) -> (Step<()>, Receipt) {
    let session = fixture.session();
    let result = attributed_with_session(fixture.path(), pytest, timeout, mutants, Some(&session));
    fixture.assert_ledger(&result.1);
    result
}
fn attributed_with_session(
    dir: &Path,
    pytest: &str,
    timeout: i64,
    mutants: Vec<Value>,
    session: Option<&crate::mutation_execution::CommandSession<'_>>,
) -> (Step<()>, Receipt) {
    write(dir, "src/a.py", "x = 1\n");
    with_session(
        dir,
        Some("/venv/bin/python"),
        json!({
            "generator": {"source": ["src/a.py"], "run_timeout_seconds": timeout},
            "test_argv": ["/bin/sh", pytest],
        }),
        session,
        |run| {
            run.receipt.insert("mutants".into(), Value::Array(mutants));
            let outcome = attribute(run, &BTreeMap::new());
            (outcome, run.receipt.clone())
        },
    )
}

#[test]
fn a_function_nodeid_is_what_junit_can_be_matched_back_to() {
    assert_eq!(
        function("tests/a.py::test_x[1-2]").unwrap(),
        "tests/a.py::test_x"
    );
    assert_eq!(
        function("tests/a.py::Case::test_x").unwrap(),
        "tests/a.py::Case::test_x"
    );
    assert_eq!(function("doctest::a.b"), None);
    assert_eq!(function("tests/a.py"), None);
}

#[test]
fn tests_are_ranked_by_what_junit_can_name() {
    let rows = [
        killed("a", &["tests/b.py::t[1]", "tests/b.py::t[2]", "doctest::a"]),
        killed("b", &["tests/a.py::t"]),
        json!({"id": "c"}),
    ];
    let (usable, unusable) = ranked(&rows);
    assert_eq!(usable, ["tests/a.py::t", "tests/b.py::t"]);
    assert_eq!(unusable, ["doctest::a"]);
}

#[test]
fn narrowing_cuts_early_exits_and_the_campaigns_own_selection() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "tests/test_a.py", "");
    let argv = strings(&[
        "python",
        "-m",
        "pytest",
        "-x",
        "--exitfirst",
        "--maxfail=1",
        "--maxfail",
        "2",
        "-k",
        "expr",
        "-m",
        "slow",
        "tests/test_a.py",
        "tests/test_a.py::test_x",
        "-q",
        "--tb=short",
        "-p",
        "plugin",
    ]);
    assert_eq!(
        narrowed(&argv, dir.path()).unwrap(),
        ["python", "-m", "pytest", "-q", "--tb=short", "-p", "plugin"]
    );
}

#[test]
fn narrowing_recognises_pytest_by_its_basename_and_refuses_anything_else() {
    let dir = tempfile::tempdir().unwrap();
    let argv = strings(&["/venv/bin/pytest", "-q"]);
    assert_eq!(narrowed(&argv, dir.path()).unwrap(), argv);
    let error = narrowed(&strings(&["cargo", "test"]), dir.path()).unwrap_err();
    assert_eq!(
        error,
        "attribution needs a pytest test command; campaign runs ['cargo', 'test']"
    );
}

#[test]
fn a_selection_pins_the_rootdir_and_reports_junit_last() {
    let dir = tempfile::tempdir().unwrap();
    let argv = strings(&["pytest", "-q"]);
    let nodes = strings(&[NODE]);
    let selected = selection(&argv, dir.path(), &nodes, Path::new("/r/j.xml")).unwrap();
    assert_eq!(
        selected,
        [
            "pytest".to_string(),
            "-q".to_string(),
            format!("--rootdir={}", dir.path().display()),
            NODE.to_string(),
            "--junitxml=/r/j.xml".to_string(),
        ]
    );
    let rooted = strings(&["pytest", "--rootdir=/elsewhere"]);
    let selected = selection(&rooted, dir.path(), &nodes, Path::new("/j")).unwrap();
    assert_eq!(
        selected
            .iter()
            .filter(|arg| arg.starts_with("--rootdir"))
            .count(),
        1
    );
}

#[test]
fn a_campaign_that_sets_its_own_junit_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    for flag in ["--junitxml=/mine.xml", "--junitxml"] {
        let argv = strings(&["pytest", flag]);
        assert_eq!(
            selection(&argv, dir.path(), &[], Path::new("/j")).unwrap_err(),
            "baseline.argv must not set --junitxml"
        );
    }
}

#[test]
fn the_mutant_is_applied_byte_exactly_and_the_source_and_bytecode_restored() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/a.py", "x = 1\n");
    write(dir.path(), "src/__pycache__/a.cpython-312.pyc", "stale");
    write(dir.path(), "src/__pycache__/b.cpython-312.pyc", "other");
    let target = dir.path().join("src/a.py");
    let stale = dir.path().join("src/__pycache__/a.cpython-312.pyc");
    let seen = with_mutant(dir.path(), &killed("m", &[]), || {
        assert!(
            !stale.exists(),
            "stale bytecode would run instead of the mutant"
        );
        Ok(fs::read_to_string(&target).unwrap())
    })
    .unwrap();
    assert_eq!(seen, "x = 2\n");
    assert_eq!(fs::read_to_string(&target).unwrap(), "x = 1\n");
    assert!(dir
        .path()
        .join("src/__pycache__/b.cpython-312.pyc")
        .exists());
}

#[test]
fn a_failing_body_still_restores_the_source() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/a.py", "x = 1\n");
    let outcome: Step<()> = with_mutant(dir.path(), &killed("m", &[]), || Err("boom".into()));
    assert_eq!(outcome, Err(Stop::Refused("boom".into())));
    assert_eq!(
        fs::read_to_string(dir.path().join("src/a.py")).unwrap(),
        "x = 1\n"
    );
}

#[test]
fn a_mutant_that_no_longer_matches_is_refused_and_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/a.py", "x = 9\n");
    let outcome: Step<()> = with_mutant(dir.path(), &killed("m", &[]), || {
        panic!("the body must not run")
    });
    assert_eq!(
        outcome,
        Err(Stop::Refused(
            "mutant m does not match src/a.py at byte 4: found b'9', expected b'1'".into()
        ))
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("src/a.py")).unwrap(),
        "x = 9\n"
    );
}

#[test]
fn an_insertion_past_the_end_of_the_file_is_refused_rather_than_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "src/a.py", "x\n");
    let mut row = killed("m", &[]);
    row["byte_offset"] = json!(50);
    row["byte_length"] = json!(0);
    row["original_text"] = json!("");
    let outcome: Step<()> = with_mutant(dir.path(), &row, || panic!("the body must not run"));
    assert!(matches!(outcome, Err(Stop::Refused(message)) if message.contains("does not match")));
}

#[test]
fn a_rerun_limit_is_capped_by_the_campaign_and_floored_against_noise() {
    assert_eq!(rerun_timeout(1800, 0.1), 60);
    assert_eq!(rerun_timeout(1800, 10.01), 101);
    assert_eq!(rerun_timeout(1800, 500.0), 1800);
    assert_eq!(rerun_timeout(30, 0.1), 30);
}

#[test]
fn one_contract_spans_the_campaign_because_kills_cross_files() {
    let dir = tempfile::tempdir().unwrap();
    let subject = campaign(dir.path(), json!({}));
    let built = spec(
        &subject,
        &strings(&[NODE]),
        &strings(&["src/a.py"]),
        &strings(&["m1", "m2"]),
    );
    assert_eq!(built["adapter"], "pytest-junit");
    assert_eq!(built["baseline_repetitions"], 2);
    assert_eq!(built["contracts"][0]["id"], "fixture");
    assert_eq!(built["contracts"][0]["criticality"], "high");
    assert_eq!(built["contracts"][0]["active_paths"], json!(["src/a.py"]));
    assert_eq!(built["tests"][0]["nodeid"], NODE);
    assert_eq!(built["tests"][0]["intentional_redundancy"], false);
    assert_eq!(
        built["mutation_contracts"],
        json!([["m1", "fixture"], ["m2", "fixture"]])
    );
}

#[test]
#[ignore = "requires strict sandbox and explicitly delegated FORGE_TEST_CGROUP_PARENT"]
fn a_kill_is_charged_to_the_test_that_failed() {
    let dir = Delegated::new(Some("/venv/bin/python"));
    let pytest = stub(dir.path(), &green(), &red());
    let (outcome, receipt) = attributed(&dir, &pytest, 30, vec![killed("m", &[NODE])]);
    outcome.unwrap();
    let summary = &receipt["attribution"];
    assert_eq!(summary["status"], "ATTRIBUTED");
    assert_eq!(summary["schema_version"], ATTRIBUTION_SCHEMA);
    assert_eq!(summary["baseline_repetitions"], 2);
    assert_eq!(summary["ranked_tests"], 1);
    assert_eq!(summary["killed_mutants"], 1);
    assert_eq!(summary["attributed_mutants"], 1);
    assert_eq!(summary["unattributed"], json!([]));
    assert!(
        receipt["test_value"].is_object(),
        "{:?}",
        receipt["test_value"]
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("src/a.py")).unwrap(),
        "x = 1\n"
    );
}

fn unattributed_reason(dir: &Delegated, mutated: &str, timeout: i64) -> Value {
    let pytest = stub(dir.path(), &green(), mutated);
    let (outcome, receipt) = attributed(dir, &pytest, timeout, vec![killed("m", &[NODE])]);
    outcome.unwrap();
    assert_eq!(receipt["attribution"]["status"], "NO_ATTRIBUTION");
    assert_eq!(receipt["attribution"]["attributed_mutants"], 0);
    assert!(!receipt.contains_key("test_value"));
    assert_eq!(
        fs::read_to_string(dir.path().join("src/a.py")).unwrap(),
        "x = 1\n"
    );
    receipt["attribution"]["unattributed"].clone()
}

#[test]
#[ignore = "requires strict sandbox and explicitly delegated FORGE_TEST_CGROUP_PARENT"]
fn a_mutant_that_aborts_the_interpreter_is_recorded_not_charged() {
    let dir = Delegated::new(Some("/venv/bin/python"));
    assert_eq!(
        unattributed_reason(&dir, "exit 134", 30),
        json!([{"id": "m", "reason": "covering set crashed without a report"}])
    );
}

#[test]
#[ignore = "requires strict sandbox and explicitly delegated FORGE_TEST_CGROUP_PARENT"]
fn a_mutant_its_covering_set_does_not_kill_again_is_recorded_not_charged() {
    let dir = Delegated::new(Some("/venv/bin/python"));
    assert_eq!(
        unattributed_reason(&dir, &green(), 30),
        json!([{"id": "m", "reason": "covering set passed on re-run"}])
    );
}

#[test]
#[ignore = "requires strict sandbox and explicitly delegated FORGE_TEST_CGROUP_PARENT"]
fn a_mutant_that_hangs_is_recorded_not_charged() {
    let dir = Delegated::new(Some("/venv/bin/python"));
    assert_eq!(
        unattributed_reason(&dir, "exec sleep 30", 1),
        json!([{"id": "m", "reason": "covering set timed out"}])
    );
}

#[test]
#[ignore = "requires strict sandbox and explicitly delegated FORGE_TEST_CGROUP_PARENT"]
fn a_report_that_cannot_be_mapped_charges_no_test() {
    let dir = Delegated::new(Some("/venv/bin/python"));
    let stranger = "report '<testcase classname=\"elsewhere\" name=\"test_y\"/>'";
    assert_eq!(
        unattributed_reason(&dir, stranger, 30),
        json!([{"id": "m", "reason": "report does not map to nodeids"}])
    );
}

#[test]
#[ignore = "requires strict sandbox and explicitly delegated FORGE_TEST_CGROUP_PARENT"]
fn a_kill_with_no_usable_covering_test_is_recorded_beside_those_with_one() {
    let dir = Delegated::new(Some("/venv/bin/python"));
    let pytest = stub(dir.path(), &green(), &red());
    let rows = vec![killed("a", &["doctest::a"]), killed("b", &[NODE])];
    let (outcome, receipt) = attributed(&dir, &pytest, 30, rows);
    outcome.unwrap();
    let summary = &receipt["attribution"];
    assert_eq!(summary["status"], "ATTRIBUTED");
    assert_eq!(summary["attributed_mutants"], 1);
    assert_eq!(summary["unusable_nodeids"], json!(["doctest::a"]));
    assert_eq!(
        summary["unattributed"],
        json!([{"id": "a", "reason": "no usable covering test"}])
    );
}

#[test]
#[ignore = "requires strict sandbox and explicitly delegated FORGE_TEST_CGROUP_PARENT"]
fn a_baseline_with_no_report_stops_attribution_and_keeps_the_summary() {
    let dir = Delegated::new(Some("/venv/bin/python"));
    let pytest = stub(dir.path(), "exit 3", &red());
    let (outcome, receipt) = attributed(&dir, &pytest, 30, vec![killed("m", &[NODE])]);
    assert_eq!(
        outcome,
        Err(Stop::Refused(
            "attribution baseline 0 exited 3 without a JUnit report".into()
        ))
    );
    assert_eq!(receipt["attribution"]["status"], "NOT_ATTEMPTED");
}

#[test]
#[ignore = "requires strict sandbox and explicitly delegated FORGE_TEST_CGROUP_PARENT"]
fn a_baseline_that_times_out_stops_attribution() {
    let dir = Delegated::new(Some("/venv/bin/python"));
    let pytest = stub(dir.path(), "exec sleep 30", &red());
    let (outcome, _) = attributed(&dir, &pytest, 1, vec![killed("m", &[NODE])]);
    assert_eq!(
        outcome,
        Err(Stop::Refused("attribution baseline 0 timed out".into()))
    );
}

#[test]
fn nothing_is_attempted_without_a_kill_or_a_test_junit_can_name() {
    let dir = tempfile::tempdir().unwrap();
    let pytest = stub(dir.path(), "exit 9", "exit 9");
    let mut survivor = killed("m", &[NODE]);
    survivor["outcome"] = json!("SURVIVED");
    let (outcome, receipt) = attributed_with_session(dir.path(), &pytest, 30, vec![survivor], None);
    outcome.unwrap();
    assert_eq!(receipt["attribution"]["status"], "NOT_ATTEMPTED");
    assert_eq!(receipt["attribution"]["reason"], "no mutant was killed");
    let (outcome, receipt) = attributed_with_session(
        dir.path(),
        &pytest,
        30,
        vec![killed("m", &["doctest::a"])],
        None,
    );
    outcome.unwrap();
    assert_eq!(receipt["attribution"]["status"], "NOT_ATTEMPTED");
    assert_eq!(
        receipt["attribution"]["reason"],
        "the coverage map names no usable pytest nodeid"
    );
}
