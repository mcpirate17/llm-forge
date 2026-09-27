#![cfg(feature = "python-compat-tests")]
//! Candidate review flow contracts 13–28, with Rust-owned fixtures and assertions.

#[path = "python_contracts/candidate_review_support.rs"]
#[allow(dead_code)]
mod candidate_review_support;
#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use candidate_review_support as fixture;
use pyo3::exceptions::PyOSError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyTuple};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use support::{assert_error, attr_text, module, path, AttrPatch};

fn tree_oid(candidate: &Bound<'_, PyAny>) -> String {
    fixture::string_attr(candidate, "tree_oid")
}

fn staged_source_case(case: &support::Case, baseline: &str, staged: &str) -> PathBuf {
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "probe.py", baseline);
    fixture::commit_all(&repo, "baseline");
    fixture::write_fixture(&repo, "probe.py", staged);
    fixture::git(&repo, &["add", "probe.py"]);
    repo
}

#[allow(clippy::too_many_arguments)]
fn context<'py>(
    py: Python<'py>,
    repo: &Path,
    snapshot: &Path,
    entries: &Bound<'py, PyAny>,
    candidate: &Bound<'py, PyAny>,
    policy: &Bound<'py, PyAny>,
    root: &Path,
    surface: &str,
    profile: &str,
    runtime: &str,
) -> Bound<'py, PyAny> {
    fixture::review_context(
        py,
        repo,
        snapshot,
        candidate,
        entries,
        policy,
        surface,
        profile,
        None,
        &root.join(runtime),
    )
}

fn default_candidate<'py>(
    py: Python<'py>,
    repo: &Path,
    policy: &Bound<'py, PyAny>,
) -> Bound<'py, PyAny> {
    fixture::classify_candidate(
        py,
        &fixture::resolve_candidate(py, repo, "index", None, None),
        policy,
    )
}

fn selection<'py>(py: Python<'py>, tests: &[&str]) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("tests", PyTuple::new(py, tests).unwrap())
        .unwrap();
    kwargs.set_item("graph", PyDict::new(py)).unwrap();
    kwargs.set_item("findings", PyTuple::empty(py)).unwrap();
    module(py, "conductor.candidate_review.checks")
        .getattr("TestSelection")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn policy_check<'py>(policy: &Bound<'py, PyAny>, id: &str) -> Bound<'py, PyAny> {
    policy
        .getattr("checks")
        .unwrap()
        .try_iter()
        .unwrap()
        .map(Result::unwrap)
        .find(|check| attr_text(check, "check_id") == id)
        .unwrap()
}

fn run_targeted<'py>(
    py: Python<'py>,
    ctx: &Bound<'py, PyAny>,
    selected: &Bound<'py, PyAny>,
    check: &Bound<'py, PyAny>,
    coverage: bool,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("coverage", coverage).unwrap();
    module(py, "conductor.candidate_review.verification")
        .getattr("run_targeted_tests")
        .unwrap()
        .call((ctx, selected, check), Some(&kwargs))
        .unwrap()
}

fn finding_rules(result: &Bound<'_, PyAny>) -> Vec<String> {
    result
        .getattr("findings")
        .unwrap()
        .try_iter()
        .unwrap()
        .map(|row| attr_text(&row.unwrap(), "rule_id"))
        .collect()
}

#[pyclass]
struct ProcessCallback {
    selected_exit: i32,
    crash: bool,
    only_t1: bool,
}

#[pymethods]
impl ProcessCallback {
    #[pyo3(signature = (command, **_kwargs))]
    fn __call__(
        &self,
        py: Python<'_>,
        command: &Bound<'_, PyAny>,
        _kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<Py<PyAny>> {
        if self.crash {
            return Err(PyOSError::new_err("deliberate test runner crash"));
        }
        let commands: Vec<String> = command.extract()?;
        let exit = if !self.only_t1 || commands.iter().any(|arg| arg == "t1.py") {
            self.selected_exit
        } else {
            0
        };
        let output = if self.only_t1 {
            "..F [  6%]"
        } else {
            "test failed"
        };
        let completed = module(py, "subprocess")
            .getattr("CompletedProcess")?
            .call1((command, exit, output, ""))?;
        Ok(completed.unbind())
    }
}

fn patch_runner<'py>(
    py: Python<'py>,
    sharding: &Bound<'py, pyo3::types::PyModule>,
    code: i32,
    crash: bool,
    only_t1: bool,
) -> AttrPatch {
    let callback = Py::new(
        py,
        ProcessCallback {
            selected_exit: code,
            crash,
            only_t1,
        },
    )
    .unwrap();
    AttrPatch::replace(
        sharding.as_any(),
        "_run_process",
        callback.bind(py).as_any(),
    )
}

