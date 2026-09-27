#![cfg(feature = "python-compat-tests")]
//! Rust assertions replacing policy exception and value-waiver Python cases.

#[path = "python_contracts/candidate_review_support.rs"]
#[allow(dead_code)]
mod candidate_review_support;
#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyModule, PySet, PyTuple};
use std::path::Path;
use std::process::Command;
use support::{attr_text, module, path, text, Case};

const BASE: &str = "dddddddddddddddddddddddddddddddddddddddd";
const NODEID: &str = "conductor/test_repo_index.py::test_the_index_is_not_degenerate";
const REASON: &str = "Rust engine pinned behaviour requires a bounded exception";
const PROBE: &str = "research/tests/test_probe.py";
const NEW_NODEID: &str = "research/tests/test_probe.py::test_probe_new";
const POLICY: &str = r#"schema_version = 1
block_at = "high"
max_workers = 1
cache_ttl_days = 1
claim_max_age_hours = 1
max_file_bytes = 1000000
max_binary_bytes = 1000000
coverage_threshold = 75.0
high_risk_coverage_threshold = 90.0
baseline_expires = 2099-01-01

[classes]

[risk]
high = []

[paths]
protected_deletes = []
hot = []
generated = []

[checks.candidate-integrity]
kind = "builtin"
profiles = ["fast", "full"]
classes = []
severity = "critical"
always = true

[checks.secret-scan]
kind = "builtin"
profiles = ["fast", "full"]
classes = []
severity = "critical"
always = true
"#;

fn shifted_date<'py>(py: Python<'py>, days: i64) -> Bound<'py, PyAny> {
    let datetime = PyModule::import(py, "datetime").unwrap();
    let today = datetime
        .getattr("date")
        .unwrap()
        .call_method0("today")
        .unwrap();
    let delta = datetime
        .getattr("timedelta")
        .unwrap()
        .call1((days,))
        .unwrap();
    today.call_method1("__add__", (delta,)).unwrap()
}

fn exception_entry(id: &str, check: &str, path: &str, rule: Option<&str>, expires: &str) -> String {
    let rule_field = rule
        .map(|rule| format!("rule = {rule:?}, "))
        .unwrap_or_default();
    format!("{{ id = {id:?}, check = {check:?}, {rule_field}path = {path:?}, owner = \"grok\", justification = \"covers a finding that may no longer exist\", expires = {expires} }}")
}

fn loaded_policy<'py>(py: Python<'py>, case: &Case, entries: &[String]) -> Bound<'py, PyAny> {
    let with_entries = POLICY.replace(
        "[classes]",
        &format!("exceptions = [{}]\n\n[classes]", entries.join(",\n")),
    );
    let fixture = case.write("candidate_policy.toml", &with_entries);
    module(py, "conductor.candidate_review.policy")
        .getattr("load_policy")
        .unwrap()
        .call1((path(py, &fixture),))
        .unwrap()
}

fn finding<'py>(py: Python<'py>, check: &str, nodeid: &str) -> Bound<'py, PyAny> {
    let model = module(py, "conductor.candidate_review.model");
    let severity = model
        .getattr("Severity")
        .unwrap()
        .getattr("CRITICAL")
        .unwrap();
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("path", "conductor/test_repo_index.py")
        .unwrap();
    let evidence = PyDict::new(py);
    evidence.set_item("nodeid", nodeid).unwrap();
    kwargs.set_item("evidence", evidence).unwrap();
    model.getattr("Finding").unwrap()
        .call((check, "new-test-value-not-admitted", severity,
               format!("conductor/test_repo_index.py: new test '{nodeid}' is classified 'DELETE_CANDIDATE'")), Some(&kwargs))
        .unwrap()
}

fn waiver<'py>(py: Python<'py>, nodeids: &[&str], expiry: Option<i64>) -> Bound<'py, PyAny> {
    let policy = module(py, "conductor.candidate_review.policy");
    policy
        .getattr("ValueWaiverPolicy")
        .unwrap()
        .call1((
            BASE,
            nodeids,
            REASON,
            "Tim",
            shifted_date(py, -1),
            expiry.map(|days| shifted_date(py, days)),
        ))
        .unwrap()
}

fn replace_field<'py>(
    py: Python<'py>,
    object: &Bound<'py, PyAny>,
    field: &str,
    value: &Bound<'py, PyAny>,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item(field, value).unwrap();
    PyModule::import(py, "dataclasses")
        .unwrap()
        .getattr("replace")
        .unwrap()
        .call((object,), Some(&kwargs))
        .unwrap()
}

