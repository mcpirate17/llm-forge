#![cfg(feature = "python-compat-tests")]
//! Candidate review flow contracts 1–12, with Rust-owned fixtures and assertions.

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
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyTuple};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::Path;
use std::process::Command;
use support::{assert_error, attr_text, module, path};

fn tree_oid(candidate: &Bound<'_, PyAny>) -> String {
    fixture::string_attr(candidate, "tree_oid")
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

fn fixture_review<'py>(
    py: Python<'py>,
    repo: &Path,
    candidate: &Bound<'py, PyAny>,
    surface: &str,
    runtime: &Path,
) -> Bound<'py, PyAny> {
    fixture::with_materialized(py, repo, &tree_oid(candidate), None, |snapshot, entries| {
        let policy = fixture::load_policy(py, &snapshot.join("conductor/candidate_policy.toml"));
        let classified = fixture::classify_candidate(py, candidate, &policy);
        let context = fixture::review_context(
            py,
            repo,
            snapshot,
            &classified,
            entries,
            &policy,
            surface,
            "fast",
            None,
            runtime,
        );
        module(py, "conductor.candidate_review.engine")
            .getattr("run_review")
            .unwrap()
            .call1((context,))
            .unwrap()
    })
}

fn repo_with_staged_probe(case: &support::Case) -> std::path::PathBuf {
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "probe.py", "VALUE = 1\n");
    fixture::commit_all(&repo, "baseline");
    fixture::write_fixture(&repo, "probe.py", "VALUE = 2\n");
    fixture::git(&repo, &["add", "probe.py"]);
    repo
}

#[allow(clippy::too_many_arguments)]
fn configured_context<'py>(
    py: Python<'py>,
    repo: &Path,
    snapshot: &Path,
    entries: &Bound<'py, PyAny>,
    candidate: &Bound<'py, PyAny>,
    policy: &Bound<'py, PyAny>,
    surface: &str,
    profile: &str,
    root: &Path,
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
        &root.join("runtime"),
    )
}

#[test]
fn index_candidate_ignores_unstaged_and_untracked_content() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "tracked.txt", "committed\n");
    fixture::commit_all(&repo, "baseline");
    fixture::write_fixture(&repo, "candidate.txt", "staged\n");
    fixture::git(&repo, &["add", "candidate.txt"]);
    fixture::write_fixture(&repo, "tracked.txt", "unstaged\n");
    fixture::write_fixture(&repo, "untracked.txt", "untracked\n");
    Python::attach(|py| {
        let candidate = fixture::resolve_candidate(py, &repo, "index", None, None);
        let changes = candidate.getattr("changes").unwrap();
        assert_eq!(changes.len().unwrap(), 1);
        let change = changes.get_item(0).unwrap();
        assert_eq!(
            (attr_text(&change, "status"), attr_text(&change, "path")),
            ("A".into(), "candidate.txt".into())
        );
        fixture::with_materialized(py, &repo, &tree_oid(&candidate), None, |snapshot, _| {
            assert_eq!(
                fs::read_to_string(snapshot.join("candidate.txt")).unwrap(),
                "staged\n"
            );
            assert_eq!(
                fs::read_to_string(snapshot.join("tracked.txt")).unwrap(),
                "committed\n"
            );
            assert!(!snapshot.join("untracked.txt").exists());
        });
    });
}

#[test]
fn structured_claims_reject_exact_path_overlap_and_bind_content() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "base.txt", "base\n");
    fixture::commit_all(&repo, "baseline");
    Python::attach(|py| {
        let ownership = module(py, "conductor.candidate_review.ownership");
        let kwargs = PyDict::new(py);
        kwargs.set_item("owner", "Codex").unwrap();
        kwargs
            .set_item("paths", ["conductor/candidate_review"])
            .unwrap();
        kwargs
            .set_item("justification", "candidate governance implementation")
            .unwrap();
        kwargs.set_item("max_minutes", 60).unwrap();
        let claim = ownership
            .getattr("create_claim")
            .unwrap()
            .call((path(py, &repo),), Some(&kwargs))
            .unwrap();
        let loaded = ownership
            .getattr("load_claims")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        assert_eq!(loaded.get_item(0).unwrap().len().unwrap(), 1);
        assert!(loaded
            .get_item(0)
            .unwrap()
            .get_item(0)
            .unwrap()
            .eq(&claim)
            .unwrap());
        assert_eq!(
            loaded
                .get_item(1)
                .unwrap()
                .extract::<String>()
                .unwrap()
                .len(),
            64
        );
        kwargs.set_item("owner", "Other agent").unwrap();
        kwargs
            .set_item("paths", ["conductor/candidate_review/engine.py"])
            .unwrap();
        kwargs
            .set_item("justification", "conflicting edit")
            .unwrap();
        let error = ownership
            .getattr("create_claim")
            .unwrap()
            .call((path(py, &repo),), Some(&kwargs))
            .unwrap_err();
        let class = ownership.getattr("OwnershipError").unwrap();
        assert_error(py, error, &class, "overlaps active claim");
        let release = ownership.getattr("release_claim").unwrap();
        let options = PyDict::new(py);
        options
            .set_item("claim_id", claim.getattr("claim_id").unwrap())
            .unwrap();
        options.set_item("owner", "Other agent").unwrap();
        assert_error(
            py,
            release
                .call((path(py, &repo),), Some(&options))
                .unwrap_err(),
            &class,
            "not 'Other agent'",
        );
        options.set_item("owner", "Codex").unwrap();
        assert!(release
            .call((path(py, &repo),), Some(&options))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let empty = ownership
            .getattr("load_claims")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        assert_eq!(empty.get_item(0).unwrap().len().unwrap(), 0);
    });
}

