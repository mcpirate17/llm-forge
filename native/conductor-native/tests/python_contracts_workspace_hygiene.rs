#![cfg(feature = "python-compat-tests")]
//! Rust-owned fixtures and assertions for `conductor.workspace_hygiene`.

#[path = "python_contracts/workspace_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use fixture::{
    commit, dead_origin_refs, git, lineless_repo, proc_root, seeded_repo, set_mtime, state,
    worktree, write,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule};
use serde_json::{Map, Value};
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use support::{assert_error, module, path, text, AttrPatch, Case};

fn reap<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.worktree_reap")
}

fn hygiene<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.workspace_hygiene")
}

fn integration_ref(py: Python<'_>, repo: &Path, offline: bool) -> PyResult<String> {
    let subject = reap(py);
    let kwargs = PyDict::new(py);
    if offline {
        kwargs.set_item("allow_network", false).unwrap();
    }
    subject
        .getattr("default_integration_ref")
        .unwrap()
        .call((path(py, repo),), Some(&kwargs))?
        .extract()
}

#[test]
fn configured_branch_is_the_integration_ref() {
    let case = Case::new();
    let repo = seeded_repo(case.root(), "cfg", "master", Some("master"));
    Python::attach(|py| assert_eq!(integration_ref(py, &repo, false).unwrap(), "origin/master"));
}

#[test]
fn local_origin_head_symref_is_the_integration_ref() {
    let case = Case::new();
    let repo = seeded_repo(case.root(), "symref", "master", None);
    git(
        &repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
        ],
    );
    Python::attach(|py| assert_eq!(integration_ref(py, &repo, false).unwrap(), "origin/master"));
}

#[test]
fn lone_conventional_ref_resolves_without_network() {
    let case = Case::new();
    let repo = seeded_repo(case.root(), "offline", "work", None);
    dead_origin_refs(&repo, &["master"]);
    Python::attach(|py| assert_eq!(integration_ref(py, &repo, true).unwrap(), "origin/master"));
}

#[test]
fn offline_ambiguity_requires_network_opt_in() {
    let case = Case::new();
    let repo = seeded_repo(case.root(), "ambiguous", "work", Some("wip"));
    dead_origin_refs(&repo, &["master", "main"]);
    Python::attach(|py| {
        let subject = reap(py);
        let error = integration_ref(py, &repo, true).unwrap_err();
        assert_error(
            py,
            error,
            subject.getattr("ReapError").unwrap().as_any(),
            "network advertisement skipped",
        );
    });
}

#[test]
fn unconventional_line_uses_remote_advertisement_when_allowed() {
    let case = Case::new();
    let repo = seeded_repo(case.root(), "advertised", "trunk", None);
    Python::attach(|py| {
        assert_eq!(integration_ref(py, &repo, false).unwrap(), "origin/trunk");
        let subject = reap(py);
        let error = integration_ref(py, &repo, true).unwrap_err();
        assert_error(
            py,
            error,
            subject.getattr("ReapError").unwrap().as_any(),
            "network advertisement skipped",
        );
    });
}

#[test]
fn origin_ref_wins_over_unpushed_local_head() {
    let case = Case::new();
    let repo = seeded_repo(case.root(), "plain", "main", None);
    commit(&repo, "unpushed.txt", "unpushed.txt\n");
    Python::attach(|py| assert_eq!(integration_ref(py, &repo, false).unwrap(), "origin/main"));
}

#[test]
fn repo_without_any_integration_line_refuses() {
    let case = Case::new();
    let repo = lineless_repo(case.root(), "lineless");
    Python::attach(|py| {
        let subject = reap(py);
        let error = integration_ref(py, &repo, false).unwrap_err();
        assert_error(
            py,
            error,
            subject.getattr("ReapError").unwrap().as_any(),
            "no integration line",
        );
    });
}