fn receipt<'py>(
    py: Python<'py>,
    stale: &Bound<'py, PyAny>,
    findings: &Bound<'py, PyAny>,
) -> Bound<'py, PyAny> {
    let model = module(py, "conductor.candidate_review.model");
    let kwargs = PyDict::new(py);
    kwargs.set_item("schema_version", 1).unwrap();
    for (key, value) in [
        (
            "receipt_id",
            pyo3::types::PyString::new(py, "rcpt-0001").into_any(),
        ),
        (
            "receipt_digest",
            pyo3::types::PyString::new(py, "digest").into_any(),
        ),
        (
            "surface",
            pyo3::types::PyString::new(py, "manual").into_any(),
        ),
        ("profile", pyo3::types::PyString::new(py, "fast").into_any()),
        (
            "decision",
            pyo3::types::PyString::new(py, "pass").into_any(),
        ),
        (
            "binding",
            pyo3::types::PyString::new(py, "candidate").into_any(),
        ),
    ] {
        kwargs.set_item(key, value).unwrap();
    }
    let candidate = PyDict::new(py);
    candidate
        .set_item("tree_oid", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .unwrap();
    kwargs.set_item("candidate", candidate).unwrap();
    let policy = PyDict::new(py);
    policy.set_item("unmatched_exceptions", stale).unwrap();
    kwargs.set_item("policy", policy).unwrap();
    for key in ["engine", "graph", "bypass"] {
        kwargs.set_item(key, PyDict::new(py)).unwrap();
    }
    let timings = PyDict::new(py);
    timings.set_item("duration_ms", 12).unwrap();
    kwargs.set_item("timings", timings).unwrap();
    let cache = PyDict::new(py);
    cache.set_item("hits", 0).unwrap();
    kwargs.set_item("cache", cache).unwrap();
    kwargs.set_item("baselines", PyList::empty(py)).unwrap();
    kwargs.set_item("checks", PyList::empty(py)).unwrap();
    kwargs.set_item("findings", findings).unwrap();
    model
        .getattr("ReviewReceipt")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn branched_repo(case: &Case) -> (std::path::PathBuf, String, String) {
    let repo = case.mkdir("repo");
    git(&repo, &["init", "-q", "-b", "master"]);
    git(&repo, &["config", "user.email", "gate@example.invalid"]);
    git(&repo, &["config", "user.name", "gate"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["config", "core.hooksPath", "/dev/null"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-qm", "integration base"]);
    let integration = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["checkout", "-q", "-b", "lane"]);
    std::fs::write(repo.join("a.txt"), "two\n").unwrap();
    git(&repo, &["commit", "-qam", "lane commit"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("b.txt"), "staged\n").unwrap();
    git(&repo, &["add", "b.txt"]);
    (repo, integration, head)
}

#[test]
fn exception_staleness_requires_own_check_and_examined_path() {
    let case = Case::new();
    Python::attach(|py| {
        let expires = text(&shifted_date(py, 30));
        let policy = loaded_policy(
            py,
            &case,
            &[
                exception_entry(
                    "read-and-excused-nothing",
                    "candidate-integrity",
                    "conductor/target.py",
                    None,
                    &expires,
                ),
                exception_entry(
                    "check-read-other-files",
                    "candidate-integrity",
                    "conductor/other.py",
                    None,
                    &expires,
                ),
                exception_entry(
                    "another-checks-exemption",
                    "secret-scan",
                    "conductor/target.py",
                    None,
                    &expires,
                ),
            ],
        );
        let examined = PyDict::new(py);
        examined
            .set_item("candidate-integrity", ["conductor/target.py"])
            .unwrap();
        let result = module(py, "conductor.candidate_review.policy")
            .getattr("unmatched_exceptions")
            .unwrap()
            .call1((&policy, &examined, PyList::empty(py)))
            .unwrap();
        let stale = result.cast::<PyTuple>().unwrap();
        assert_eq!(stale.len(), 1);
        let row = stale.get_item(0).unwrap();
        assert_eq!(
            text(&row.get_item("id").unwrap()),
            "read-and-excused-nothing"
        );
        assert_eq!(text(&row.get_item("check").unwrap()), "candidate-integrity");
        assert_eq!(text(&row.get_item("owner").unwrap()), "grok");
        assert_eq!(text(&row.get_item("expires").unwrap()), expires);
        assert_eq!(text(&row.get_item("rule").unwrap()), "");
    });
}

#[test]
fn exception_finding_match_glob_and_rule_are_auditable() {
    let case = Case::new();
    Python::attach(|py| {
        let expires = text(&shifted_date(py, 30));
        let policy = loaded_policy(
            py,
            &case,
            &[exception_entry(
                "globbed",
                "candidate-integrity",
                "conductor/candidate_review/mutation_*.py",
                Some("oversized-function"),
                &expires,
            )],
        );
        let examined = PyDict::new(py);
        examined
            .set_item(
                "candidate-integrity",
                ["conductor/candidate_review/mutation_testing.py"],
            )
            .unwrap();
        let policy_module = module(py, "conductor.candidate_review.policy");
        let unmatched = policy_module.getattr("unmatched_exceptions").unwrap();
        let empty = PyList::empty(py);
        let stale = unmatched.call1((&policy, &examined, &empty)).unwrap();
        let row = stale.get_item(0).unwrap();
        assert_eq!(text(&row.get_item("id").unwrap()), "globbed");
        assert_eq!(text(&row.get_item("rule").unwrap()), "oversized-function");
        let model = module(py, "conductor.candidate_review.model");
        let severity = model
            .getattr("Severity")
            .unwrap()
            .getattr("CRITICAL")
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs
            .set_item("path", "conductor/candidate_review/mutation_testing.py")
            .unwrap();
        kwargs.set_item("exception_id", "globbed").unwrap();
        let excused = model
            .getattr("Finding")
            .unwrap()
            .call(
                (
                    "candidate-integrity",
                    "oversized-function",
                    severity,
                    "bounded finding",
                ),
                Some(&kwargs),
            )
            .unwrap();
        let findings = PyList::new(py, [&excused]).unwrap();
        assert_eq!(
            unmatched
                .call1((&policy, &examined, findings))
                .unwrap()
                .len()
                .unwrap(),
            0
        );
    });
}

#[test]
fn value_waivers_require_base_expiry_and_exact_nodeid() {
    let _case = Case::new();
    Python::attach(|py| {
        let waivers = module(py, "conductor.candidate_review.value_waivers");
        let apply = waivers.getattr("apply_value_waivers").unwrap();
        let today = shifted_date(py, 0);
        let longer = format!("{NODEID}_more");
        for (nodeids, expiry, base) in [
            (
                vec![NODEID],
                Some(30),
                "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            ),
            (vec![NODEID], Some(-1), BASE),
            (vec![NODEID], None, BASE),
            (vec![&NODEID[..NODEID.len() - 4]], Some(30), BASE),
            (vec![longer.as_str()], Some(30), BASE),
            (vec![], Some(30), BASE),
        ] {
            let finding = finding(py, "mutation-evidence", NODEID);
            let kwargs = PyDict::new(py);
            kwargs.set_item("base", base).unwrap();
            kwargs.set_item("today", &today).unwrap();
            let result = apply
                .call(
                    (vec![finding], vec![waiver(py, &nodeids, expiry)]),
                    Some(&kwargs),
                )
                .unwrap();
            let row = result.get_item(0).unwrap();
            assert_eq!(attr_text(&row, "rule_id"), "new-test-value-not-admitted");
            assert_eq!(attr_text(&row, "severity"), "critical");
        }
        let kwargs = PyDict::new(py);
        kwargs.set_item("base", BASE).unwrap();
        kwargs.set_item("today", &today).unwrap();
        let result = apply
            .call(
                (
                    vec![finding(py, "mutation-evidence", NODEID)],
                    vec![waiver(py, &[NODEID], Some(30))],
                ),
                Some(&kwargs),
            )
            .unwrap();
        let row = result.get_item(0).unwrap();
        assert_eq!(attr_text(&row, "rule_id"), "new-test-value-waived");
        assert_eq!(attr_text(&row, "severity"), "info");
        assert!(attr_text(&row, "message").starts_with("WAIVED conductor/test_repo_index.py:"));
        assert!(attr_text(&row, "message").contains(REASON));
        assert!(attr_text(&row, "message").contains("approved by Tim on"));
        assert_eq!(
            text(&row.getattr("evidence").unwrap().get_item("nodeid").unwrap()),
            NODEID
        );
        assert_eq!(
            text(
                &row.getattr("evidence")
                    .unwrap()
                    .get_item("waived_by")
                    .unwrap()
                    .get_item("integration_base")
                    .unwrap()
            ),
            BASE
        );
    });
}

#[test]
fn baseline_receipt_requires_matching_profile_class_and_file() {
    let case = Case::new();
    let baseline_file = case.write("baseline.json", "{}\n");
    Python::attach(|py| {
        let policy_module = module(py, "conductor.candidate_review.policy");
        let policy = loaded_policy(py, &case, &[]);
        let baseline = policy_module
            .getattr("BaselinePolicy")
            .unwrap()
            .call1(("probe", "baseline.json", ("python",), ("full",)))
            .unwrap();
        let baselines = PyTuple::new(py, [baseline]).unwrap();
        let bounded = replace_field(py, &policy, "baselines", baselines.as_any());
        let classes = PySet::new(py, ["python"]).unwrap();
        let receipt_fn = policy_module.getattr("baseline_receipts").unwrap();
        let receipts = receipt_fn
            .call1((&bounded, path(py, case.root()), "full", &classes))
            .unwrap();
        assert_eq!(
            text(&receipts.get_item(0).unwrap().get_item("path").unwrap()),
            "baseline.json"
        );
        assert_eq!(
            receipt_fn
                .call1((&bounded, path(py, case.root()), "fast", &classes))
                .unwrap()
                .len()
                .unwrap(),
            0
        );
        std::fs::remove_file(baseline_file).unwrap();
        let error = receipt_fn
            .call1((&bounded, path(py, case.root()), "full", &classes))
            .unwrap_err();
        support::assert_error(
            py,
            error,
            &policy_module.getattr("PolicyError").unwrap(),
            "required baseline is absent",
        );
    });
}

#[test]
fn exception_match_uses_fingerprint_and_rejects_ambiguous_exemptions() {
    let case = Case::new();
    Python::attach(|py| {
        let policy_module = module(py, "conductor.candidate_review.policy");
        let model = module(py, "conductor.candidate_review.model");
        let severity = model.getattr("Severity").unwrap().getattr("HIGH").unwrap();
        let finding = model
            .getattr("Finding")
            .unwrap()
            .call1(("python-ast", "owned-debt", severity, "bounded finding"))
            .unwrap()
            .call_method0("finalize")
            .unwrap();
        let fingerprint = attr_text(&finding, "fingerprint");
        let exception = policy_module
            .getattr("ExceptionPolicy")
            .unwrap()
            .call1((
                "owned",
                "python-ast",
                "owned-debt",
                "conductor/probe.py",
                fingerprint,
                "governance",
                "Specific temporary test-only exception.",
                shifted_date(py, 1),
            ))
            .unwrap();
        let policy = loaded_policy(py, &case, &[]);
        let exceptions = PyTuple::new(py, [&exception]).unwrap();
        let bounded = replace_field(py, &policy, "exceptions", exceptions.as_any());
        let apply = policy_module.getattr("apply_exceptions").unwrap();
        apply.call1((&bounded, vec![&finding])).unwrap();
        assert_eq!(attr_text(&finding, "exception_id"), "owned");
        finding.setattr("exception_id", py.None()).unwrap();
        let duplicate = replace_field(
            py,
            &exception,
            "exception_id",
            pyo3::types::PyString::new(py, "also-owned").as_any(),
        );
        let both = PyTuple::new(py, [&exception, &duplicate]).unwrap();
        let ambiguous = replace_field(py, &policy, "exceptions", both.as_any());
        let error = apply.call1((&ambiguous, vec![&finding])).unwrap_err();
        support::assert_error(
            py,
            error,
            &policy_module.getattr("PolicyError").unwrap(),
            "multiple exceptions",
        );
    });
}

#[test]
fn pathless_fingerprint_exception_matches_only_its_exact_finding() {
    let case = Case::new();
    Python::attach(|py| {
        let policy_module = module(py, "conductor.candidate_review.policy");
        let model = module(py, "conductor.candidate_review.model");
        let severity = model.getattr("Severity").unwrap().getattr("HIGH").unwrap();
        let make_finding = |message: &str| {
            model
                .getattr("Finding")
                .unwrap()
                .call1((
                    "research-integrity",
                    "incomplete-result-provenance",
                    &severity,
                    message,
                ))
                .unwrap()
                .call_method0("finalize")
                .unwrap()
        };
        let matching =
            make_finding("research decision path lacks exact identity/provenance fields: baseline");
        assert!(matching.getattr("path").unwrap().is_none());
        let fingerprint = attr_text(&matching, "fingerprint");
        let exception = policy_module
            .getattr("ExceptionPolicy")
            .unwrap()
            .call1((
                "pathless-probe",
                "research-integrity",
                "incomplete-result-provenance",
                "conductor/candidate_policy.toml",
                fingerprint,
                "governance",
                "Specific temporary test-only exception.",
                shifted_date(py, 1),
            ))
            .unwrap();
        let policy = loaded_policy(py, &case, &[]);
        let exceptions = PyTuple::new(py, [&exception]).unwrap();
        let bounded = replace_field(py, &policy, "exceptions", exceptions.as_any());
        let apply = policy_module.getattr("apply_exceptions").unwrap();
        apply.call1((&bounded, vec![&matching])).unwrap();
        assert_eq!(attr_text(&matching, "exception_id"), "pathless-probe");
        let unrelated = make_finding("a different finding entirely");
        apply.call1((&bounded, vec![&unrelated])).unwrap();
        assert!(unrelated.getattr("exception_id").unwrap().is_none());
    });
}

#[test]
fn policy_dataclass_validation_rejects_duplicate_unknown_and_unbounded_exceptions() {
    let case = Case::new();
    Python::attach(|py| {
        let policy_module = module(py, "conductor.candidate_review.policy");
        let policy = loaded_policy(py, &case, &[]);
        let exception = policy_module
            .getattr("ExceptionPolicy")
            .unwrap()
            .call1((
                "owned",
                "candidate-integrity",
                py.None(),
                "conductor/probe.py",
                py.None(),
                "governance",
                "Specific temporary test-only exception.",
                shifted_date(py, 1),
            ))
            .unwrap();
        let validate = policy_module.getattr("_validate_policy").unwrap();
        let valid = PyTuple::new(py, [&exception]).unwrap();
        validate
            .call1((replace_field(py, &policy, "exceptions", valid.as_any()),))
            .unwrap();
        let duplicate = PyTuple::new(py, [&exception, &exception]).unwrap();
        let invalid = replace_field(py, &policy, "exceptions", duplicate.as_any());
        support::assert_error(
            py,
            validate.call1((invalid,)).unwrap_err(),
            &policy_module.getattr("PolicyError").unwrap(),
            "identifiers must be unique",
        );
        for (field, value, expected) in [
            (
                "check_id",
                pyo3::types::PyString::new(py, "unknown").into_any(),
                "unknown check",
            ),
            ("expires", shifted_date(py, -1), "expired"),
            ("expires", shifted_date(py, 91), "more than 90 days"),
        ] {
            let changed = replace_field(py, &exception, field, &value);
            let entries = PyTuple::new(py, [changed]).unwrap();
            let invalid = replace_field(py, &policy, "exceptions", entries.as_any());
            support::assert_error(
                py,
                validate.call1((invalid,)).unwrap_err(),
                &policy_module.getattr("PolicyError").unwrap(),
                expected,
            );
        }
    });
}

#[test]
fn examined_paths_unions_shards_for_the_same_check() {
    let _case = Case::new();
    Python::attach(|py| {
        let model = module(py, "conductor.candidate_review.model");
        let status = model
            .getattr("CheckStatus")
            .unwrap()
            .getattr("PASSED")
            .unwrap();
        let check = model.getattr("CheckResult").unwrap();
        let first = check.call1(("python-ast", &status, 1)).unwrap();
        first.setattr("files", ["conductor/a.py"]).unwrap();
        let second = check.call1(("python-ast", &status, 1)).unwrap();
        second.setattr("files", ["conductor/b.py"]).unwrap();
        let result = module(py, "conductor.candidate_review.engine")
            .getattr("examined_paths")
            .unwrap()
            .call1((vec![first, second],))
            .unwrap();
        let files = result.get_item("python-ast").unwrap();
        assert_eq!(files.len().unwrap(), 2);
        assert!(files.contains("conductor/a.py").unwrap());
        assert!(files.contains("conductor/b.py").unwrap());
    });
}

#[test]
fn human_report_shows_stale_debt_and_every_waived_line_without_blocking_them() {
    let _case = Case::new();
    Python::attach(|py| {
        let stale = PyDict::new(py);
        for (key, value) in [
            ("id", "mut-testing-oversized-func"),
            ("check", "python-ast"),
            ("rule", "oversized-function"),
            ("path", "conductor/mutation_testing.py"),
            ("owner", "grok"),
            ("expires", "2026-09-15"),
        ] {
            stale.set_item(key, value).unwrap();
        }
        let stale_list = PyList::new(py, [&stale]).unwrap();
        let empty = PyList::empty(py);
        let reporter = module(py, "conductor.candidate_review.reporters");
        let summary = text(
            &reporter
                .getattr("human_summary")
                .unwrap()
                .call1((receipt(py, stale_list.as_any(), empty.as_any()),))
                .unwrap(),
        );
        assert!(summary
            .contains("STALE exception mut-testing-oversized-func (grok, expires 2026-09-15)"));
        assert!(summary
            .contains("python-ast read conductor/mutation_testing.py and it excused nothing"));
        assert!(summary.contains("blocking=0 advisory=0 waived=0"));
        let findings = PyList::empty(py);
        for (rule, severity, message) in [
            ("policy-drift", "critical", "policy fingerprint drifted"),
            (
                "new-test-value-waived",
                "info",
                "WAIVED first value finding",
            ),
            (
                "new-test-value-waived",
                "info",
                "WAIVED a second value finding",
            ),
        ] {
            let row = PyDict::new(py);
            for (key, value) in [
                ("check_id", "mutation-evidence"),
                ("rule_id", rule),
                ("severity", severity),
                ("message", message),
                ("path", "conductor/test_repo_index.py"),
            ] {
                row.set_item(key, value).unwrap();
            }
            findings.append(row).unwrap();
        }
        let summary = text(
            &reporter
                .getattr("human_summary")
                .unwrap()
                .call1((receipt(py, empty.as_any(), findings.as_any()),))
                .unwrap(),
        );
        assert!(summary.contains("blocking=1 advisory=0 waived=2"));
        assert!(summary.contains("INFO mutation-evidence/new-test-value-waived"));
        assert!(summary.contains("WAIVED first value finding"));
        assert!(summary.contains("WAIVED a second value finding"));
    });
}

#[test]
fn index_range_and_commit_candidates_bind_waivers_to_the_integration_base() {
    let mut case = Case::new();
    case.set_env("CONDUCTOR_INTEGRATION_BRANCH", "master");
    let (repo, integration, head) = branched_repo(&case);
    Python::attach(|py| {
        let source = module(py, "conductor.candidate_review.git_source");
        let resolve = source.getattr("resolve_candidate").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("kind", "index").unwrap();
        let index = resolve.call((path(py, &repo),), Some(&kwargs)).unwrap();
        assert_eq!(attr_text(&index, "base_commit_oid"), head);
        assert_eq!(attr_text(&index, "integration_base_oid"), integration);
        assert_eq!(attr_text(&index, "waiver_base"), integration);
        assert!(attr_text(&index, "integration_base_detail").contains("master"));
        let changes = index.getattr("changes").unwrap();
        assert_eq!(changes.len().unwrap(), 1);
        assert_eq!(attr_text(&changes.get_item(0).unwrap(), "path"), "b.txt");
        let ranged = PyDict::new(py);
        for (key, value) in [
            ("kind", "range"),
            ("target_ref", "lane"),
            ("base_ref", "master"),
        ] {
            ranged.set_item(key, value).unwrap();
        }
        let range = resolve.call((path(py, &repo),), Some(&ranged)).unwrap();
        assert_eq!(attr_text(&range, "waiver_base"), integration);
        assert_eq!(
            attr_text(&range, "integration_base_detail"),
            "range candidate base"
        );
        let committed = PyDict::new(py);
        committed.set_item("kind", "commit").unwrap();
        committed.set_item("target_ref", "lane").unwrap();
        let commit = resolve.call((path(py, &repo),), Some(&committed)).unwrap();
        assert_eq!(attr_text(&commit, "waiver_base"), integration);
        assert_eq!(
            attr_text(&commit, "integration_base_detail"),
            "commit candidate base"
        );
    });
}

#[test]
fn unrelated_history_cannot_be_a_value_waiver_base() {
    let case = Case::new();
    let (repo, _integration, _head) = branched_repo(&case);
    git(&repo, &["checkout", "-q", "--orphan", "unrelated"]);
    std::fs::write(repo.join("c.txt"), "elsewhere\n").unwrap();
    git(&repo, &["add", "c.txt"]);
    git(&repo, &["commit", "-qm", "unrelated root"]);
    let unrelated = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["checkout", "-q", "lane"]);
    Python::attach(|py| {
        let resolve = module(py, "conductor.candidate_review.git_source")
            .getattr("resolve_candidate")
            .unwrap();
        for kind in ["index", "commit"] {
            let kwargs = PyDict::new(py);
            kwargs.set_item("kind", kind).unwrap();
            kwargs.set_item("base_ref", &unrelated).unwrap();
            if kind == "commit" {
                kwargs.set_item("target_ref", "lane").unwrap();
            }
            let candidate = resolve.call((path(py, &repo),), Some(&kwargs)).unwrap();
            assert_eq!(attr_text(&candidate, "base_commit_oid"), unrelated);
            assert!(candidate.getattr("integration_base_oid").unwrap().is_none());
            assert!(candidate.getattr("waiver_base").unwrap().is_none());
            assert!(
                attr_text(&candidate, "integration_base_detail").contains("is not an ancestor of")
            );
        }
    });
}

#[test]
fn ownership_rejects_broad_paths_overlong_claims_and_tampered_store() {
    let case = Case::new();
    let (repo, _, _) = branched_repo(&case);
    Python::attach(|py| {
        let ownership = module(py, "conductor.candidate_review.ownership");
        let error_class = ownership.getattr("OwnershipError").unwrap();
        let normalize = ownership.getattr("normalize_claim_path").unwrap();
        for invalid in [
            "research",
            "../escape",
            "/absolute",
            "conductor/**",
            "conductor/gate.py,conductor/kb_retrieve.py",
            "conductor/gate.py conductor/kb_retrieve.py",
        ] {
            support::assert_error(
                py,
                normalize.call1((invalid,)).unwrap_err(),
                &error_class,
                "narrow and repository-relative",
            );
        }
        let create = ownership.getattr("create_claim").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("owner", "Codex").unwrap();
        kwargs.set_item("paths", ["conductor/probe.py"]).unwrap();
        kwargs
            .set_item("justification", "tamper-evident ownership test")
            .unwrap();
        kwargs.set_item("max_minutes", 25 * 60).unwrap();
        support::assert_error(
            py,
            create.call((path(py, &repo),), Some(&kwargs)).unwrap_err(),
            &error_class,
            "max time must be",
        );
        kwargs.set_item("max_minutes", 60).unwrap();
        let claim = create.call((path(py, &repo),), Some(&kwargs)).unwrap();
        assert!(!attr_text(&claim, "claim_id").is_empty());
        let store = ownership
            .getattr("claim_store_path")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        let store_path = std::path::PathBuf::from(text(&store));
        let mut payload: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&store_path).unwrap()).unwrap();
        payload["claims"][0]["owner"] = serde_json::json!("Mallory");
        std::fs::write(&store_path, serde_json::to_vec(&payload).unwrap()).unwrap();
        let error = ownership
            .getattr("load_claims")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap_err();
        support::assert_error(py, error, &error_class, "not bound to its content");
    });
}

fn mutation_context<'py>(
    py: Python<'py>,
    case: &Case,
    repo: &Path,
    candidate: &Bound<'py, PyAny>,
    policy: &Bound<'py, PyAny>,
    waiver_value: Option<Bound<'py, PyAny>>,
) -> Bound<'py, PyAny> {
    let policy = match waiver_value {
        Some(value) => {
            let items = PyTuple::new(py, [value]).unwrap();
            replace_field(py, policy, "value_waivers", items.as_any())
        }
        None => policy.clone(),
    };
    let fields = PyDict::new(py);
    fields.set_item("repo", path(py, repo)).unwrap();
    fields.set_item("snapshot", path(py, repo)).unwrap();
    fields.set_item("candidate", candidate).unwrap();
    fields.set_item("entries", ()).unwrap();
    fields.set_item("policy", policy).unwrap();
    fields.set_item("surface", "manual").unwrap();
    fields.set_item("profile", "fast").unwrap();
    fields.set_item("owner", py.None()).unwrap();
    fields
        .set_item("runtime_dir", path(py, &case.root().join("runtime")))
        .unwrap();
    module(py, "conductor.candidate_review.checks")
        .getattr("ReviewContext")
        .unwrap()
        .call((), Some(&fields))
        .unwrap()
}