fn ownership_result<'py>(
    py: Python<'py>,
    repo: &Path,
    candidate: &Bound<'py, PyAny>,
    root: &Path,
    ledger: &str,
) -> Bound<'py, PyAny> {
    fixture::write_fixture(repo, ".current_work.md", ledger);
    fixture::with_materialized(py, repo, &tree_oid(candidate), None, |snapshot, entries| {
        let policy = fixture::load_policy(py, &snapshot.join("conductor/candidate_policy.toml"));
        let context = fixture::review_context(
            py,
            repo,
            snapshot,
            candidate,
            entries,
            &policy,
            "pre-commit",
            "fast",
            Some("Codex"),
            &root.join("runtime"),
        );
        module(py, "conductor.candidate_review.checks")
            .getattr("check_ownership")
            .unwrap()
            .call1((context,))
            .unwrap()
    })
}

#[test]
fn ownership_claim_is_independent_of_ignored_worktree_ledger() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    Python::attach(|py| {
        fixture::write_policy(py, &repo, "conductor/candidate_policy.toml");
        fixture::write_fixture(&repo, "source.py", "VALUE = 1\n");
        fixture::commit_all(&repo, "baseline");
        fixture::write_fixture(&repo, "source.py", "VALUE = 2\n");
        fixture::git(&repo, &["add", "source.py"]);
        let candidate = fixture::resolve_candidate(py, &repo, "index", None, None);
        let ownership = module(py, "conductor.candidate_review.ownership");
        let options = PyDict::new(py);
        options.set_item("owner", "Codex").unwrap();
        options.set_item("paths", ["source.py"]).unwrap();
        options
            .set_item("justification", "focused ownership test")
            .unwrap();
        options.set_item("max_minutes", 60).unwrap();
        ownership
            .getattr("create_claim")
            .unwrap()
            .call((path(py, &repo),), Some(&options))
            .unwrap();
        let first = ownership_result(
            py,
            &repo,
            &candidate,
            case.root(),
            "unrelated untracked state one\n",
        );
        let second = ownership_result(
            py,
            &repo,
            &candidate,
            case.root(),
            "malicious unrelated state two\n",
        );
        assert_eq!(first.getattr("findings").unwrap().len().unwrap(), 0);
        assert_eq!(second.getattr("findings").unwrap().len().unwrap(), 0);
        assert!(first
            .getattr("metrics")
            .unwrap()
            .get_item("state_sha256")
            .unwrap()
            .eq(second
                .getattr("metrics")
                .unwrap()
                .get_item("state_sha256")
                .unwrap())
            .unwrap());
        let store: std::path::PathBuf = ownership
            .getattr("claim_store_path")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap()
            .extract()
            .unwrap();
        fs::write(store, "{").unwrap();
        let malformed = ownership_result(py, &repo, &candidate, case.root(), "still unrelated\n");
        assert_eq!(finding_rules(&malformed), ["malformed-claim-store"]);
    });
}

#[test]
fn range_candidate_uses_merge_base_even_with_a_clean_index() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "shared.txt", "base\n");
    let merge_base = fixture::commit_all(&repo, "base");
    fixture::git(&repo, &["switch", "--quiet", "--create", "feature"]);
    fixture::write_fixture(&repo, "shared.txt", "feature\n");
    fixture::write_fixture(&repo, "feature.txt", "feature-only\n");
    let feature = fixture::commit_all(&repo, "feature");
    fixture::git(&repo, &["switch", "--quiet", "main"]);
    fixture::write_fixture(&repo, "shared.txt", "main\n");
    fixture::commit_all(&repo, "main divergence");
    assert_eq!(fixture::git(&repo, &["status", "--porcelain"]), "");
    Python::attach(|py| {
        let candidate =
            fixture::resolve_candidate(py, &repo, "range", Some("main"), Some("feature"));
        assert_eq!(attr_text(&candidate, "base_commit_oid"), merge_base);
        assert_eq!(attr_text(&candidate, "commit_oid"), feature);
        let paths: BTreeSet<String> = candidate
            .getattr("changes")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|row| attr_text(&row.unwrap(), "path"))
            .collect();
        assert_eq!(
            paths,
            BTreeSet::from(["feature.txt".into(), "shared.txt".into()])
        );
    });
}