#[test]
fn decide_checks_containment_against_resolved_main() {
    let case = Case::new();
    let repo = seeded_repo(case.root(), "reap", "main", None);
    let tree = worktree(&repo, "done-tree", "topic/done", "HEAD");
    let proc = proc_root(&case.root().join("empty-proc"), "4242", None);
    Python::attach(|py| {
        let subject = reap(py);
        let rows = fixture::decide(py, &subject, &repo, &repo, &proc, None);
        let row = state(py, &rows, &tree);
        assert!(row.getattr("eligible").unwrap().extract::<bool>().unwrap());
        let first = row.getattr("reasons").unwrap().get_item(0).unwrap();
        assert!(text(&first).contains("contained in origin/main"));
    });
}

#[test]
fn reap_preview_survives_a_main_line_repo() {
    let case = Case::new();
    let repo = seeded_repo(case.root(), "preview", "main", None);
    worktree(&repo, "preview-tree", "topic/over", "HEAD");
    Python::attach(|py| {
        let rows = hygiene(py)
            .getattr("reap_preview")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        assert!(rows.len().unwrap() > 0);
        let first = rows.get_item(0).unwrap();
        assert!(text(&first.get_item("reason").unwrap()).contains("contained in origin/main"));
    });
}

#[test]
fn live_ref_resolution_has_no_master_literal() {
    let case = Case::new();
    let repo = seeded_repo(case.root(), "live", "master", None);
    let lineless = lineless_repo(case.root(), "lineless");
    Python::attach(|py| {
        let subject = hygiene(py);
        let resolve = subject.getattr("_live_ref_or_default").unwrap();
        assert_eq!(
            resolve
                .call1((path(py, &repo),))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "origin/master"
        );
        let error = resolve.call1((path(py, &lineless),)).unwrap_err();
        assert_error(
            py,
            error,
            subject.getattr("HygieneError").unwrap().as_any(),
            "no integration line",
        );
    });
}

#[test]
fn cheap_counts_show_unknown_on_offline_ambiguity() {
    let case = Case::new();
    let repo = seeded_repo(case.root(), "offline-counts", "work", Some("wip"));
    dead_origin_refs(&repo, &["master", "main"]);
    Python::attach(|py| {
        let counts = hygiene(py)
            .getattr("cheap_exposure_counts")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        assert!(counts.get_item("landed_worktrees").unwrap().is_none());
        assert!(text(&counts.get_item("worktrees_skipped").unwrap())
            .contains("network advertisement skipped"));
    });
}

#[test]
fn root_is_the_repository_root() {
    let _case = Case::new();
    let expected = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    Python::attach(|py| {
        let root = hygiene(py).getattr("ROOT").unwrap();
        assert!(root.eq(path(py, &expected)).unwrap());
        assert!(expected.join(".git").exists());
    });
}

#[test]
fn manifest_state_uses_the_configured_registry() {
    let _case = Case::new();
    Python::attach(|py| {
        let rows = hygiene(py)
            .getattr("manifest_state")
            .unwrap()
            .call0()
            .unwrap();
        assert!(rows
            .get_item(0)
            .unwrap()
            .is_instance_of::<pyo3::types::PyList>());
        assert!(rows
            .get_item(1)
            .unwrap()
            .is_instance_of::<pyo3::types::PyList>());
    });
}

