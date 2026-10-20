#![cfg(feature = "python-compat-tests")]
//! Research-integrity and commit-attestation contracts, source cases 10–17 of 17.

#[path = "python_contracts/candidate_review_support.rs"]
#[allow(dead_code)]
mod candidate_review_support;
#[path = "python_contracts/candidate_runtime_support.rs"]
#[allow(dead_code)]
mod candidate_runtime_support;
#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use candidate_review_support as fixture;
use candidate_runtime_support as runtime;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use support::{module, path, AttrPatch};

const NUMERICAL_PROSE: &str = "# Memory measure\n\nThe ledger writes a score per candidate.\nIt runs on one device and rejects NaN inputs.\n\nSee `../OLD_REPORT.md` for the postmortem.\n";
const AFTER: &str = "2026-09-07T09:00:00+00:00";
const BEFORE: &str = "2026-09-01T09:00:00+00:00";
const LONG_BEFORE: &str = "2026-01-02T09:00:00+00:00";

fn research_repo(case: &support::Case, seed: &str, changed: &str) -> std::path::PathBuf {
    let repo = runtime::repo(case);
    runtime::write(&repo, "component_fab/notes.md", seed);
    fixture::commit_all(&repo, "seed");
    runtime::write(&repo, "component_fab/notes.md", changed);
    fixture::git(&repo, &["add", "--all"]);
    repo
}

#[test]
fn research_integrity_arms_on_changed_lines_not_the_whole_file() {
    let case = fixture::isolated_case();
    let changed = NUMERICAL_PROSE.replace("`../OLD_REPORT.md`", "`research/notes/report.md`");
    let repo = research_repo(&case, NUMERICAL_PROSE, &changed);
    let (rules, metrics) = Python::attach(|py| runtime::research_rules(py, &repo));
    assert_eq!(metrics["armed_on"], "changed-lines");
    assert!(rules.is_empty());
}

#[test]
fn research_integrity_still_fires_on_a_line_this_change_added() {
    let case = fixture::isolated_case();
    let changed = format!("{NUMERICAL_PROSE}\nThe reader now promotes on a float16 dtype.\n");
    let repo = research_repo(&case, NUMERICAL_PROSE, &changed);
    let (rules, metrics) = Python::attach(|py| runtime::research_rules(py, &repo));
    assert_eq!(metrics["armed_on"], "changed-lines");
    assert!(rules.contains(&"missing-numerical-device-tests".to_owned()));
    assert!(rules.contains(&"incomplete-result-provenance".to_owned()));
}

#[test]
fn research_integrity_reads_provenance_the_candidate_did_not_touch() {
    let case = fixture::isolated_case();
    let seed = "# Run\n\nbaseline: control\nseed: 11\nconfig: sweep.toml\nfingerprint: abc123\n";
    let repo = research_repo(
        &case,
        seed,
        &format!("{seed}\nThe promoted candidate is recorded.\n"),
    );
    let (rules, _) = Python::attach(|py| runtime::research_rules(py, &repo));
    assert!(!rules.contains(&"incomplete-result-provenance".to_owned()));
}

#[test]
fn research_integrity_falls_back_to_whole_files_when_the_diff_fails() {
    let case = fixture::isolated_case();
    let changed = NUMERICAL_PROSE.replace("`../OLD_REPORT.md`", "`research/notes/report.md`");
    let repo = research_repo(&case, NUMERICAL_PROSE, &changed);
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.git_source");
        let error = source
            .getattr("GitSourceError")
            .unwrap()
            .call1(("diff unavailable",))
            .unwrap();
        let explode = runtime::raise(py, &error);
        let _patch = runtime::set_module_attr(
            py,
            "conductor.candidate_review.checks",
            "changed_line_numbers",
            &explode,
        );
        let (rules, metrics) = runtime::research_rules(py, &repo);
        assert_eq!(metrics["armed_on"], "whole-files");
        assert!(metrics["diff_error"]
            .as_str()
            .unwrap()
            .contains("GitSourceError"));
        assert!(rules.contains(&"missing-numerical-device-tests".to_owned()));
        assert!(rules.contains(&"incomplete-result-provenance".to_owned()));
    });
}