#[test]
fn ci_empty_range_fails_closed_with_clean_index() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::install_candidate_engine(&repo);
    Python::attach(|py| {
        fixture::write_policy(py, &repo, "conductor/candidate_policy.toml");
        fixture::commit_all(&repo, "governance baseline");
        assert_eq!(fixture::git(&repo, &["status", "--porcelain"]), "");
        let candidate = fixture::resolve_candidate(py, &repo, "range", Some("HEAD"), Some("HEAD"));
        let outcome = fixture_review(py, &repo, &candidate, "ci", &case.root().join("runtime"));
        let receipt = outcome.getattr("receipt").unwrap();
        assert_eq!(attr_text(&receipt, "decision"), "fail");
        let critical = outcome
            .getattr("results")
            .unwrap()
            .try_iter()
            .unwrap()
            .any(|row| {
                row.unwrap()
                    .getattr("findings")
                    .unwrap()
                    .try_iter()
                    .unwrap()
                    .any(|f| {
                        let f = f.unwrap();
                        attr_text(&f, "rule_id") == "empty-ci-range"
                            && attr_text(&f, "severity") == "critical"
                    })
            });
        assert!(critical);
        assert_eq!(
            receipt
                .getattr("candidate")
                .unwrap()
                .get_item("tree_oid")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            tree_oid(&candidate)
        );
        let payload = receipt.call_method0("to_dict").unwrap();
        let valid = module(py, "conductor.candidate_review.engine")
            .getattr("verify_receipt_payload")
            .unwrap()
            .call1((payload,))
            .unwrap();
        assert_eq!(
            (
                valid.get_item(0).unwrap().extract::<bool>().unwrap(),
                valid.get_item(1).unwrap().extract::<String>().unwrap()
            ),
            (true, "ok".into())
        );
    });
}

fn policy_findings(outcome: &Bound<'_, PyAny>) -> BTreeSet<(String, String, String)> {
    let mut rows = BTreeSet::new();
    for result in outcome.getattr("results").unwrap().try_iter().unwrap() {
        let result = result.unwrap();
        if matches!(
            attr_text(&result, "check_id").as_str(),
            "engine-integrity" | "attestation"
        ) {
            continue;
        }
        for finding in result.getattr("findings").unwrap().try_iter().unwrap() {
            let finding = finding.unwrap();
            rows.insert((
                attr_text(&finding, "check_id"),
                attr_text(&finding, "rule_id"),
                attr_text(&finding, "fingerprint"),
            ));
        }
    }
    rows
}

#[test]
fn local_and_ci_policy_findings_are_parity_bound() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::install_candidate_engine(&repo);
    Python::attach(|py| {
        fixture::write_policy(py, &repo, "conductor/candidate_policy.toml");
        let base = fixture::commit_all(&repo, "governance baseline");
        fixture::write_fixture(&repo, "README.md", "candidate docs\n");
        fixture::git(&repo, &["add", "README.md"]);
        let local_candidate = fixture::resolve_candidate(py, &repo, "index", Some(&base), None);
        let local = fixture_review(
            py,
            &repo,
            &local_candidate,
            "pre-commit",
            &case.root().join("local-runtime"),
        );
        let commit = fixture::commit_all(&repo, "candidate");
        let ci_candidate =
            fixture::resolve_candidate(py, &repo, "range", Some(&base), Some(&commit));
        let ci = fixture_review(
            py,
            &repo,
            &ci_candidate,
            "ci",
            &case.root().join("ci-runtime"),
        );
        let local_receipt = local.getattr("receipt").unwrap();
        let ci_receipt = ci.getattr("receipt").unwrap();
        assert!(local_receipt
            .getattr("candidate")
            .unwrap()
            .get_item("tree_oid")
            .unwrap()
            .eq(ci_receipt
                .getattr("candidate")
                .unwrap()
                .get_item("tree_oid")
                .unwrap())
            .unwrap());
        assert!(local_receipt
            .getattr("policy")
            .unwrap()
            .get_item("sha256")
            .unwrap()
            .eq(ci_receipt
                .getattr("policy")
                .unwrap()
                .get_item("sha256")
                .unwrap())
            .unwrap());
        assert_eq!(policy_findings(&local), policy_findings(&ci));
        assert_eq!(attr_text(&local_receipt, "decision"), "pass");
        assert_eq!(attr_text(&ci_receipt, "decision"), "pass");
    });
}