#[test]
fn targeted_test_selection_execution_and_coverage() {
    let case = fixture::isolated_case();
    let repo = staged_source_case(
        &case,
        "def value():\n    return 1\n",
        "def value():\n    return 2\n",
    );
    fixture::write_fixture(&repo, "conductor/__init__.py", "");
    fixture::write_fixture(
        &repo,
        "conductor/test_probe.py",
        "from probe import value\n\ndef test_value_boundary_property():\n    assert value() == 2\n",
    );
    fixture::git(
        &repo,
        &["add", "conductor/__init__.py", "conductor/test_probe.py"],
    );
    Python::attach(|py| {
        let policy = fixture::default_policy(py);
        let candidate = default_candidate(py, &repo, &policy);
        fixture::with_materialized(
            py,
            &repo,
            &tree_oid(&candidate),
            None,
            |snapshot, entries| {
                let ctx = context(
                    py,
                    &repo,
                    snapshot,
                    entries,
                    &candidate,
                    &policy,
                    case.root(),
                    "pre-commit",
                    "full",
                    "runtime",
                );
                assert_targeted_selection_and_coverage(py, &ctx, &policy);
                assert_targeted_failures(py, &ctx, &policy);
            },
        );
    });
}

fn assert_targeted_selection_and_coverage(
    py: Python<'_>,
    ctx: &Bound<'_, PyAny>,
    policy: &Bound<'_, PyAny>,
) {
    let evidence = module(py, "conductor.candidate_review.verification")
        .getattr("check_test_evidence")
        .unwrap()
        .call1((ctx,))
        .unwrap();
    let selected = evidence.get_item(1).unwrap();
    assert_eq!(
        selected
            .getattr("tests")
            .unwrap()
            .extract::<Vec<String>>()
            .unwrap(),
        ["conductor/test_probe.py"]
    );
    let rules: BTreeSet<_> = finding_rules(&evidence.get_item(0).unwrap())
        .into_iter()
        .collect();
    assert_eq!(rules, BTreeSet::from(["graph-evidence-incomplete".into()]));
    let fast = policy_check(policy, "targeted-tests");
    let fast_result = run_targeted(py, ctx, &selected, &fast, false);
    assert_eq!(
        attr_text(&fast_result, "status"),
        "passed",
        "{}",
        fast_result.repr().unwrap()
    );
    let full = policy_check(policy, "targeted-tests-full");
    let covered = run_targeted(py, ctx, &selected, &full, true);
    assert_eq!(
        attr_text(&covered, "status"),
        "passed",
        "{:?}",
        finding_rules(&covered)
    );
    assert_eq!(
        covered
            .getattr("metrics")
            .unwrap()
            .get_item("changed_coverage_percent")
            .unwrap()
            .extract::<f64>()
            .unwrap(),
        100.0
    );
    assert_eq!(
        attr_text(
            &run_targeted(py, ctx, &selection(py, &[]), &fast, false),
            "status"
        ),
        "skipped"
    );
}

fn assert_targeted_failures(py: Python<'_>, ctx: &Bound<'_, PyAny>, policy: &Bound<'_, PyAny>) {
    let sharding = module(py, "conductor.candidate_review.sharding");
    let selected = selection(py, &["conductor/test_probe.py"]);
    let fast = policy_check(policy, "targeted-tests");
    {
        let _failure = patch_runner(py, &sharding, 1, false, false);
        let failed = run_targeted(py, ctx, &selected, &fast, false);
        assert_eq!(finding_rules(&failed)[0], "targeted-test-failure");
    }
    {
        let _crash = patch_runner(py, &sharding, 0, true, false);
        let crashed = run_targeted(py, ctx, &selected, &fast, false);
        assert_eq!(finding_rules(&crashed)[0], "targeted-test-crash");
    }
    let empty = PyDict::new(py);
    let coverage = module(py, "conductor.candidate_review.verification");
    assert_error(
        py,
        coverage
            .getattr("_coverage_counts")
            .unwrap()
            .call1((ctx, &empty, &empty))
            .unwrap_err(),
        &module(py, "builtins").getattr("ValueError").unwrap(),
        "no files object",
    );
}

