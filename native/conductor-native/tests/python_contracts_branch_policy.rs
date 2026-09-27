#![cfg(feature = "python-compat-tests")]
//! Rust-owned compatibility checks for the Python branch-policy API.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)] // This helper is shared by three separate integration-test binaries.
mod support;

use pyo3::call::PyCallArgs;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{assert_error, attr_text, module, path, text, AttrPatch, Case};

const TOPIC: &str = "claude/topic-20260829";

fn branch_case() -> Case {
    let mut case = Case::new();
    case.set_env("CONDUCTOR_INTEGRATION_BRANCH", "master");
    case
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run isolated fixture git command");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 git output")
}

fn repository(case: &Case) -> PathBuf {
    let repo = case.mkdir("repo");
    git(&repo, &["init", "--quiet", "--initial-branch=master"]);
    git(&repo, &["config", "user.name", "Branch Policy Test"]);
    git(
        &repo,
        &["config", "user.email", "branch-policy@example.invalid"],
    );
    repo
}

fn commit(repo: &Path, filename: &str) -> String {
    fs::write(repo.join(filename), format!("{filename}\n")).expect("write commit fixture");
    git(repo, &["add", "--", filename]);
    git(
        repo,
        &["commit", "--quiet", "-m", &format!("add {filename}")],
    );
    git(repo, &["rev-parse", "HEAD"]).trim().to_owned()
}

fn call<'py>(
    bp: &Bound<'py, PyModule>,
    name: &str,
    args: impl PyCallArgs<'py>,
) -> PyResult<Bound<'py, PyAny>> {
    bp.getattr(name)?.call1(args)
}

fn call_kw<'py>(
    bp: &Bound<'py, PyModule>,
    name: &str,
    args: impl PyCallArgs<'py>,
    kwargs: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyAny>> {
    bp.getattr(name)?.call(args, Some(kwargs))
}

fn branch_kwargs<'py>(
    py: Python<'py>,
    branch: &str,
    claim: &str,
    owner: &str,
) -> Bound<'py, PyDict> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("branch", branch).unwrap();
    kwargs.set_item("claim_id", claim).unwrap();
    kwargs.set_item("owner", owner).unwrap();
    kwargs
}

fn binding_store(py: Python<'_>, bp: &Bound<'_, PyModule>, repo: &Path) -> PathBuf {
    let result = call(bp, "binding_store_path", (path(py, repo),)).unwrap();
    PathBuf::from(text(&result))
}

fn bindings<'py>(py: Python<'py>, bp: &Bound<'py, PyModule>, repo: &Path) -> Bound<'py, PyAny> {
    call(bp, "load_bindings", (path(py, repo),)).unwrap()
}

fn binding_row(branch: &str, claim: &str) -> serde_json::Value {
    serde_json::json!({
        "branch": branch,
        "claim_id": claim,
        "owner": "claude",
        "created_at": "2026-01-01T00:00:00+00:00",
        "last_push_at": null,
        "pr_number": null
    })
}

fn write_bindings(
    py: Python<'_>,
    bp: &Bound<'_, PyModule>,
    repo: &Path,
    rows: &[serde_json::Value],
) {
    let store = binding_store(py, bp, repo);
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    fs::write(
        store,
        serde_json::json!({"schema_version": 1, "bindings": rows}).to_string(),
    )
    .unwrap();
}

fn audit(py: Python<'_>, bp: &Bound<'_, PyModule>, repo: &Path) -> Vec<String> {
    call(bp, "audit_repo", (path(py, repo),))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn validates_shape_calendar_and_specific_refusals() {
    let _case = branch_case();
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let valid = call(
            &bp,
            "validate_branch_name",
            ("claude/branch-policy-20260829",),
        )
        .unwrap();
        assert_eq!(attr_text(&valid, "raw"), "claude/branch-policy-20260829");
        assert_eq!(attr_text(&valid, "agent"), "claude");
        assert_eq!(attr_text(&valid, "topic"), "branch-policy");
        assert_eq!(attr_text(&valid, "date"), "20260829");
        let digits = call(
            &bp,
            "validate_branch_name",
            ("glm-flash-04/topic-name-20260101",),
        )
        .unwrap();
        assert_eq!(attr_text(&digits, "agent"), "glm-flash-04");
        assert_eq!(attr_text(&digits, "topic"), "topic-name");
        assert_eq!(attr_text(&digits, "date"), "20260101");
        assert_eq!(
            attr_text(
                &call(&bp, "validate_branch_name", ("claude/topic-20260228",)).unwrap(),
                "date"
            ),
            "20260228"
        );

        for (name, reason) in [
            ("no-slash-here-20260829", "no '<agent>/' segment"),
            ("Claude/topic-20260829", "invalid agent slug"),
            ("9agent/topic-20260829", "invalid agent slug"),
            ("claude/nodatehere", "no '<topic>-<yyyymmdd>' segment"),
            ("claude/-topic-20260829", "no '<topic>-<yyyymmdd>' segment"),
            ("claude/topic-2026082", "no '<topic>-<yyyymmdd>' segment"),
            ("claude/topic-20260229", "invalid calendar date"),
        ] {
            let error = call(&bp, "validate_branch_name", (name,)).unwrap_err();
            assert_error(py, error, &bp.getattr("BranchPolicyError").unwrap(), reason);
        }
        for bad in [
            "nodate",
            "claude/nodate",
            "Claude/topic-20260829",
            "claude/topic-20260229",
        ] {
            let error = call(&bp, "validate_branch_name", (bad,)).unwrap_err();
            assert!(error.to_string().contains("try '"), "{bad}: {error}");
        }
    });
}