#[test]
fn analyzer_git_mutation_cannot_rebind_shared_worktree() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_fixture(&repo, "candidate.txt", "candidate\n");
    fixture::commit_all(&repo, "candidate");
    Python::attach(|py| {
        let candidate = fixture::resolve_candidate(py, &repo, "index", None, None);
        fixture::with_materialized(
            py,
            &repo,
            &tree_oid(&candidate),
            None,
            |snapshot, entries| {
                let policy_file = fixture::write_policy(py, &repo, "policy.toml");
                let policy = fixture::load_policy(py, &policy_file);
                let context = configured_context(
                    py,
                    &repo,
                    snapshot,
                    entries,
                    &candidate,
                    &policy,
                    "pre-commit",
                    "fast",
                    case.root(),
                );
                let runner = module(py, "conductor.candidate_review.command_runner");
                runner
                    .getattr("prepare_candidate_git_environment")
                    .unwrap()
                    .call1((&context,))
                    .unwrap();
                let env: HashMap<String, String> = runner
                    .getattr("_environment")
                    .unwrap()
                    .call1((&context,))
                    .unwrap()
                    .extract()
                    .unwrap();
                let completed = Command::new("git")
                    .args(["config", "core.worktree", "/tmp/analyzer-poison"])
                    .current_dir(snapshot)
                    .env_clear()
                    .envs(env)
                    .output()
                    .unwrap();
                assert!(
                    completed.status.success(),
                    "{}",
                    String::from_utf8_lossy(&completed.stderr)
                );
            },
        );
    });
    let shared = Command::new("git")
        .args(["config", "--local", "--get", "core.worktree"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(shared.status.code(), Some(1));
}

#[test]
fn adversarial_builtin_matrix_exercises_real_candidate_flows() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    fixture::write_adversarial_sources(&repo);
    Python::attach(|py| {
        let kwargs = PyDict::new(py);
        kwargs.set_item("max_file_bytes", 200).unwrap();
        kwargs.set_item("max_binary_bytes", 1).unwrap();
        let policy = fixture::replace_fields(py, &fixture::default_policy(py), &kwargs);
        let candidate = fixture::classify_candidate(
            py,
            &fixture::resolve_candidate(py, &repo, "index", None, None),
            &policy,
        );
        let selection = fixture::test_selection(py, &[]);
        fixture::with_materialized(
            py,
            &repo,
            &tree_oid(&candidate),
            None,
            |snapshot, entries| {
                let context = configured_context(
                    py,
                    &repo,
                    snapshot,
                    entries,
                    &candidate,
                    &policy,
                    "ci",
                    "full",
                    case.root(),
                );
                assert_adversarial_results(py, &context, &selection, &policy);
            },
        );
    });
}

fn assert_adversarial_results(
    py: Python<'_>,
    context: &Bound<'_, PyAny>,
    selection: &Bound<'_, PyAny>,
    policy: &Bound<'_, PyAny>,
) {
    let checks = module(py, "conductor.candidate_review.checks");
    let mut rules = BTreeSet::new();
    for name in [
        "check_candidate_integrity",
        "check_config_and_notebooks",
        "check_secrets",
        "check_python_ast",
        "check_dependency_integrity",
        "check_performance_evidence",
        "check_research_evidence",
        "check_native_source",
        "check_duplicate_function_bodies",
    ] {
        let args = if matches!(
            name,
            "check_performance_evidence" | "check_research_evidence"
        ) {
            PyTuple::new(py, [context.clone(), selection.clone()]).unwrap()
        } else {
            PyTuple::new(py, [context.clone()]).unwrap()
        };
        let result = checks.getattr(name).unwrap().call1(args).unwrap();
        rules.extend(finding_rules(&result));
    }
    let expected = BTreeSet::from([
        "protected-delete-or-move",
        "binary-admission",
        "oversized-artifact",
        "malformed-config",
        "notebook-output",
        "generic-api-key",
        "pass-stub",
        "ellipsis-stub",
        "dynamic-execution",
        "unsafe-deserialization",
        "unsafe-yaml",
        "unsafe-shell",
        "not-implemented-stub",
        "softmax-shaped-fallback",
        "nondeterministic-research",
        "partial-promotion-write",
        "missing-lockfile",
        "missing-performance-budget",
        "python-only-hotpath",
        "incomplete-result-provenance",
        "missing-numerical-device-tests",
        "unsafe-native-api",
        "copied-function-body",
    ])
    .into_iter()
    .map(str::to_owned)
    .collect::<BTreeSet<_>>();
    assert!(
        expected.is_subset(&rules),
        "missing rules: {:?}",
        expected.difference(&rules)
    );
    let command = policy
        .getattr("checks")
        .unwrap()
        .try_iter()
        .unwrap()
        .map(Result::unwrap)
        .find(|check| attr_text(check, "kind") == "command")
        .unwrap();
    assert!(
        checks
            .getattr("files_for_policy")
            .unwrap()
            .call1((context, &command))
            .unwrap()
            .len()
            .unwrap()
            > 0
    );
    let kwargs = PyDict::new(py);
    kwargs.set_item("check_id", "missing-builtin").unwrap();
    kwargs.set_item("kind", "builtin").unwrap();
    let unknown = fixture::replace_fields(py, &command, &kwargs);
    let result = checks
        .getattr("run_builtin")
        .unwrap()
        .call1((context, unknown))
        .unwrap();
    assert_eq!(finding_rules(&result).first().unwrap(), "unknown-builtin");
}