fn assert_shard_distribution(py: Python<'_>) {
    let sharding = module(py, "conductor.candidate_review.sharding");
    let tests: Vec<String> = (0..10).map(|i| format!("t{i}.py")).collect();
    let shard = sharding.getattr("shard_tests").unwrap();
    for max in [0, 100] {
        assert_eq!(
            shard
                .call1((&tests, max))
                .unwrap()
                .extract::<Vec<Vec<String>>>()
                .unwrap(),
            vec![tests.clone()]
        );
    }
    let chunks: Vec<Vec<String>> = shard.call1((&tests, 3)).unwrap().extract().unwrap();
    assert_eq!(chunks.len(), 4);
    let mut flattened: Vec<String> = chunks.iter().flatten().cloned().collect();
    flattened.sort();
    assert_eq!(flattened, tests);
    assert!(chunks.iter().all(|part| part.len() <= 3));
    assert_eq!(chunks[0][0], "t0.py");
    assert_eq!(chunks[1][0], "t1.py");
    let lengths: Vec<usize> = chunks.iter().map(Vec::len).collect();
    assert!(lengths.iter().max().unwrap() - lengths.iter().min().unwrap() <= 1);
}

#[allow(clippy::too_many_arguments)]
fn coverage_result<'py>(
    py: Python<'py>,
    repo: &Path,
    root: &Path,
    candidate: &Bound<'py, PyAny>,
    policy: &Bound<'py, PyAny>,
    selected: &Bound<'py, PyAny>,
    check: &Bound<'py, PyAny>,
    runtime: &str,
) -> Bound<'py, PyAny> {
    fixture::with_materialized(py, repo, &tree_oid(candidate), None, |snapshot, entries| {
        let ctx = context(
            py,
            repo,
            snapshot,
            entries,
            candidate,
            policy,
            root,
            "pre-commit",
            "full",
            runtime,
        );
        run_targeted(py, &ctx, selected, check, true)
    })
}

#[test]
fn targeted_test_sharding_preserves_the_changed_coverage_verdict() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    Python::attach(|py| {
        assert_shard_distribution(py);
        fixture::coverage_fixture(&repo);
        let policy = fixture::default_policy(py);
        let candidate = default_candidate(py, &repo, &policy);
        let selected = selection(py, &["conductor/test_a.py", "conductor/test_b.py"]);
        let full = policy_check(&policy, "targeted-tests-full");
        let plain = PyDict::new(py);
        plain.set_item("shard_max_files", 0).unwrap();
        let unsharded = coverage_result(
            py,
            &repo,
            case.root(),
            &candidate,
            &policy,
            &selected,
            &fixture::replace_fields(py, &full, &plain),
            "runtime-plain",
        );
        let shards = PyDict::new(py);
        shards.set_item("shard_max_files", 1).unwrap();
        shards.set_item("shard_workers", 2).unwrap();
        let sharded = coverage_result(
            py,
            &repo,
            case.root(),
            &candidate,
            &policy,
            &selected,
            &fixture::replace_fields(py, &full, &shards),
            "runtime-sharded",
        );
        assert_eq!(
            sharded
                .getattr("metrics")
                .unwrap()
                .get_item("shard_count")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            2
        );
        assert_eq!(
            attr_text(&unsharded, "status"),
            "passed",
            "{}",
            unsharded.repr().unwrap()
        );
        assert_eq!(
            attr_text(&sharded, "status"),
            "passed",
            "{}",
            sharded.repr().unwrap()
        );
        let unsharded_coverage: f64 = unsharded
            .getattr("metrics")
            .unwrap()
            .get_item("changed_coverage_percent")
            .unwrap()
            .extract()
            .unwrap();
        let sharded_coverage: f64 = sharded
            .getattr("metrics")
            .unwrap()
            .get_item("changed_coverage_percent")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(unsharded_coverage, 100.0);
        assert_eq!(sharded_coverage, unsharded_coverage);
    });
}

#[test]
fn targeted_test_shard_killed_by_signal_is_not_reported_as_a_failure() {
    let case = fixture::isolated_case();
    let repo = staged_source_case(
        &case,
        "def value():\n    return 2\n",
        "def value():\n    return 3\n",
    );
    Python::attach(|py| {
        let policy = fixture::default_policy(py);
        let candidate = default_candidate(py, &repo, &policy);
        let options = PyDict::new(py);
        options.set_item("shard_max_files", 2).unwrap();
        options.set_item("shard_workers", 4).unwrap();
        let check =
            fixture::replace_fields(py, &policy_check(&policy, "targeted-tests-full"), &options);
        let tests: Vec<String> = (0..8).map(|i| format!("t{i}.py")).collect();
        let refs: Vec<&str> = tests.iter().map(String::as_str).collect();
        let selected = selection(py, &refs);
        fixture::with_materialized(
            py,
            &repo,
            &tree_oid(&candidate),
            None,
            |snapshot, entries| {
                let ctx = context(
                    py,
                    &repo,
                    snapshot,
                    entries,
                    &candidate,
                    &policy,
                    case.root(),
                    "pre-commit",
                    "full",
                    "runtime-killed",
                );
                let sharding = module(py, "conductor.candidate_review.sharding");
                {
                    let _killed = patch_runner(py, &sharding, -9, false, true);
                    let result = run_targeted(py, &ctx, &selected, &check, false);
                    assert_eq!(finding_rules(&result), ["targeted-test-killed"]);
                    let finding = result.getattr("findings").unwrap().get_item(0).unwrap();
                    assert_eq!(
                        finding
                            .getattr("evidence")
                            .unwrap()
                            .get_item("killed_shards")
                            .unwrap()
                            .extract::<Vec<usize>>()
                            .unwrap(),
                        [2]
                    );
                    assert!(finding
                        .getattr("evidence")
                        .unwrap()
                        .get_item("timeout_seconds")
                        .unwrap()
                        .eq(check.getattr("timeout_seconds").unwrap())
                        .unwrap());
                }
                {
                    let _failure = patch_runner(py, &sharding, 1, false, true);
                    assert_eq!(
                        finding_rules(&run_targeted(py, &ctx, &selected, &check, false)),
                        ["targeted-test-failure"]
                    );
                }
            },
        );
    });
}