#[test]
fn integration_lines_resolve_fresh_from_host_configuration() {
    let mut case = branch_case();
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let pp = module(py, "conductor.project_paths");
        let integration = bp.getattr("is_integration_branch").unwrap();
        assert!(integration
            .call1(("master",))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(!integration
            .call1((TOPIC,))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(!integration
            .call1(("mastered",))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(!integration
            .call1(("w7-trident-program-old",))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let host = pp.getattr("host_root").unwrap().call0().unwrap();
        let line = pp
            .getattr("integration_branch")
            .unwrap()
            .call1((&host,))
            .unwrap();
        assert!(integration
            .call1((line,))
            .unwrap()
            .extract::<bool>()
            .unwrap());

        case.remove_env("CONDUCTOR_INTEGRATION_BRANCH");
        case.write(
            "pyproject.toml",
            "[tool.conductor]\nintegration_branch = \"master\"\n",
        );
        let _cwd = case.chdir(".");
        assert!(integration
            .call1(("master",))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(!integration
            .call1(("main",))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        case.write("pyproject.toml", "[tool.conductor]\nintegration_branch = \"master\"\nretired_integration_branches = [\"w7-trident-program\"]\n");
        assert!(integration
            .call1(("w7-trident-program",))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        let retired: Vec<String> = pp
            .getattr("retired_integration_branches")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(retired, ["w7-trident-program"]);
        fs::remove_file(case.root().join("pyproject.toml")).unwrap();
        let retired: Vec<String> = pp
            .getattr("retired_integration_branches")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap()
            .extract()
            .unwrap();
        assert!(retired.is_empty());
    });
}

#[test]
fn fast_forward_shortcuts_and_history_direction() {
    let case = branch_case();
    let repo = repository(&case);
    let base = commit(&repo, "a.txt");
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let ff = bp.getattr("is_fast_forward").unwrap();
        let repo_path = path(py, &repo);
        let subprocess = bp.getattr("subprocess").unwrap();
        let no_shell = PyCFunction::new_closure(
            py,
            None,
            None,
            |_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>| -> PyResult<()> {
                Err(pyo3::exceptions::PyAssertionError::new_err(
                    "equal SHAs must not shell out to Git",
                ))
            },
        )
        .unwrap();
        let patch = AttrPatch::replace(&subprocess, "run", no_shell.as_any());
        let kwargs = PyDict::new(py);
        kwargs.set_item("old", &base).unwrap();
        kwargs.set_item("new", &base).unwrap();
        assert!(ff
            .call((&repo_path,), Some(&kwargs))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        drop(patch);
        for old in ["", "0000000000000000000000000000000000000000"] {
            let kwargs = PyDict::new(py);
            kwargs.set_item("old", old).unwrap();
            kwargs.set_item("new", &base).unwrap();
            assert!(ff
                .call((&repo_path,), Some(&kwargs))
                .unwrap()
                .extract::<bool>()
                .unwrap());
        }
    });
    let newer = commit(&repo, "b.txt");
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let ff = bp.getattr("is_fast_forward").unwrap();
        let repo_path = path(py, &repo);
        for (old, new, expected) in [(&base, &newer, true), (&newer, &base, false)] {
            let kwargs = PyDict::new(py);
            kwargs.set_item("old", old).unwrap();
            kwargs.set_item("new", new).unwrap();
            assert_eq!(
                ff.call((&repo_path,), Some(&kwargs))
                    .unwrap()
                    .extract::<bool>()
                    .unwrap(),
                expected
            );
        }
    });
    git(&repo, &["checkout", "--quiet", "-b", "side", &base]);
    let side = commit(&repo, "c.txt");
    git(&repo, &["checkout", "--quiet", "master"]);
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let kwargs = PyDict::new(py);
        kwargs.set_item("old", side).unwrap();
        kwargs.set_item("new", newer).unwrap();
        assert!(
            !call_kw(&bp, "is_fast_forward", (path(py, &repo),), &kwargs)
                .unwrap()
                .extract::<bool>()
                .unwrap()
        );
    });
}