#[test]
fn mutation_result_uses_integration_base_expiry_and_waived_pass_promotion() {
    let mut case = Case::new();
    case.set_env("CONDUCTOR_INTEGRATION_BRANCH", "master");
    let (repo, integration, head) = branched_repo(&case);
    Python::attach(|py| {
        let resolve = module(py, "conductor.candidate_review.git_source")
            .getattr("resolve_candidate")
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("kind", "index").unwrap();
        let candidate = resolve.call((path(py, &repo),), Some(&kwargs)).unwrap();
        assert_ne!(head, integration);
        let policy = loaded_policy(py, &case, &[]);
        let model = module(py, "conductor.candidate_review.model");
        let result_fn = module(py, "conductor.candidate_review.verification")
            .getattr("_mutation_evidence_result")
            .unwrap();
        for (expiry, rule, status, active) in [
            (30, "new-test-value-waived", "passed", true),
            (-1, "new-test-value-not-admitted", "failed", false),
        ] {
            let bound_base = pyo3::types::PyString::new(py, &integration);
            let active_waiver = replace_field(
                py,
                &waiver(py, &[NODEID], Some(expiry)),
                "integration_base",
                bound_base.as_any(),
            );
            let ctx = mutation_context(py, &case, &repo, &candidate, &policy, Some(active_waiver));
            let result = result_fn
                .call1((
                    ctx,
                    0.0,
                    vec![finding(py, "mutation-evidence", NODEID)],
                    vec!["conductor/test_repo_index.py"],
                    PyDict::new(py),
                ))
                .unwrap();
            assert_eq!(
                attr_text(
                    &result.getattr("findings").unwrap().get_item(0).unwrap(),
                    "rule_id"
                ),
                rule
            );
            assert_eq!(attr_text(&result, "status"), status);
            let metrics = result.getattr("metrics").unwrap();
            assert_eq!(
                text(
                    &metrics
                        .get_item("value_waiver_base")
                        .unwrap()
                        .get_item("commit")
                        .unwrap()
                ),
                integration
            );
            assert_eq!(
                metrics
                    .get_item("value_waiver_states")
                    .unwrap()
                    .get_item(0)
                    .unwrap()
                    .get_item("active")
                    .unwrap()
                    .extract::<bool>()
                    .unwrap(),
                active
            );
        }
        let info = model.getattr("Severity").unwrap().getattr("INFO").unwrap();
        let already_waived = model
            .getattr("Finding")
            .unwrap()
            .call1((
                "mutation-evidence",
                "new-test-value-waived",
                info,
                "WAIVED a value finding",
            ))
            .unwrap();
        let result = result_fn
            .call1((
                mutation_context(py, &case, &repo, &candidate, &policy, None),
                0.0,
                vec![already_waived],
                vec!["conductor/test_repo_index.py"],
                PyDict::new(py),
            ))
            .unwrap();
        assert_eq!(attr_text(&result, "status"), "passed");
    });
}