#[test]
fn engine_and_tree_integrity_fail_closed_without_candidate_evidence() {
    let case = fixture::isolated_case();
    let repo = staged_source_case(&case, "VALUE = 1\n", "VALUE = 2\n");
    Python::attach(|py| {
        let policy = fixture::default_policy(py);
        let candidate = default_candidate(py, &repo, &policy);
        fixture::with_materialized(
            py,
            &repo,
            &tree_oid(&candidate),
            None,
            |snapshot, entries| {
                let ctx = context(
                    py,
                    &repo,
                    snapshot,
                    entries,
                    &candidate,
                    &policy,
                    case.root(),
                    "post-commit",
                    "fast",
                    "runtime",
                );
                assert_engine_integrity(py, &ctx);
                assert_tree_integrity(py, &ctx);
            },
        );
        let empty = PyDict::new(py);
        let result = module(py, "conductor.candidate_review.engine")
            .getattr("verify_receipt_payload")
            .unwrap()
            .call1((empty,))
            .unwrap();
        assert!(!result.get_item(0).unwrap().extract::<bool>().unwrap());
        assert_eq!(
            result.get_item(1).unwrap().extract::<String>().unwrap(),
            "receipt_digest is absent"
        );
    });
}

fn assert_engine_integrity(py: Python<'_>, ctx: &Bound<'_, PyAny>) {
    let engine = module(py, "conductor.candidate_review.engine");
    let integrity = engine
        .getattr("_engine_integrity")
        .unwrap()
        .call1((ctx,))
        .unwrap();
    assert_eq!(
        integrity
            .get_item(0)
            .unwrap()
            .get_item("candidate_source_sha256")
            .unwrap()
            .extract::<String>()
            .unwrap(),
        ""
    );
    assert_eq!(
        finding_rules(&integrity.get_item(1).unwrap())[0],
        "engine-absent-from-candidate"
    );
    let bypass = engine
        .getattr("_bypass_evidence")
        .unwrap()
        .call1((ctx,))
        .unwrap();
    assert_eq!(
        bypass
            .get_item(0)
            .unwrap()
            .get_item("precommit_receipt")
            .unwrap()
            .extract::<String>()
            .unwrap(),
        "missing_or_invalid"
    );
    assert_eq!(
        finding_rules(&bypass.get_item(1).unwrap())[0],
        "precommit-bypass-recovered"
    );
    let runtime = module(py, "builtins")
        .getattr("RuntimeError")
        .unwrap()
        .call1(("boom",))
        .unwrap();
    let crash = engine
        .getattr("_crash_result")
        .unwrap()
        .call1(("probe", runtime))
        .unwrap();
    assert_eq!(attr_text(&crash, "status"), "error");
}

fn assert_tree_integrity(py: Python<'_>, ctx: &Bound<'_, PyAny>) {
    let model = module(py, "conductor.candidate_review.model");
    let entry = model.getattr("TreeEntry").unwrap();
    let first = entry
        .call1(("Case.py", "100644", "blob", "1".repeat(40), 1))
        .unwrap();
    let second = entry
        .call1(("case.py", "100600", "blob", "2".repeat(40), 1))
        .unwrap();
    let options = PyDict::new(py);
    options
        .set_item("entries", PyTuple::new(py, [first, second]).unwrap())
        .unwrap();
    let bad = fixture::replace_fields(py, ctx, &options);
    let outcome = module(py, "conductor.candidate_review.checks")
        .getattr("_tree_integrity_findings")
        .unwrap()
        .call1((bad,))
        .unwrap();
    let rules: BTreeSet<String> = outcome
        .get_item(0)
        .unwrap()
        .try_iter()
        .unwrap()
        .map(|row| attr_text(&row.unwrap(), "rule_id"))
        .collect();
    assert_eq!(
        rules,
        BTreeSet::from(["case-collision".into(), "unsupported-git-mode".into()])
    );
    let keys: BTreeSet<String> = outcome
        .get_item(1)
        .unwrap()
        .call_method0("keys")
        .unwrap()
        .try_iter()
        .unwrap()
        .map(|key| key.unwrap().extract::<String>().unwrap())
        .collect();
    assert_eq!(keys, BTreeSet::from(["Case.py".into(), "case.py".into()]));
}