#[test]
fn idle_claims_resolve_claim_paths_from_repository_root() {
    let case = Case::new();
    let repo = seeded_repo(case.root(), "claims", "main", None);
    let quiet_epoch = (SystemTime::now() - Duration::from_secs(100 * 60))
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let quiet =
        chrono::DateTime::<chrono::Utc>::from(UNIX_EPOCH + Duration::from_secs(quiet_epoch as u64))
            .to_rfc3339();
    Python::attach(|py| {
        let ownership = module(py, "conductor.candidate_review.ownership");
        let kwargs = PyDict::new(py);
        kwargs.set_item("owner", "hygiene-test").unwrap();
        kwargs.set_item("paths", vec!["tracked.txt"]).unwrap();
        kwargs
            .set_item("justification", "slice I defect test")
            .unwrap();
        kwargs.set_item("expected_minutes", 15).unwrap();
        kwargs.set_item("max_minutes", 115).unwrap();
        ownership
            .getattr("create_claim")
            .unwrap()
            .call((path(py, &repo),), Some(&kwargs))
            .unwrap();
    });
    write(&repo.join("tracked.txt"), "aged\n");
    set_mtime(&repo.join("tracked.txt"), quiet_epoch);
    let store = repo.join(".git/governance/ownership-claims.json");
    let mut payload: Value = serde_json::from_slice(&fs::read(&store).unwrap()).unwrap();
    let claim = payload["claims"][0].as_object_mut().unwrap();
    claim.insert("created_at".into(), Value::String(quiet));
    let mut fields = Map::new();
    for key in [
        "owner",
        "paths",
        "justification",
        "created_at",
        "expires_at",
        "expected_at",
    ] {
        if let Some(value) = claim.get(key) {
            fields.insert(key.into(), value.clone());
        }
    }
    Python::attach(|py| {
        let json = py.import("json").unwrap();
        let digest_input = json
            .getattr("loads")
            .unwrap()
            .call1((Value::Object(fields).to_string(),))
            .unwrap();
        let digest: String = module(py, "conductor.candidate_review.model")
            .getattr("sha256_json")
            .unwrap()
            .call1((digest_input,))
            .unwrap()
            .extract()
            .unwrap();
        claim.insert(
            "claim_id".into(),
            Value::String(format!("claim-{}", &digest[..20])),
        );
    });
    fs::write(&store, payload.to_string()).unwrap();
    Python::attach(|py| {
        let subject = hygiene(py);
        let _root = AttrPatch::replace(subject.as_any(), "ROOT", path(py, &repo).as_any());
        let rows = subject.getattr("idle_claims").unwrap().call0().unwrap();
        assert!(rows.len().unwrap() > 0);
        let first = rows.get_item(0).unwrap();
        assert_eq!(
            text(&first.get_item("claim_id").unwrap()),
            payload["claims"][0]["claim_id"].as_str().unwrap()
        );
        assert_eq!(text(&first.get_item("idle_minutes").unwrap()), "100");
    });
}

#[test]
fn claim_checks_never_spawn_a_second_interpreter() {
    let case = Case::new();
    let repo = seeded_repo(case.root(), "inproc", "main", None);
    Python::attach(|py| {
        let subject = hygiene(py);
        let _root = AttrPatch::replace(subject.as_any(), "ROOT", path(py, &repo).as_any());
        let subprocess = subject.getattr("subprocess").unwrap();
        let original = subprocess.getattr("run").unwrap().unbind();
        let probe =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                let argv = args.get_item(0)?;
                let rendered = argv.str()?.to_str()?.to_owned();
                assert!(
                    !rendered.contains("-m") && !rendered.contains("python"),
                    "second interpreter: {rendered}"
                );
                Ok(original.bind(args.py()).call(args, kwargs)?.unbind())
            })
            .unwrap();
        let _run = AttrPatch::replace(&subprocess, "run", probe.as_any());
        assert_eq!(
            subject
                .getattr("expired_claims")
                .unwrap()
                .call0()
                .unwrap()
                .len()
                .unwrap(),
            0
        );
        assert_eq!(
            subject
                .getattr("idle_claims")
                .unwrap()
                .call0()
                .unwrap()
                .len()
                .unwrap(),
            0
        );
    });
}

#[test]
fn exposure_line_reports_the_cheap_counts() {
    let case = Case::new();
    let repo = seeded_repo(case.root(), "line", "main", None);
    Python::attach(|py| {
        let actual: String = hygiene(py)
            .getattr("exposure_line")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(actual, "EXPOSED: 0 local-only commit(s), 0 stale dirty file(s), 0 finished worktree(s) to remove, branches skipped (needs gh). python -m conductor.workspace_hygiene");
    });
}

#[test]
fn exposure_line_shows_unknown_without_integration_line() {
    let case = Case::new();
    let repo = lineless_repo(case.root(), "lineless");
    Python::attach(|py| {
        let actual: String = hygiene(py)
            .getattr("exposure_line")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(actual, "EXPOSED: 1 local-only commit(s), 0 stale dirty file(s), unknown finished worktree(s) to remove, branches skipped (needs gh). python -m conductor.workspace_hygiene");
    });
}