fn mutation_gate_fixture<'py>(
    py: Python<'py>,
    case: &Case,
) -> (
    Bound<'py, PyAny>,
    Vec<support::AttrPatch>,
    support::AttrPatch,
) {
    let (context, anchor) = candidate_review_support::gate_context(
        py,
        case.root(),
        case.root(),
        &[(PROBE, &["test_probe_legacy"])],
        &"c".repeat(40),
    );
    let snapshot = std::path::PathBuf::from(text(&context.getattr("snapshot").unwrap()));
    let schema = module(py, "conductor.mutation_value")
        .getattr("VALUE_SCHEMA")
        .unwrap()
        .extract::<String>()
        .unwrap();
    std::fs::write(
        snapshot.join("receipt.json"),
        serde_json::json!({
            "test_value": {"schema_version": schema, "status": "PASS",
                           "tests": [{"nodeid": NEW_NODEID, "classification": "DELETE_CANDIDATE"}]}
        })
        .to_string(),
    )
    .unwrap();
    let payload = serde_json::json!({
        "status": "PASS", "checked_test_paths": [PROBE],
        "evidence": [{"path": PROBE, "receipt": "receipt.json"}],
        "missing_evidence": [], "malformed_receipts": []
    });
    let py_payload = module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((payload.to_string(),))
        .unwrap();
    let mock_class = module(py, "unittest.mock").getattr("Mock").unwrap();
    let mock_kwargs = PyDict::new(py);
    mock_kwargs.set_item("return_value", py_payload).unwrap();
    let mocked = mock_class.call((), Some(&mock_kwargs)).unwrap();
    let verify = support::AttrPatch::replace(
        &module(py, "conductor.mutation_testing"),
        "verify_evidence",
        &mocked,
    );
    (context, anchor, verify)
}