#[test]
fn graph_selected_tests_use_immutable_matching_metadata() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "probe.py", "def value():\n    return 1\n");
    let base = fixture::commit_all(&repo, "baseline");
    fixture::write_fixture(&repo, "probe.py", "def value():\n    return 2\n");
    fixture::write_fixture(
        &repo,
        "conductor/test_graph_probe.py",
        "def test_value_property():\n    assert True\n",
    );
    fixture::write_fixture(&repo, "native/lib.rs", "#[test]\nfn probe() {}\n");
    fixture::git(
        &repo,
        &[
            "add",
            "probe.py",
            "conductor/test_graph_probe.py",
            "native/lib.rs",
        ],
    );
    fixture::write_graph_database(&repo, &base);
    Python::attach(|py| {
        let policy = fixture::default_policy(py);
        let candidate = default_candidate(py, &repo, &policy);
        fixture::with_materialized(
            py,
            &repo,
            &tree_oid(&candidate),
            None,
            |snapshot, entries| {
                let ctx = context(
                    py,
                    &repo,
                    snapshot,
                    entries,
                    &candidate,
                    &policy,
                    case.root(),
                    "pre-commit",
                    "fast",
                    "runtime",
                );
                let result = module(py, "conductor.candidate_review.verification")
                    .getattr("check_test_evidence")
                    .unwrap()
                    .call1((ctx,))
                    .unwrap();
                assert_eq!(
                    result
                        .get_item(0)
                        .unwrap()
                        .getattr("findings")
                        .unwrap()
                        .len()
                        .unwrap(),
                    0
                );
                let selected = result.get_item(1).unwrap();
                assert_eq!(
                    selected
                        .getattr("tests")
                        .unwrap()
                        .extract::<Vec<String>>()
                        .unwrap(),
                    ["conductor/test_graph_probe.py"]
                );
                assert_eq!(
                    selected
                        .getattr("graph")
                        .unwrap()
                        .get_item("selected_edges")
                        .unwrap()
                        .extract::<usize>()
                        .unwrap(),
                    1
                );
                assert_eq!(
                    selected
                        .getattr("graph")
                        .unwrap()
                        .get_item("head_sha")
                        .unwrap()
                        .extract::<String>()
                        .unwrap(),
                    base
                );
            },
        );
    });
}

#[test]
fn index_preserves_deletion_and_rename_identity() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "deleted.txt", "remove me\n");
    fixture::write_fixture(&repo, "old-name.txt", "rename me exactly\n");
    fixture::commit_all(&repo, "baseline");
    fixture::git(&repo, &["rm", "--quiet", "deleted.txt"]);
    fixture::git(&repo, &["mv", "old-name.txt", "new-name.txt"]);
    Python::attach(|py| {
        let candidate = fixture::resolve_candidate(py, &repo, "index", None, None);
        let mut changes = HashMap::new();
        for row in candidate.getattr("changes").unwrap().try_iter().unwrap() {
            let row = row.unwrap();
            changes.insert(attr_text(&row, "path"), row);
        }
        let deleted = &changes["deleted.txt"];
        assert_eq!(attr_text(deleted, "status"), "D");
        assert!(deleted
            .getattr("deleted")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert_eq!(attr_text(deleted, "new_mode"), "000000");
        assert_eq!(attr_text(deleted, "new_oid"), "0".repeat(40));
        let renamed = &changes["new-name.txt"];
        assert_eq!(attr_text(renamed, "status"), "R100");
        assert_eq!(attr_text(renamed, "old_path"), "old-name.txt");
        assert!(renamed
            .getattr("old_oid")
            .unwrap()
            .eq(renamed.getattr("new_oid").unwrap())
            .unwrap());
    });
}