#[test]
fn ci_attestation_flags_commits_with_no_agent_trailer() {
    let case = fixture::isolated_case();
    let repo = runtime::repo(&case);
    runtime::write(&repo, "seed.txt", "base\n");
    let base = runtime::dated_commit(&repo, "baseline", AFTER);
    runtime::write(&repo, "seed.txt", "one\n");
    let first = runtime::dated_commit(&repo, "feat(x): one", AFTER);
    runtime::write(&repo, "seed.txt", "two\n");
    let second = runtime::dated_commit(&repo, "feat(x): two", AFTER);
    let (evidence, findings) = Python::attach(|py| runtime::attestation(py, &repo, &base, &second));
    assert_eq!(
        evidence["unattributed_commits"],
        serde_json::json!([first, second])
    );
    let unattributed: Vec<_> = findings
        .iter()
        .filter(|row| row["rule_id"] == "commit-agent-unattributed")
        .collect();
    assert_eq!(unattributed.len(), 1);
    assert_eq!(unattributed[0]["severity"], "HIGH");
    assert_eq!(
        unattributed[0]["evidence"]["commits"],
        serde_json::json!([first, second])
    );
}

#[test]
fn ci_attestation_accepts_an_agent_trailer_beside_other_trailers() {
    let case = fixture::isolated_case();
    let repo = runtime::repo(&case);
    runtime::write(&repo, "seed.txt", "base\n");
    let base = runtime::dated_commit(&repo, "baseline", AFTER);
    runtime::write(&repo, "seed.txt", "one\n");
    let tip = runtime::dated_commit(
        &repo,
        "feat(x): one\n\nAgent: llm-04\nCo-Authored-By: Someone <s@example.invalid>\n",
        AFTER,
    );
    let (evidence, findings) = Python::attach(|py| runtime::attestation(py, &repo, &base, &tip));
    assert_eq!(evidence["unattributed_commits"], serde_json::json!([]));
    assert!(findings
        .iter()
        .all(|row| row["rule_id"] != "commit-agent-unattributed"));
}

#[test]
fn ci_attestation_exempts_commits_written_before_the_rule() {
    let case = fixture::isolated_case();
    let repo = runtime::repo(&case);
    runtime::write(&repo, "seed.txt", "base\n");
    let base = runtime::dated_commit(&repo, "baseline", BEFORE);
    runtime::write(&repo, "seed.txt", "one\n");
    runtime::dated_commit(&repo, "feat(x): predates the rule", BEFORE);
    runtime::write(&repo, "seed.txt", "two\n");
    let head = runtime::dated_commit(&repo, "feat(x): after the rule", AFTER);
    let (evidence, _) = Python::attach(|py| runtime::attestation(py, &repo, &base, &head));
    assert_eq!(evidence["commits_examined"], 2);
    assert_eq!(evidence["unattributed_commits"], serde_json::json!([head]));
}