#[test]
fn local_only_commits_exclude_remote_and_snapshot_reachability() {
    let case = branch_case();
    let repo = repository(&case);
    let first = commit(&repo, "a.txt");
    let second = commit(&repo, "b.txt");
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let rows = call(&bp, "local_only_commits", (path(py, &repo), "master")).unwrap();
        let shas: Vec<String> = rows
            .try_iter()
            .unwrap()
            .map(|row| text(&row.unwrap().get_item("sha").unwrap()))
            .collect();
        assert_eq!(shas, vec![second.clone(), first.clone()]);
    });
    git(&repo, &["update-ref", "refs/remotes/origin/master", &first]);
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let rows = call(&bp, "local_only_commits", (path(py, &repo), "master")).unwrap();
        assert_eq!(rows.len().unwrap(), 1);
        assert_eq!(
            text(&rows.get_item(0).unwrap().get_item("sha").unwrap()),
            second
        );
    });
    git(&repo, &["update-ref", "refs/snapshots/backup-1", &second]);
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        assert_eq!(
            call(&bp, "local_only_commits", (path(py, &repo), "master"))
                .unwrap()
                .len()
                .unwrap(),
            0
        );
    });
}

#[test]
fn binding_store_location_schema_and_roundtrip() {
    let case = branch_case();
    let repo = repository(&case);
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let store = binding_store(py, &bp, &repo);
        assert_eq!(store, repo.join(".git/governance/branch-bindings.json"));
        assert_eq!(bindings(py, &bp, &repo).len().unwrap(), 0);
        fs::create_dir_all(store.parent().unwrap()).unwrap();
        for (body, reason) in [
            (r#"{"bindings":[]}"#, "invalid top-level schema"),
            (r#"{"schema_version":2,"bindings":[]}"#, "schema version"),
        ] {
            fs::write(&store, body).unwrap();
            let error = call(&bp, "load_bindings", (path(py, &repo),)).unwrap_err();
            assert_error(py, error, &bp.getattr("BranchPolicyError").unwrap(), reason);
        }
        let duplicate = binding_row(TOPIC, "c1");
        write_bindings(py, &bp, &repo, &[duplicate.clone(), duplicate]);
        let error = call(&bp, "load_bindings", (path(py, &repo),)).unwrap_err();
        assert_error(
            py,
            error,
            &bp.getattr("BranchPolicyError").unwrap(),
            "duplicate branches",
        );
        fs::remove_file(store).unwrap();
    });
    commit(&repo, "a.txt");
    git(&repo, &["checkout", "--quiet", "-b", TOPIC]);
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let kwargs = branch_kwargs(py, TOPIC, "c1", "claude");
        let bound = call_kw(&bp, "bind_branch", (path(py, &repo),), &kwargs).unwrap();
        let loaded = bindings(py, &bp, &repo);
        assert_eq!(loaded.len().unwrap(), 1);
        assert!(bound.eq(loaded.get_item(0).unwrap()).unwrap());
        let rebound = call_kw(&bp, "bind_branch", (path(py, &repo),), &kwargs).unwrap();
        assert_eq!(attr_text(&rebound, "claim_id"), "c1");
        assert_eq!(bindings(py, &bp, &repo).len().unwrap(), 1);
        let unbind = PyDict::new(py);
        unbind.set_item("branch", TOPIC).unwrap();
        assert!(call_kw(&bp, "unbind_branch", (path(py, &repo),), &unbind)
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert_eq!(bindings(py, &bp, &repo).len().unwrap(), 0);
        assert!(!call_kw(&bp, "unbind_branch", (path(py, &repo),), &unbind)
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn owner_and_live_claim_conflicts_use_current_refs() {
    let case = branch_case();
    let repo = repository(&case);
    let base = commit(&repo, "a.txt");
    git(
        &repo,
        &[
            "checkout",
            "--quiet",
            "-b",
            "claude/topic-a-20260829",
            &base,
        ],
    );
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let wrong = branch_kwargs(py, "claude/topic-a-20260829", "c1", "fable-5");
        let error = call_kw(&bp, "bind_branch", (path(py, &repo),), &wrong).unwrap_err();
        assert_error(
            py,
            error,
            &bp.getattr("BranchPolicyError").unwrap(),
            "does not match owner",
        );
        let proper = branch_kwargs(py, "claude/topic-a-20260829", "c1", "claude");
        call_kw(&bp, "bind_branch", (path(py, &repo),), &proper).unwrap();
    });
    git(&repo, &["checkout", "--quiet", "master"]);
    git(
        &repo,
        &[
            "checkout",
            "--quiet",
            "-b",
            "claude/topic-b-20260829",
            &base,
        ],
    );
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let second = branch_kwargs(py, "claude/topic-b-20260829", "c1", "claude");
        let error = call_kw(&bp, "bind_branch", (path(py, &repo),), &second).unwrap_err();
        assert_error(
            py,
            error,
            &bp.getattr("BranchPolicyError").unwrap(),
            "already bound to live branch",
        );
    });
    git(&repo, &["branch", "-D", "claude/topic-a-20260829"]);
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let second = branch_kwargs(py, "claude/topic-b-20260829", "c1", "claude");
        let bound = call_kw(&bp, "bind_branch", (path(py, &repo),), &second).unwrap();
        assert_eq!(attr_text(&bound, "branch"), "claude/topic-b-20260829");
    });
}