#[test]
fn dynamic_execution_gate_distinguishes_builtins_from_method_calls() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    let lines = [
        "import os",
        "",
        "",
        "def check_host(value):",
        "    return os.system(value)",
        "",
        "",
        "def refresh(model):",
        "    return model.eval()",
        "",
        "",
        "make().eval()",
        "eval_used = eval('1')",
        "exec_used = exec('pass')",
    ];
    fixture::write_fixture(&repo, "probe.py", &(lines.join("\n") + "\n"));
    fixture::git(&repo, &["add", "--all"]);
    Python::attach(|py| {
        let policy = fixture::default_policy(py);
        let candidate = fixture::classify_candidate(
            py,
            &fixture::resolve_candidate(py, &repo, "index", None, None),
            &policy,
        );
        fixture::with_materialized(
            py,
            &repo,
            &tree_oid(&candidate),
            None,
            |snapshot, entries| {
                let context = configured_context(
                    py,
                    &repo,
                    snapshot,
                    entries,
                    &candidate,
                    &policy,
                    "ci",
                    "full",
                    case.root(),
                );
                let result = module(py, "conductor.candidate_review.checks")
                    .getattr("check_python_ast")
                    .unwrap()
                    .call1((context,))
                    .unwrap();
                let dynamic: Vec<_> = result
                    .getattr("findings")
                    .unwrap()
                    .try_iter()
                    .unwrap()
                    .map(Result::unwrap)
                    .filter(|f| attr_text(f, "rule_id") == "dynamic-execution")
                    .collect();
                let mut messages: Vec<String> =
                    dynamic.iter().map(|f| attr_text(f, "message")).collect();
                messages.sort();
                assert_eq!(
                    messages,
                    [
                        "unsafe dynamic execution via eval",
                        "unsafe dynamic execution via exec",
                        "unsafe dynamic execution via os.system"
                    ]
                );
                let unflagged = lines
                    .iter()
                    .position(|line| *line == "make().eval()")
                    .unwrap()
                    + 1;
                assert!(dynamic.iter().all(|f| f
                    .getattr("line")
                    .unwrap()
                    .extract::<usize>()
                    .unwrap()
                    != unflagged));
            },
        );
    });
}

#[test]
fn protocol_ellipsis_methods_are_not_flagged_as_stubs() {
    let case = fixture::isolated_case();
    let repo = case.root().join("repo");
    fixture::init_repo(&repo);
    let source = concat!(
        "from typing import Protocol\n\n\nclass Sink(Protocol):\n",
        "    def put(self, key: str, value: int) -> None:\n        ...\n\n",
        "    @property\n    def size(self) -> int:\n        ...\n\n\n",
        "class Nested(Protocol):\n    class Inner(Protocol):\n",
        "        def deep(self) -> str:\n            ...\n\n",
        "    def flat(self) -> None:\n        ...\n\n",
        "    class Concrete:\n        def inner_stub(self): ...\n\n\n",
        "class Impl:\n    def real(self):\n        return 1\n\n    def missing(self): ...\n"
    );
    fixture::write_fixture(&repo, "protocol_probe.py", source);
    fixture::git(&repo, &["add", "--all"]);
    Python::attach(|py| {
        let policy = fixture::default_policy(py);
        let candidate = fixture::classify_candidate(
            py,
            &fixture::resolve_candidate(py, &repo, "index", None, None),
            &policy,
        );
        fixture::with_materialized(
            py,
            &repo,
            &tree_oid(&candidate),
            None,
            |snapshot, entries| {
                let context = configured_context(
                    py,
                    &repo,
                    snapshot,
                    entries,
                    &candidate,
                    &policy,
                    "ci",
                    "full",
                    case.root(),
                );
                let result = module(py, "conductor.candidate_review.checks")
                    .getattr("check_python_ast")
                    .unwrap()
                    .call1((context,))
                    .unwrap();
                let flagged: Vec<usize> = result
                    .getattr("findings")
                    .unwrap()
                    .try_iter()
                    .unwrap()
                    .map(Result::unwrap)
                    .filter(|f| attr_text(f, "rule_id") == "ellipsis-stub")
                    .map(|f| f.getattr("line").unwrap().extract().unwrap())
                    .collect();
                let expected: Vec<usize> = ["def inner_stub", "def missing"]
                    .iter()
                    .map(|marker| {
                        source[..source.find(marker).unwrap()]
                            .bytes()
                            .filter(|byte| *byte == b'\n')
                            .count()
                            + 1
                    })
                    .collect();
                assert_eq!(flagged, expected);
                assert_eq!(flagged.len(), 2);
            },
        );
    });
}