#[test]
fn rename_retains_old_path_risk_and_policy_classes() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "sensitive/mechanism.py", "VALUE = 1\n");
    fixture::commit_all(&repo, "baseline");
    fixture::git(&repo, &["mv", "sensitive/mechanism.py", "moved.py"]);
    Python::attach(|py| {
        let candidate = fixture::resolve_candidate(py, &repo, "index", None, None);
        let policy = fixture::minimal_policy_text(&fixture::utc_date(py, 30), "exceptions = []")
            .replace(
                "[classes]\n\n[risk]\nhigh = []",
                "[classes]\nnovel = [\"sensitive/**\"]\n\n[risk]\nhigh = [\"sensitive/**\"]",
            );
        let file = fixture::write_fixture(&repo, "policy.toml", &policy);
        let classified =
            fixture::classify_candidate(py, &candidate, &fixture::load_policy(py, &file));
        let change = classified.getattr("changes").unwrap().get_item(0).unwrap();
        assert_eq!(attr_text(&change, "path"), "moved.py");
        assert_eq!(attr_text(&change, "old_path"), "sensitive/mechanism.py");
        assert_eq!(attr_text(&change, "risk"), "high");
        let classes: BTreeSet<String> = change
            .getattr("classes")
            .unwrap()
            .extract::<Vec<String>>()
            .unwrap()
            .into_iter()
            .collect();
        assert!(BTreeSet::from(["python".into(), "novel".into()]).is_subset(&classes));
    });
}

#[test]
fn materialize_tree_allows_internal_symlink_and_rejects_escape() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "docs/target.txt", "inside\n");
    symlink("target.txt", repo.join("docs/link.txt")).unwrap();
    let safe_commit = fixture::commit_all(&repo, "safe link");
    Python::attach(|py| {
        let safe = fixture::resolve_candidate(py, &repo, "commit", None, Some(&safe_commit));
        fixture::with_materialized(py, &repo, &tree_oid(&safe), None, |snapshot, entries| {
            let link = snapshot.join("docs/link.txt");
            assert!(link.is_symlink());
            assert_eq!(fs::read_link(&link).unwrap(), Path::new("target.txt"));
            assert_eq!(fs::read_to_string(link).unwrap(), "inside\n");
            assert!(entries
                .try_iter()
                .unwrap()
                .any(|entry| attr_text(&entry.unwrap(), "path") == "docs/link.txt"));
        });
        symlink("../outside.txt", repo.join("escape")).unwrap();
        let unsafe_commit = fixture::commit_all(&repo, "escaping link");
        let unsafe_candidate =
            fixture::resolve_candidate(py, &repo, "commit", None, Some(&unsafe_commit));
        let source = module(py, "conductor.candidate_review.git_source");
        let manager = source
            .getattr("materialize_tree")
            .unwrap()
            .call1((path(py, &repo), tree_oid(&unsafe_candidate)))
            .unwrap();
        assert_error(
            py,
            manager.call_method0("__enter__").unwrap_err(),
            &source.getattr("GitSourceError").unwrap(),
            "symlink escapes candidate snapshot",
        );
    });
}

#[test]
fn materialize_tree_rejects_blob_before_loading_over_budget() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fs::write(repo.join("payload.bin"), b"0123456789").unwrap();
    let commit = fixture::commit_all(&repo, "payload");
    Python::attach(|py| {
        let candidate = fixture::resolve_candidate(py, &repo, "commit", None, Some(&commit));
        let source = module(py, "conductor.candidate_review.git_source");
        let kwargs = PyDict::new(py);
        kwargs.set_item("max_blob_bytes", 9).unwrap();
        let manager = source
            .getattr("materialize_tree")
            .unwrap()
            .call((path(py, &repo), tree_oid(&candidate)), Some(&kwargs))
            .unwrap();
        assert_error(
            py,
            manager.call_method0("__enter__").unwrap_err(),
            &source.getattr("GitSourceError").unwrap(),
            "blob exceeds materialization limit",
        );
    });
}

#[test]
fn gitlink_is_represented_without_materializing_foreign_tree() {
    let case = fixture::isolated_case();
    let module_repo = case.root().join("module");
    fixture::init_repo(&module_repo);
    fixture::write_fixture(&module_repo, "module.txt", "module\n");
    let module_commit = fixture::commit_all(&module_repo, "module");
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::git(
        &repo,
        &[
            "commit",
            "--quiet",
            "--allow-empty",
            "--message",
            "baseline",
        ],
    );
    fixture::git(
        &repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            &module_commit,
            "vendor/module",
        ],
    );
    fixture::git(
        &repo,
        &["commit", "--quiet", "--message", "record submodule"],
    );
    Python::attach(|py| {
        let candidate = fixture::resolve_candidate(py, &repo, "commit", None, None);
        let source = module(py, "conductor.candidate_review.git_source");
        let entries = source
            .getattr("list_tree")
            .unwrap()
            .call1((path(py, &repo), tree_oid(&candidate)))
            .unwrap();
        let entry = entries.get_item(0).unwrap();
        assert_eq!(
            (
                attr_text(&entry, "path"),
                attr_text(&entry, "mode"),
                attr_text(&entry, "object_type"),
                attr_text(&entry, "oid")
            ),
            (
                "vendor/module".into(),
                "160000".into(),
                "commit".into(),
                module_commit.clone()
            )
        );
        let change = candidate.getattr("changes").unwrap().get_item(0).unwrap();
        assert_eq!(attr_text(&change, "new_mode"), "160000");
        assert_eq!(attr_text(&change, "new_oid"), module_commit);
        fixture::with_materialized(
            py,
            &repo,
            &tree_oid(&candidate),
            None,
            |snapshot, materialized| {
                assert!(materialized.eq(&entries).unwrap());
                assert!(!snapshot.join("vendor/module").exists());
            },
        );
    });
}