#[test]
fn stale_binding_threshold_is_strictly_greater_than_six_hours() {
    let _case = branch_case();
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let datetime = PyModule::import(py, "datetime")
            .unwrap()
            .getattr("datetime")
            .unwrap();
        let now = datetime
            .call_method1("fromisoformat", ("2026-08-29T12:00:00+00:00",))
            .unwrap();
        for (created, pushed, stale) in [
            ("2026-08-29T06:00:36+00:00", None, false),
            ("2026-08-29T06:00:00+00:00", None, false),
            ("2026-08-29T05:59:24+00:00", None, true),
            (
                "2026-08-27T12:00:00+00:00",
                Some("2026-08-29T11:55:00+00:00"),
                false,
            ),
        ] {
            let binding = bp
                .getattr("BranchBinding")
                .unwrap()
                .call1((TOPIC, "c1", "claude", created, pushed, py.None()))
                .unwrap();
            let kwargs = PyDict::new(py);
            kwargs.set_item("now", &now).unwrap();
            let result: bool = call_kw(&bp, "binding_is_stale", (binding,), &kwargs)
                .unwrap()
                .extract()
                .unwrap();
            assert_eq!(result, stale, "created={created}, pushed={pushed:?}");
        }
    });
}

#[test]
fn clean_ahead_and_local_only_integration_lines_are_silent() {
    let case = branch_case();
    let repo = repository(&case);
    let base = commit(&repo, "a.txt");
    Python::attach(|py| {
        assert!(audit(py, &module(py, "conductor.branch_policy"), &repo).is_empty())
    });
    git(&repo, &["update-ref", "refs/remotes/origin/master", &base]);
    commit(&repo, "b.txt");
    Python::attach(|py| {
        assert!(audit(py, &module(py, "conductor.branch_policy"), &repo).is_empty())
    });
    git(&repo, &["update-ref", "-d", "refs/remotes/origin/master"]);
    Python::attach(|py| {
        assert!(audit(py, &module(py, "conductor.branch_policy"), &repo).is_empty())
    });
}

#[test]
fn audit_reports_diverged_integration_and_misnamed_feature() {
    let case = branch_case();
    let repo = repository(&case);
    let base = commit(&repo, "a.txt");
    git(&repo, &["update-ref", "refs/remotes/origin/master", &base]);
    git(&repo, &["commit", "--quiet", "--amend", "-m", "rewritten"]);
    Python::attach(|py| {
        let findings = audit(py, &module(py, "conductor.branch_policy"), &repo);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("rule 1") && findings[0].contains("diverged"));
    });
    git(&repo, &["update-ref", "-d", "refs/remotes/origin/master"]);
    git(&repo, &["branch", "not-a-valid-name"]);
    Python::attach(|py| {
        let findings = audit(py, &module(py, "conductor.branch_policy"), &repo);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("rule 2"));
        assert!(findings[0].contains("not-a-valid-name"));
        assert!(findings[0].contains("try 'not-a-valid-name/topic-"));
    });
    git(&repo, &["branch", "-D", "not-a-valid-name"]);
    git(&repo, &["branch", "claude/topic-20260906"]);
    Python::attach(|py| {
        assert!(audit(py, &module(py, "conductor.branch_policy"), &repo).is_empty())
    });
}