#[test]
fn analyzer_reporting_includes_stdout_alongside_warning_stderr() {
    let case = fixture::isolated_case();
    let repo = repo_with_staged_probe(&case);
    Python::attach(|py| {
        let policy = fixture::default_policy(py);
        let candidate = fixture::classify_candidate(
            py,
            &fixture::resolve_candidate(py, &repo, "index", None, None),
            &policy,
        );
        fixture::with_materialized(
            py,
            &repo,
            &tree_oid(&candidate),
            None,
            |snapshot, entries| {
                let context = configured_context(
                    py,
                    &repo,
                    snapshot,
                    entries,
                    &candidate,
                    &policy,
                    "pre-commit",
                    "fast",
                    case.root(),
                );
                let template = policy
                    .getattr("checks")
                    .unwrap()
                    .try_iter()
                    .unwrap()
                    .map(Result::unwrap)
                    .find(|c| attr_text(c, "kind") == "command")
                    .unwrap();
                let exe = module(py, "sys").getattr("executable").unwrap();
                let kwargs = PyDict::new(py);
                kwargs.set_item("check_id", "analyzer-probe").unwrap();
                kwargs.set_item("classes", PyTuple::empty(py)).unwrap();
                kwargs.set_item("always", true).unwrap();
                kwargs.set_item("command", PyTuple::new(py, [exe.clone(), fixture::py_string(py, "-c"),
                fixture::py_string(py, "import sys; print('FINDINGS LIVE ON STDOUT'); sys.stderr.write('UserWarning: stale warning\\n'); sys.exit(3)")]).unwrap()).unwrap();
                kwargs
                    .set_item(
                        "version_command",
                        PyTuple::new(py, [exe, fixture::py_string(py, "--version")]).unwrap(),
                    )
                    .unwrap();
                let check = fixture::replace_fields(py, &template, &kwargs);
                let run_kwargs = PyDict::new(py);
                run_kwargs.set_item("version", "pinned-analyzer").unwrap();
                let result = module(py, "conductor.candidate_review.command_runner")
                    .getattr("run_command_check")
                    .unwrap()
                    .call((&context, check), Some(&run_kwargs))
                    .unwrap();
                assert_eq!(attr_text(&result, "status"), "failed");
                let findings = result.getattr("findings").unwrap();
                assert_eq!(findings.len().unwrap(), 1);
                let finding = findings.get_item(0).unwrap();
                assert_eq!(attr_text(&finding, "rule_id"), "analyzer-finding");
                assert_eq!(attr_text(&finding, "severity"), "high");
                let message = attr_text(&finding, "message");
                assert!(message.contains("FINDINGS LIVE ON STDOUT"));
                assert!(message.contains("UserWarning"));
                assert!(message.find("FINDINGS LIVE ON STDOUT") < message.find("UserWarning"));
                assert_eq!(
                    result
                        .getattr("exit_code")
                        .unwrap()
                        .extract::<i32>()
                        .unwrap(),
                    3
                );
            },
        );
    });
}