#[test]
fn malformed_and_ambiguous_refs_fail_closed() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "value.txt", "one\n");
    fixture::commit_all(&repo, "one");
    fixture::git(&repo, &["branch", "collision"]);
    fixture::write_fixture(&repo, "value.txt", "two\n");
    fixture::commit_all(&repo, "two");
    fixture::git(&repo, &["tag", "collision"]);
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.git_source");
        let resolve = source.getattr("resolve_commit").unwrap();
        let class = source.getattr("GitSourceError").unwrap();
        assert_error(
            py,
            resolve.call1((path(py, &repo), "--verify")).unwrap_err(),
            &class,
            "invalid or ambiguous Git ref",
        );
        assert_error(
            py,
            resolve.call1((path(py, &repo), "collision")).unwrap_err(),
            &class,
            "ambiguous",
        );
    });
}

#[test]
fn malformed_policy_fails_closed() {
    let case = fixture::isolated_case();
    let file = case.write("candidate_policy.toml", "schema_version = [\n");
    Python::attach(|py| {
        let policy = module(py, "conductor.candidate_review.policy");
        assert_error(
            py,
            policy
                .getattr("load_policy")
                .unwrap()
                .call1((path(py, &file),))
                .unwrap_err(),
            &policy.getattr("PolicyError").unwrap(),
            "malformed candidate policy",
        );
    });
}

#[test]
fn expired_policy_fails_closed() {
    let case = fixture::isolated_case();
    Python::attach(|py| {
        let file = case.write(
            "candidate_policy.toml",
            &fixture::minimal_policy_text(&fixture::utc_date(py, -1), "exceptions = []"),
        );
        let policy = module(py, "conductor.candidate_review.policy");
        assert_error(
            py,
            policy
                .getattr("load_policy")
                .unwrap()
                .call1((path(py, &file),))
                .unwrap_err(),
            &policy.getattr("PolicyError").unwrap(),
            "baseline window expired",
        );
    });
}

#[test]
fn blanket_exception_scope_fails_closed() {
    let case = fixture::isolated_case();
    Python::attach(|py| {
        let expiry = fixture::utc_date(py, 30);
        let exception = format!(
            r#"[[exceptions]]
id = "too-broad"
check = "candidate-integrity"
path = "**"
owner = "governance"
justification = "This intentionally broad test exception must be rejected."
expires = {expiry}
"#
        );
        let file = case.write(
            "candidate_policy.toml",
            &fixture::minimal_policy_text(&expiry, &exception),
        );
        let policy = module(py, "conductor.candidate_review.policy");
        assert_error(
            py,
            policy
                .getattr("load_policy")
                .unwrap()
                .call1((path(py, &file),))
                .unwrap_err(),
            &policy.getattr("PolicyError").unwrap(),
            "forbidden blanket scope",
        );
    });
}

#[test]
fn receipt_digest_is_invalidated_by_candidate_tree_mutation() {
    let _case = fixture::isolated_case();
    Python::attach(|py| {
        let receipt = fixture::fixture_receipt(py);
        let payload = receipt.call_method0("to_dict").unwrap();
        let engine = module(py, "conductor.candidate_review.engine");
        let verify = engine.getattr("verify_receipt_payload").unwrap();
        let valid = verify.call1((&payload,)).unwrap();
        assert!(valid.get_item(0).unwrap().extract::<bool>().unwrap());
        assert_eq!(
            valid.get_item(1).unwrap().extract::<String>().unwrap(),
            "ok"
        );
        let tampered = module(py, "copy")
            .getattr("deepcopy")
            .unwrap()
            .call1((payload,))
            .unwrap();
        tampered
            .get_item("candidate")
            .unwrap()
            .set_item("tree_oid", "9".repeat(40))
            .unwrap();
        let invalid = verify.call1((tampered,)).unwrap();
        assert!(!invalid.get_item(0).unwrap().extract::<bool>().unwrap());
        assert!(invalid
            .get_item(1)
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("receipt digest mismatch"));
    });
}