#[test]
fn audit_counts_only_live_branches_per_claim() {
    let case = branch_case();
    let repo = repository(&case);
    commit(&repo, "a.txt");
    git(&repo, &["branch", "claude/first-20260906"]);
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        write_bindings(
            py,
            &bp,
            &repo,
            &[binding_row("claude/first-20260906", "claim-x")],
        );
        assert!(audit(py, &bp, &repo).is_empty());
    });
    git(&repo, &["branch", "claude/second-20260906"]);
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        write_bindings(
            py,
            &bp,
            &repo,
            &[
                binding_row("claude/first-20260906", "claim-x"),
                binding_row("claude/second-20260906", "claim-x"),
            ],
        );
        let findings = audit(py, &bp, &repo);
        assert_eq!(findings.len(), 1);
        for part in [
            "rule 3",
            "claim-x",
            "claude/first-20260906",
            "claude/second-20260906",
        ] {
            assert!(
                findings[0].contains(part),
                "missing {part}: {}",
                findings[0]
            );
        }
    });
    git(&repo, &["branch", "-D", "claude/second-20260906"]);
    Python::attach(|py| {
        assert!(audit(py, &module(py, "conductor.branch_policy"), &repo).is_empty())
    });
}

fn cli(py: Python<'_>, bp: &Bound<'_, PyModule>, argv: &[&str]) -> (i32, String) {
    let sink = PyModule::import(py, "io")
        .unwrap()
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let redirect = PyModule::import(py, "contextlib")
        .unwrap()
        .getattr("redirect_stdout")
        .unwrap()
        .call1((&sink,))
        .unwrap();
    redirect.call_method0("__enter__").unwrap();
    let result = bp.getattr("main").unwrap().call1((argv.to_vec(),));
    redirect
        .call_method1("__exit__", (py.None(), py.None(), py.None()))
        .unwrap();
    let code = result.unwrap().extract().unwrap();
    let output = sink.call_method0("getvalue").unwrap().extract().unwrap();
    (code, output)
}

#[test]
fn cli_check_branch_reports_exit_status_and_json_fields() {
    let _case = branch_case();
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let (code, output) = cli(py, &bp, &["check-branch", TOPIC]);
        assert_eq!(code, 0);
        assert!(output.contains("OK"));
        let (code, output) = cli(py, &bp, &["check-branch", "bad name"]);
        assert_eq!(code, 1);
        assert!(output.contains("REFUSED"));
        let (code, output) = cli(py, &bp, &["check-branch", TOPIC, "--json"]);
        assert_eq!(code, 0);
        let json: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "ok": true,
                "branch": TOPIC,
                "raw": TOPIC,
                "agent": "claude",
                "topic": "topic",
                "date": "20260829"
            })
        );
    });
}

#[test]
fn cli_binding_status_and_audit_use_temporary_repository() {
    let case = branch_case();
    let repo = repository(&case);
    commit(&repo, "a.txt");
    git(&repo, &["checkout", "--quiet", "-b", TOPIC]);
    let _cwd = case.chdir("repo");
    Python::attach(|py| {
        let bp = module(py, "conductor.branch_policy");
        let (code, output) = cli(py, &bp, &["status"]);
        assert_eq!(code, 0);
        assert!(output.contains("no branch bindings"));
        let (code, output) = cli(py, &bp, &["bind", "--branch", TOPIC, "--claim", "c1"]);
        assert_eq!(code, 0);
        assert!(output.contains("BOUND"));
        let (code, output) = cli(py, &bp, &["status"]);
        assert_eq!(code, 0);
        assert!(output.contains(TOPIC) && output.contains("c1"));
        let (code, output) = cli(py, &bp, &["unbind", "--branch", TOPIC]);
        assert_eq!(code, 0);
        assert!(output.contains("UNBOUND"));
        let (code, output) = cli(py, &bp, &["unbind", "--branch", TOPIC]);
        assert_eq!(code, 1);
        assert!(output.contains("NO BINDING"));
        let (code, output) = cli(py, &bp, &["audit"]);
        assert_eq!(code, 0);
        assert!(output.starts_with("OK:"));
        git(&repo, &["branch", "Not-A-Valid-Name"]);
        let (code, output) = cli(py, &bp, &["audit"]);
        assert_eq!(code, 1);
        assert!(output.contains("VIOLATIONS: 1"));
        assert!(output.contains("Not-A-Valid-Name"));
        let (code, _) = cli(py, &bp, &["exposed"]);
        assert_eq!(code, 0);
    });
}