fn command_contract(py: Python<'_>, context: &Bound<'_, PyAny>, policy: &Bound<'_, PyAny>) {
    let runner = module(py, "conductor.candidate_review.command_runner");
    runner
        .getattr("prepare_candidate_git_environment")
        .unwrap()
        .call1((context,))
        .unwrap();
    let template = policy
        .getattr("checks")
        .unwrap()
        .try_iter()
        .unwrap()
        .map(Result::unwrap)
        .find(|c| attr_text(c, "kind") == "command")
        .unwrap();
    let exe = module(py, "sys").getattr("executable").unwrap();
    let kwargs = PyDict::new(py);
    kwargs.set_item("check_id", "command-probe").unwrap();
    kwargs.set_item("classes", PyTuple::empty(py)).unwrap();
    kwargs.set_item("always", true).unwrap();
    kwargs
        .set_item(
            "command",
            PyTuple::new(
                py,
                [
                    exe.clone(),
                    fixture::py_string(py, "-c"),
                    fixture::py_string(py, "print('command-ok')"),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    kwargs
        .set_item(
            "version_command",
            PyTuple::new(py, [exe.clone(), fixture::py_string(py, "--version")]).unwrap(),
        )
        .unwrap();
    let passing = fixture::replace_fields(py, &template, &kwargs);
    let version = runner
        .getattr("tool_version")
        .unwrap()
        .call1((context, &passing))
        .unwrap();
    assert!(!version
        .get_item(0)
        .unwrap()
        .extract::<String>()
        .unwrap()
        .is_empty());
    assert!(version.get_item(1).unwrap().is_none());
    let value = version.get_item(0).unwrap();
    let options = PyDict::new(py);
    options.set_item("version", &value).unwrap();
    let run = runner.getattr("run_command_check").unwrap();
    let result = run.call((context, &passing), Some(&options)).unwrap();
    assert_eq!(attr_text(&result, "status"), "passed");
    assert_eq!(attr_text(&result, "stdout_tail").trim(), "command-ok");
    kwargs
        .set_item(
            "command",
            PyTuple::new(
                py,
                [
                    exe,
                    fixture::py_string(py, "-c"),
                    fixture::py_string(py, "import sys; sys.exit(5)"),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    let failing = fixture::replace_fields(py, &passing, &kwargs);
    assert_eq!(
        finding_rules(&run.call((context, failing), Some(&options)).unwrap())[0],
        "analyzer-finding"
    );
    let unavailable = PyDict::new(py);
    unavailable
        .set_item("version_error", "missing pinned tool")
        .unwrap();
    assert_eq!(
        finding_rules(&run.call((context, &passing), Some(&unavailable)).unwrap())[0],
        "required-analyzer-unavailable"
    );
    let material = runner
        .getattr("command_cache_material")
        .unwrap()
        .call1((context, &passing, value, ["probe.py"]))
        .unwrap();
    assert!(material.get_item("files").unwrap().len().unwrap() > 0);
}

fn cache_mutex_contract(py: Python<'_>, root: &Path, repo: &Path) {
    let engine = module(py, "conductor.candidate_review.engine");
    let kwargs = PyDict::new(py);
    kwargs.set_item("ttl_days", 1).unwrap();
    let cache = engine
        .getattr("ResultCache")
        .unwrap()
        .call((path(py, &root.join("cache")),), Some(&kwargs))
        .unwrap();
    let model = module(py, "conductor.candidate_review.model");
    let severity = model.getattr("Severity").unwrap().getattr("INFO").unwrap();
    let finding = model
        .getattr("Finding")
        .unwrap()
        .call1(("cache-probe", "evidence", severity, "cache round trip"))
        .unwrap()
        .call_method0("finalize")
        .unwrap();
    let status = model
        .getattr("CheckStatus")
        .unwrap()
        .getattr("PASSED")
        .unwrap();
    let options = PyDict::new(py);
    options
        .set_item("findings", PyList::new(py, [finding]).unwrap())
        .unwrap();
    let result = model
        .getattr("CheckResult")
        .unwrap()
        .call(("cache-probe", status, 3), Some(&options))
        .unwrap();
    let key = "a".repeat(64);
    cache.call_method1("store", (&key, result)).unwrap();
    let loaded = cache.call_method1("load", (&key,)).unwrap();
    assert!(!loaded.is_none());
    assert!(loaded
        .getattr("cache_hit")
        .unwrap()
        .extract::<bool>()
        .unwrap());
    assert_eq!(
        attr_text(
            &loaded.getattr("findings").unwrap().get_item(0).unwrap(),
            "message"
        ),
        "cache round trip"
    );
    let lock = engine.getattr("governance_lock").unwrap();
    let exclusive = PyDict::new(py);
    exclusive.set_item("exclusive", true).unwrap();
    let first = lock.call((path(py, repo),), Some(&exclusive)).unwrap();
    first.call_method0("__enter__").unwrap();
    let nested = PyDict::new(py);
    nested.set_item("exclusive", true).unwrap();
    nested.set_item("timeout_seconds", 0).unwrap();
    let second = lock.call((path(py, repo),), Some(&nested)).unwrap();
    let error = second.call_method0("__enter__").unwrap_err();
    assert_error(
        py,
        error,
        &module(py, "builtins").getattr("TimeoutError").unwrap(),
        "mutex remained busy",
    );
    first
        .call_method1("__exit__", (py.None(), py.None(), py.None()))
        .unwrap();
}

fn receipt_attestation_contract(
    py: Python<'_>,
    root: &Path,
    repo: &Path,
    candidate: &Bound<'_, PyAny>,
    policy: &Bound<'_, PyAny>,
    context: &Bound<'_, PyAny>,
) {
    let engine = module(py, "conductor.candidate_review.engine");
    let model = module(py, "conductor.candidate_review.model");
    let receipt = fixture::fixture_receipt(py);
    receipt.setattr("surface", "pre-commit").unwrap();
    receipt.setattr("profile", "fast").unwrap();
    receipt.setattr("decision", "pass").unwrap();
    receipt
        .getattr("candidate")
        .unwrap()
        .set_item("tree_oid", tree_oid(candidate))
        .unwrap();
    let policy_sha = PyDict::new(py);
    policy_sha
        .set_item("sha256", policy.getattr("digest").unwrap())
        .unwrap();
    receipt.setattr("policy", policy_sha).unwrap();
    model
        .getattr("seal_receipt")
        .unwrap()
        .call1((&receipt,))
        .unwrap();
    let receipt_file = engine
        .getattr("receipt_path")
        .unwrap()
        .call1((path(py, repo), "pre-commit", candidate, "fast"))
        .unwrap();
    model
        .getattr("write_json_atomic")
        .unwrap()
        .call1((&receipt_file, receipt.call_method0("to_dict").unwrap()))
        .unwrap();
    let matched = engine
        .getattr("_matching_precommit_receipt")
        .unwrap()
        .call1((context,))
        .unwrap();
    assert_eq!(
        matched.get_item(1).unwrap().extract::<String>().unwrap(),
        "ok"
    );
    assert_eq!(
        matched
            .get_item(0)
            .unwrap()
            .get_item("receipt_id")
            .unwrap()
            .extract::<String>()
            .unwrap(),
        attr_text(&receipt, "receipt_id")
    );
    let tampered = receipt.call_method0("to_dict").unwrap();
    tampered.set_item("receipt_digest", "0".repeat(64)).unwrap();
    model
        .getattr("write_json_atomic")
        .unwrap()
        .call1((&receipt_file, tampered))
        .unwrap();
    assert!(engine
        .getattr("_matching_precommit_receipt")
        .unwrap()
        .call1((context,))
        .unwrap()
        .get_item(0)
        .unwrap()
        .is_none());
    model
        .getattr("write_json_atomic")
        .unwrap()
        .call1((&receipt_file, receipt.call_method0("to_dict").unwrap()))
        .unwrap();
    assert_attestation_contract(py, root, repo, candidate, &engine);
}

fn assert_attestation_contract(
    py: Python<'_>,
    root: &Path,
    repo: &Path,
    candidate: &Bound<'_, PyAny>,
    engine: &Bound<'_, pyo3::types::PyModule>,
) {
    let message = root.join("COMMIT_EDITMSG");
    fs::write(&message, "test: candidate\n").unwrap();
    let trailers = engine
        .getattr("append_attestation")
        .unwrap()
        .call1((path(py, &message), path(py, repo)))
        .unwrap();
    assert_eq!(
        trailers
            .get_item("Governance-Tree")
            .unwrap()
            .extract::<String>()
            .unwrap(),
        tree_oid(candidate)
    );
    assert!(engine
        .getattr("append_attestation")
        .unwrap()
        .call1((path(py, &message), path(py, repo)))
        .unwrap()
        .eq(&trailers)
        .unwrap());
    assert_error(
        py,
        engine
            .getattr("run_locked_git_commit")
            .unwrap()
            .call1((path(py, repo), ["status"]))
            .unwrap_err(),
        &module(py, "builtins").getattr("ValueError").unwrap(),
        "beginning with 'commit'",
    );
}

#[test]
fn command_cache_mutex_and_attestation_contracts() {
    let case = fixture::isolated_case();
    let repo = repo_with_staged_probe(&case);
    Python::attach(|py| {
        let policy = fixture::default_policy(py);
        let candidate = fixture::classify_candidate(
            py,
            &fixture::resolve_candidate(py, &repo, "index", None, None),
            &policy,
        );
        let context = fixture::with_materialized(
            py,
            &repo,
            &tree_oid(&candidate),
            None,
            |snapshot, entries| {
                let context = configured_context(
                    py,
                    &repo,
                    snapshot,
                    entries,
                    &candidate,
                    &policy,
                    "pre-commit",
                    "fast",
                    case.root(),
                );
                command_contract(py, &context, &policy);
                context
            },
        );
        cache_mutex_contract(py, case.root(), &repo);
        receipt_attestation_contract(py, case.root(), &repo, &candidate, &policy, &context);
    });
}