#[test]
fn agent_trailer_requirement_fails_closed_on_an_unreadable_date() {
    let case = fixture::isolated_case();
    let repo = runtime::repo(&case);
    runtime::write(&repo, "seed.txt", "base\n");
    let head = runtime::dated_commit(&repo, "baseline", LONG_BEFORE);
    Python::attach(|py| {
        let engine = module(py, "conductor.candidate_review.engine");
        let required = engine.getattr("_agent_trailer_required").unwrap();
        assert!(!required
            .call1((path(py, &repo), &head))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("stdout", pyo3::types::PyBytes::new(py, b"not-a-date\n"))
            .unwrap();
        let garbage = module(py, "types")
            .getattr("SimpleNamespace")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let mock = runtime::constant(py, &garbage);
        let _patch = AttrPatch::replace(engine.as_any(), "run_git", &mock);
        assert!(required
            .call1((path(py, &repo), &head))
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

const BASELINE_CASE: &str = r#"
import json
import subprocess
from conductor.candidate_review import baseline_rerun as b
from conductor.candidate_review.model import Finding, Severity

def run(check, base_fails, raise_):
    def fake(ctx, chk, shard_tests):
        # The replay must get the failed shard's whole file list, in order.
        assert shard_tests == {0: ["t/a.py", "t/c.py", "t/b.py"]}, shard_tests
        if raise_:
            raise RuntimeError("boom")
        return {0: set(base_fails)}
    orig = b._base_failures
    b._base_failures = fake
    try:
        done = [
            subprocess.CompletedProcess([], 1, "FAILED t/a.py::x - boom\nERROR t/b.py\n", ""),
            subprocess.CompletedProcess([], 0, "", ""),
        ]
        fallback = [Finding(check_id=check.check_id, rule_id="targeted-test-failure",
                            severity=Severity.HIGH, message="shard 1 failed")]
        shards = [["t/a.py", "t/c.py", "t/b.py"], ["t/d.py"]]
        out = b.baseline_failure_findings(
            None, check, shards, done, [0], [True, True], fallback
        )
    finally:
        b._base_failures = orig
    return [(f.rule_id, f.inherited, f.message) for f in out]

def run_collection_error(check):
    def replay(ctx, chk, shard_tests):
        assert shard_tests == {0: ["t/a.py", "t/c.py", "t/b.py"]}, shard_tests
        output = (
            "FAILED t/a.py::x - boom\nERROR collecting t/b.py\n"
            "!!!!!!!!!!!!!!!! Interrupted: 1 error during collection !!!!!!!!!!!!!!!!\n"
            "1 error in 0.01s\n"
        )
        return {0: b._base_replay_node_ids(
            subprocess.CompletedProcess([], 2, output, ""), 1
        )}
    orig = b._base_failures
    b._base_failures = replay
    try:
        done = [
            subprocess.CompletedProcess([], 1, "FAILED t/a.py::x - boom\n", ""),
            subprocess.CompletedProcess([], 0, "", ""),
        ]
        fallback = [Finding(check_id=check.check_id, rule_id="targeted-test-failure",
                            severity=Severity.HIGH, message="shard 1 failed")]
        shards = [["t/a.py", "t/c.py", "t/b.py"], ["t/d.py"]]
        out = b.baseline_failure_findings(
            None, check, shards, done, [0], [True, True], fallback
        )
    finally:
        b._base_failures = orig
    finding = out[0]
    return (
        finding.rule_id,
        finding.inherited,
        finding.message,
        finding.evidence["base_replay"]["status"],
        json.dumps(finding.evidence["base_replay"]),
    )
"#;

const BASELINE_VERDICT_CASE: &str = r#"
import json
import subprocess
from conductor.candidate_review.baseline_rerun import (
    BaseReplayInconclusive,
    _base_replay_node_ids,
    _timed_out_shards,
)

def classify(code, stdout, stderr=""):
    try:
        ids = _base_replay_node_ids(
            subprocess.CompletedProcess([], code, stdout, stderr), 7
        )
        return ("valid", sorted(ids), "", "{}")
    except BaseReplayInconclusive as exc:
        return ("inconclusive", [], str(exc), json.dumps(exc.evidence))

def cases():
    return [
        classify(0, "1 passed in 0.01s\n"),
        classify(1, "FAILED tests/test_a.py::test_x - assertion\n1 failed in 0.01s\n"),
        classify(1, "FAILED tests/test_a.py::test_x - assertion\n"
                       "ERROR collecting tests/test_b.py\n"),
        classify(2, "ERROR collecting tests/test_b.py\n1 error in 0.01s\n"),
        classify(2, "!!!!!!!!!!!!!!!! Interrupted: 1 error during collection !!!!!!!!!!!!!!!!\n", "control stderr"),
        classify(3, "INTERNALERROR> runner crashed\n", "internal stderr"),
        classify(4, "ERROR: usage: pytest [options]\n", "usage stderr"),
        classify(5, "no tests ran in 0.01s\n", "empty collection stderr"),
        classify(1, "no tests ran in 0.01s\n"),
        classify(1, "FAILED tests/test_a.py::test_x - assertion\n1 passed in 0.01s\n"),
        classify(0, ""),
        classify(0, "FAILED tests/test_a.py::test_x - assertion\n1 failed in 0.01s\n"),
        classify(1, "E AssertionError: KeyboardInterrupt; Interrupted; no tests ran; "
                       "collected 0 items; ERROR collecting tests/test_data.py\n"
                       "FAILED tests/test_a.py::test_x - assertion\n1 failed in 0.01s\n"),
        classify(1, "ERROR tests/test_a.py::test_setup - fixture setup failed\n"
                       "1 error in 0.01s\n"),
        classify(1, "FAILED tests/test_a.py::test_x - assertion\n"
                       "1 error in 0.01s\n"),
        classify(1, "FAILED tests/test_a.py::test_x - assertion\n"
                       "================ 1 failed, 145 passed in 97.65s (0:01:37) ================\n"),
        classify(0, "0 passed in 0.01s\n"),
        classify(0, "1 warning in 0.01s\n"),
        classify(2, "x" * 1400 + "tail-marker\n", "diagnostic stderr tail\n"),
        classify(1, "FAILED tests/test_a.py::test_x - assertion\n"
                       "FAILED tests/test_b.py::test_y - assertion\n"
                       "2 failed, 192 passed, 61 skipped, 24 warnings, 8 subtests passed in 24.48s\n"),
        classify(1, "FAILED tests/test_a.py::test_x - assertion\n"
                       "2 subtests failed, 8 subtests passed in 0.5s\n"),
    ]

def timeout_mapping():
    # `execute_shards` reports command ordinal 0, while only original shard 3
    # survived base-tree filtering and was placed in the one-command list.
    return _timed_out_shards([2], [0])
"#;

fn baseline_outcome(base_fails: &[&str], raise: bool) -> Vec<(String, bool, String)> {
    Python::attach(|py| {
        let full = runtime::policy(py)
            .getattr("checks")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .find(|row| {
                row.getattr("check_id")
                    .unwrap()
                    .extract::<String>()
                    .unwrap()
                    == "targeted-tests-full"
            })
            .unwrap();
        let code = std::ffi::CString::new(BASELINE_CASE).unwrap();
        pyo3::types::PyModule::from_code(py, &code, c"baseline_case.py", c"baseline_case")
            .unwrap()
            .getattr("run")
            .unwrap()
            .call1((full, base_fails.to_vec(), raise))
            .unwrap()
            .extract()
            .unwrap()
    })
}

fn collection_error_outcome() -> (String, bool, String, String, String) {
    Python::attach(|py| {
        let full = runtime::policy(py)
            .getattr("checks")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .find(|row| {
                row.getattr("check_id")
                    .unwrap()
                    .extract::<String>()
                    .unwrap()
                    == "targeted-tests-full"
            })
            .unwrap();
        let code = std::ffi::CString::new(BASELINE_CASE).unwrap();
        pyo3::types::PyModule::from_code(py, &code, c"baseline_case.py", c"baseline_case")
            .unwrap()
            .getattr("run_collection_error")
            .unwrap()
            .call1((full,))
            .unwrap()
            .extract()
            .unwrap()
    })
}

#[test]
fn a_failure_that_passes_on_base_still_blocks() {
    let _case = fixture::isolated_case();
    let out = baseline_outcome(&["t/b.py"], false);
    assert_eq!(out.len(), 2);
    let summary = out[0].2.lines().next().unwrap();
    assert!(!out[0].1 && summary.contains("t/a.py::x") && !summary.contains("t/b.py"));
    assert!(out[1].1 && out[1].2.contains("t/b.py"));
}

#[test]
fn failures_already_red_on_base_are_inherited_only() {
    let _case = fixture::isolated_case();
    let out = baseline_outcome(&["t/a.py::x", "t/b.py"], false);
    assert_eq!(out.len(), 1);
    assert!(out[0].1);
}

#[test]
fn a_base_rerun_that_cannot_run_keeps_every_failure_blocking() {
    let _case = fixture::isolated_case();
    let out = baseline_outcome(&["t/a.py::x", "t/b.py"], true);
    assert_eq!(out.len(), 1);
    assert!(!out[0].1 && out[0].2.contains("base replay inconclusive"));
    assert!(out[0].2.contains("no base attribution was made"));
    assert!(out[0].2.contains("candidate finding remains blocking"));
}

#[test]
fn collection_errors_with_failed_nodes_do_not_create_base_attribution() {
    let _case = fixture::isolated_case();
    let (rule, inherited, message, replay_status, replay_evidence) = collection_error_outcome();
    assert_eq!(rule, "targeted-test-failure");
    assert!(!inherited);
    assert_eq!(replay_status, "inconclusive");
    assert!(message.contains("base replay inconclusive"));
    assert!(message.contains("no base attribution was made"));
    assert!(!message.contains("pass when their shard"));
    let evidence: serde_json::Value = serde_json::from_str(&replay_evidence).unwrap();
    assert_eq!(evidence["shards"][0]["shard_index"], 1);
    assert_eq!(evidence["shards"][0]["returncode"], 2);
    assert!(evidence["shards"][0]["stdout_tail"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    assert!(evidence["shards"][0]["stderr_tail"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
}

#[test]
fn base_replay_accepts_only_unambiguous_pytest_verdicts() {
    let _case = fixture::isolated_case();
    Python::attach(|py| {
        let code = std::ffi::CString::new(BASELINE_VERDICT_CASE).unwrap();
        let cases = pyo3::types::PyModule::from_code(
            py,
            &code,
            c"baseline_verdict_case.py",
            c"baseline_verdict_case",
        )
        .unwrap()
        .getattr("cases")
        .unwrap()
        .call0()
        .unwrap()
        .extract::<Vec<(String, Vec<String>, String, String)>>()
        .unwrap();

        assert_eq!(
            (
                cases[0].0.as_str(),
                cases[0].1.as_slice(),
                cases[0].2.as_str()
            ),
            ("valid", &[][..], "")
        );
        assert_eq!(
            (
                cases[1].0.as_str(),
                cases[1].1.as_slice(),
                cases[1].2.as_str()
            ),
            ("valid", &["tests/test_a.py::test_x".to_owned()][..], "")
        );
        for (index, exit_code) in [
            (2, 1),
            (3, 2),
            (4, 2),
            (5, 3),
            (6, 4),
            (7, 5),
            (8, 1),
            (9, 1),
            (10, 0),
            (11, 0),
            (14, 1),
            (16, 0),
            (17, 0),
            (18, 2),
        ] {
            assert_eq!(
                cases[index].0, "inconclusive",
                "exit {exit_code}: {:?}",
                cases[index]
            );
            assert!(!cases[index].2.is_empty());
        }
        assert!(cases[2]
            .2
            .contains("did not produce a pytest terminal test summary"));
        assert!(cases[3].2.contains("non-verdict pytest code 2"));
        assert!(cases[4].2.contains("non-verdict pytest code 2"));
        assert!(cases[5].2.contains("non-verdict pytest code 3"));
        assert!(cases[6].2.contains("non-verdict pytest code 4"));
        assert!(cases[7].2.contains("non-verdict pytest code 5"));
        assert!(cases[8]
            .2
            .contains("did not produce a pytest terminal test summary"));
        assert!(cases[9]
            .2
            .contains("without a consistent failed-test or setup-error summary"));
        assert!(cases[10]
            .2
            .contains("did not produce a pytest terminal test summary"));
        assert!(cases[11].2.contains("exited 0 with failure summaries"));
        assert_eq!(cases[12].0, "valid");
        assert_eq!(cases[12].1, vec!["tests/test_a.py::test_x"]);
        assert_eq!(cases[13].0, "valid");
        assert_eq!(cases[13].1, vec!["tests/test_a.py::test_setup".to_owned()]);
        assert!(cases[14]
            .2
            .contains("without a consistent failed-test or setup-error summary"));
        assert_eq!(cases[15].0, "valid");
        assert_eq!(cases[15].1, vec!["tests/test_a.py::test_x"]);
        assert!(cases[16].2.contains("for any collected tests"));
        assert!(cases[17].2.contains("for any collected tests"));

        assert_eq!(cases[19].0, "valid");
        assert_eq!(
            cases[19].1,
            vec!["tests/test_a.py::test_x", "tests/test_b.py::test_y"]
        );
        assert_eq!(cases[20].0, "inconclusive");
        assert!(cases[20].2.contains("for any collected tests"));

        let evidence: serde_json::Value = serde_json::from_str(&cases[18].3).unwrap();
        assert_eq!(evidence["status"], "inconclusive");
        assert_eq!(evidence["shards"][0]["shard_index"], 7);
        assert_eq!(evidence["shards"][0]["returncode"], 2);
        let stdout_tail = evidence["shards"][0]["stdout_tail"].as_str().unwrap();
        let stderr_tail = evidence["shards"][0]["stderr_tail"].as_str().unwrap();
        assert!(stdout_tail.len() <= 1000 && stdout_tail.contains("tail-marker"));
        assert!(!stderr_tail.is_empty());

        let timeout_mapping: Vec<usize> = pyo3::types::PyModule::from_code(
            py,
            &code,
            c"baseline_verdict_case.py",
            c"baseline_verdict_case",
        )
        .unwrap()
        .getattr("timeout_mapping")
        .unwrap()
        .call0()
        .unwrap()
        .extract()
        .unwrap();
        assert_eq!(timeout_mapping, vec![3]);
    });
}