#[test]
fn human_sarif_and_junit_reports_preserve_identity_and_findings() {
    let case = fixture::isolated_case();
    Python::attach(|py| {
        let receipt = fixture::fixture_receipt(py);
        let reporters = module(py, "conductor.candidate_review.reporters");
        let id = attr_text(&receipt, "receipt_id");
        let summary = reporters
            .getattr("human_summary")
            .unwrap()
            .call1((&receipt,))
            .unwrap()
            .extract::<String>()
            .unwrap();
        assert!(summary.contains(&id));
        assert!(summary.contains("candidate-integrity/unsafe-symlink"));
        assert_report_payload(py, &reporters, &receipt, &id);
        assert_report_files(py, &reporters, &receipt, &id, case.root());
    });
}

fn assert_report_payload(
    py: Python<'_>,
    reporters: &Bound<'_, pyo3::types::PyModule>,
    receipt: &Bound<'_, PyAny>,
    id: &str,
) {
    let sarif = reporters
        .getattr("sarif_payload")
        .unwrap()
        .call1((receipt,))
        .unwrap();
    assert_eq!(
        sarif
            .get_item("version")
            .unwrap()
            .extract::<String>()
            .unwrap(),
        "2.1.0"
    );
    let run = sarif.get_item("runs").unwrap().get_item(0).unwrap();
    assert_eq!(
        run.get_item("automationDetails")
            .unwrap()
            .get_item("id")
            .unwrap()
            .extract::<String>()
            .unwrap(),
        id
    );
    let result = run.get_item("results").unwrap().get_item(0).unwrap();
    let fingerprint: HashMap<String, String> = result
        .get_item("partialFingerprints")
        .unwrap()
        .extract()
        .unwrap();
    assert_eq!(
        fingerprint,
        HashMap::from([(
            "governanceFingerprint".into(),
            "test-fingerprint-not-a-secret".into()
        )])
    );
    let location = result
        .get_item("locations")
        .unwrap()
        .get_item(0)
        .unwrap()
        .get_item("physicalLocation")
        .unwrap();
    let actual = module(py, "json")
        .getattr("dumps")
        .unwrap()
        .call1((location,))
        .unwrap();
    let value: Value = serde_json::from_str(&actual.extract::<String>().unwrap()).unwrap();
    assert_eq!(
        value,
        json!({"artifactLocation":{"uri":"escape"},
                            "region":{"startLine":2,"startColumn":1}})
    );
    let xml = reporters
        .getattr("junit_xml")
        .unwrap()
        .call1((receipt,))
        .unwrap();
    let suite = module(py, "xml.etree.ElementTree")
        .getattr("fromstring")
        .unwrap()
        .call1((xml,))
        .unwrap();
    let attributes: HashMap<String, String> = suite.getattr("attrib").unwrap().extract().unwrap();
    assert_eq!(
        attributes,
        HashMap::from([
            ("name".into(), "candidate-review:manual:fast".into()),
            ("tests".into(), "1".into()),
            ("failures".into(), "1".into()),
            ("skipped".into(), "0".into()),
            ("time".into(), "0.125".into()),
            ("id".into(), id.into()),
        ])
    );
    let failure = suite.call_method1("find", ("./testcase/failure",)).unwrap();
    assert!(!failure.is_none());
    assert!(attr_text(&failure, "text").contains("critical unsafe-symlink"));
}

fn assert_report_files(
    py: Python<'_>,
    reporters: &Bound<'_, pyo3::types::PyModule>,
    receipt: &Bound<'_, PyAny>,
    id: &str,
    root: &Path,
) {
    let json_file = root.join("receipt.json");
    let sarif_file = root.join("receipt.sarif");
    let junit_file = root.join("receipt.junit.xml");
    let kwargs = PyDict::new(py);
    kwargs.set_item("json_out", path(py, &json_file)).unwrap();
    kwargs.set_item("sarif_out", path(py, &sarif_file)).unwrap();
    kwargs.set_item("junit_out", path(py, &junit_file)).unwrap();
    reporters
        .getattr("write_outputs")
        .unwrap()
        .call((receipt,), Some(&kwargs))
        .unwrap();
    let json: Value = serde_json::from_slice(&fs::read(json_file).unwrap()).unwrap();
    assert_eq!(json["receipt_id"], id);
    let sarif: Value = serde_json::from_slice(&fs::read(sarif_file).unwrap()).unwrap();
    assert_eq!(sarif["version"], "2.1.0");
    let suite = module(py, "xml.etree.ElementTree")
        .getattr("parse")
        .unwrap()
        .call1((path(py, &junit_file),))
        .unwrap()
        .call_method0("getroot")
        .unwrap();
    assert_eq!(
        suite
            .getattr("attrib")
            .unwrap()
            .get_item("id")
            .unwrap()
            .extract::<String>()
            .unwrap(),
        id
    );
}