#[test]
fn mutation_gate_names_new_nodeids_and_applies_only_active_integration_waivers() {
    let case = Case::new();
    Python::attach(|py| {
        let (context, _anchor, _verify) = mutation_gate_fixture(py, &case);
        let integration = pyo3::types::PyString::new(py, &"f".repeat(40));
        let candidate = replace_field(
            py,
            &context.getattr("candidate").unwrap(),
            "integration_base_oid",
            integration.as_any(),
        );
        let detail = pyo3::types::PyString::new(py, "merge base with origin/master");
        let candidate = replace_field(py, &candidate, "integration_base_detail", detail.as_any());
        let context = replace_field(py, &context, "candidate", &candidate);
        let check = module(py, "conductor.candidate_review.checks")
            .getattr("check_mutation_evidence")
            .unwrap();
        let no_waiver = check.call1((&context,)).unwrap();
        let missing = no_waiver.getattr("findings").unwrap().get_item(0).unwrap();
        assert_eq!(
            attr_text(&missing, "rule_id"),
            "new-test-value-not-admitted"
        );
        assert_eq!(
            text(
                &missing
                    .getattr("evidence")
                    .unwrap()
                    .get_item("nodeid")
                    .unwrap()
            ),
            NEW_NODEID
        );
        assert_eq!(attr_text(&no_waiver, "status"), "failed");
        for (expiry, expected_rule, expected_status, active) in [
            (30, "new-test-value-waived", "passed", true),
            (-1, "new-test-value-not-admitted", "failed", false),
        ] {
            let base = replace_field(
                py,
                &waiver(py, &[NEW_NODEID], Some(expiry)),
                "integration_base",
                integration.as_any(),
            );
            let waivers = PyTuple::new(py, [base]).unwrap();
            let policy = replace_field(
                py,
                &context.getattr("policy").unwrap(),
                "value_waivers",
                waivers.as_any(),
            );
            let changed = replace_field(py, &context, "policy", &policy);
            let result = check.call1((changed,)).unwrap();
            assert_eq!(
                attr_text(
                    &result.getattr("findings").unwrap().get_item(0).unwrap(),
                    "rule_id"
                ),
                expected_rule
            );
            assert_eq!(attr_text(&result, "status"), expected_status);
            let metrics = result.getattr("metrics").unwrap();
            assert_eq!(
                text(
                    &metrics
                        .get_item("value_waiver_base")
                        .unwrap()
                        .get_item("commit")
                        .unwrap()
                ),
                "f".repeat(40)
            );
            assert_eq!(
                metrics
                    .get_item("value_waiver_states")
                    .unwrap()
                    .get_item(0)
                    .unwrap()
                    .get_item("active")
                    .unwrap()
                    .extract::<bool>()
                    .unwrap(),
                active
            );
        }
    });
}
