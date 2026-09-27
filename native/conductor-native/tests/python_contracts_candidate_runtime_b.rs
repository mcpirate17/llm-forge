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
