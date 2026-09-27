#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the shipped Python worktree reaper.

#[path = "python_contracts/workspace_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use fixture::{age, commit, git, proc_root, reap_repo, state, worktree, write};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use support::{assert_error, module, path, text, AttrPatch, Case};

fn subject<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.worktree_reap")
}

fn decisions<'py>(
    py: Python<'py>,
    reap: &Bound<'py, PyModule>,
    repo: &Path,
    idle: Option<f64>,
) -> Bound<'py, PyAny> {
    let proc = proc_root(&repo.parent().unwrap().join("empty-proc"), "4242", None);
    fixture::decide(py, reap, repo, repo, &proc, idle)
}

fn apply<'py>(
    py: Python<'py>,
    reap: &Bound<'py, PyModule>,
    repo: &Path,
    rows: &Bound<'py, PyAny>,
    checkpoint: &Path,
    proc: &Path,
) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("checkpoint_root", path(py, checkpoint))
        .unwrap();
    kwargs.set_item("delete_remote", false).unwrap();
    kwargs.set_item("current", path(py, repo)).unwrap();
    kwargs.set_item("proc_root", path(py, proc)).unwrap();
    reap.getattr("apply")
        .unwrap()
        .call((path(py, repo), rows), Some(&kwargs))
}

fn str_field(value: &Bound<'_, PyAny>, name: &str) -> String {
    value.getattr(name).unwrap().extract().unwrap()
}

fn reasons(value: &Bound<'_, PyAny>) -> Vec<String> {
    value.getattr("reasons").unwrap().extract().unwrap()
}

fn eligible(value: &Bound<'_, PyAny>) -> bool {
    value.getattr("eligible").unwrap().extract().unwrap()
}

fn lease(tree: &Path, branch: &str, opened_ago: Duration, expires_in: i64) {
    let now = SystemTime::now();
    let opened = chrono::DateTime::<chrono::Utc>::from(now - opened_ago).to_rfc3339();
    let expiry = if expires_in >= 0 {
        now + Duration::from_secs(expires_in as u64)
    } else {
        now - Duration::from_secs((-expires_in) as u64)
    };
    let expiry = chrono::DateTime::<chrono::Utc>::from(expiry).to_rfc3339();
    write(
        &tree.join(".worktree-lease.json"),
        json!({
            "schema": "worktree-lease.v1",
            "owner": "llm-b0",
            "purpose": "fixture",
            "branch": branch,
            "worktree": tree.display().to_string(),
            "opened_at": opened,
            "expires_at": expiry,
        })
        .to_string(),
    );
}

fn capture<'py>(py: Python<'py>, name: &str) -> (Bound<'py, PyAny>, AttrPatch) {
    let stream = py
        .import("io")
        .unwrap()
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let patch = AttrPatch::replace(module(py, "sys").as_any(), name, stream.as_any());
    (stream, patch)
}

fn files(root: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    if !root.exists() {
        return found;
    }
    visit_files(root, root, &mut found);
    found
}

fn visit_files(root: &Path, here: &Path, found: &mut BTreeSet<String>) {
    for entry in fs::read_dir(here).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            visit_files(root, &path, found);
        } else if path.is_file() {
            found.insert(path.strip_prefix(root).unwrap().display().to_string());
        }
    }
}

#[test]
fn parse_worktrees_preserves_safety_markers() {
    let case = Case::new();
    let output = format!("worktree {}\nHEAD {}\nlocked reason\n\nworktree /gone\nHEAD {}\nprunable gitdir missing\n\n", case.root().join("one").display(), "a".repeat(40), "b".repeat(40));
    Python::attach(|py| {
        let rows = subject(py)
            .getattr("parse_worktrees")
            .unwrap()
            .call1((output,))
            .unwrap();
        let first = rows.get_item(0).unwrap();
        let second = rows.get_item(1).unwrap();
        assert_eq!(str_field(&first, "head"), "a".repeat(40));
        assert!(first.getattr("locked").unwrap().extract::<bool>().unwrap());
        assert_eq!(str_field(&second, "head"), "b".repeat(40));
        assert!(second
            .getattr("prunable")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(second
            .getattr("missing")
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn parse_worktrees_strips_branch_prefix_and_marks_bare_missing() {
    let case = Case::new();
    fs::create_dir(case.root().join("one")).unwrap();
    let output = format!(
        "worktree {}\nHEAD {}\nbranch refs/heads/topic\n\nworktree /bare\nHEAD {}\nbare\n\n",
        case.root().join("one").display(),
        "a".repeat(40),
        "b".repeat(40)
    );
    Python::attach(|py| {
        let rows = subject(py)
            .getattr("parse_worktrees")
            .unwrap()
            .call1((output,))
            .unwrap();
        let first = rows.get_item(0).unwrap();
        let second = rows.get_item(1).unwrap();
        assert_eq!(str_field(&first, "branch"), "topic");
        assert!(!first.getattr("missing").unwrap().extract::<bool>().unwrap());
        assert_eq!(str_field(&second, "branch"), "");
        assert!(second
            .getattr("missing")
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn parse_worktrees_keeps_empty_head_and_boolean_markers() {
    let case = Case::new();
    let tree = case.root().join("one");
    fs::create_dir(&tree).unwrap();
    let output = format!(
        "worktree {}\nbranch refs/heads/topic\nlocked\nprunable\nbare\n\n",
        tree.display()
    );
    Python::attach(|py| {
        let rows = subject(py)
            .getattr("parse_worktrees")
            .unwrap()
            .call1((output,))
            .unwrap();
        let first = rows.get_item(0).unwrap();
        assert_eq!(str_field(&first, "head"), "");
        assert!(first.getattr("locked").unwrap().extract::<bool>().unwrap());
        assert!(first
            .getattr("prunable")
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn unreadable_process_is_counted_without_blocking() {
    let case = Case::new();
    let proc = proc_root(case.root(), "7", Some(case.root()));
    Python::attach(|py| {
        let reap = subject(py);
        let os = reap.getattr("os").unwrap();
        let original = os.getattr("readlink").unwrap().unbind();
        let denied =
            PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
                let target = args.get_item(0)?.str()?.to_str()?.to_owned();
                if target.ends_with("/7/cwd") {
                    return Err(pyo3::exceptions::PyPermissionError::new_err(
                        "Permission denied",
                    ));
                }
                Ok(original.bind(args.py()).call(args, kwargs)?.unbind())
            })
            .unwrap();
        let _patch = AttrPatch::replace(&os, "readlink", denied.as_any());
        let result = reap
            .getattr("active_process_cwds")
            .unwrap()
            .call1((path(py, case.root()), path(py, &proc)))
            .unwrap();
        assert_eq!(result.get_item(0).unwrap().len().unwrap(), 0);
        assert_eq!(result.get_item(1).unwrap().extract::<usize>().unwrap(), 1);
    });
}

#[test]
fn live_process_cwd_is_reported() {
    let case = Case::new();
    let tree = case.mkdir("tree");
    let proc = proc_root(case.root(), "9", Some(&tree));
    Python::attach(|py| {
        let result = subject(py)
            .getattr("active_process_cwds")
            .unwrap()
            .call1((path(py, &tree), path(py, &proc)))
            .unwrap();
        assert_eq!(
            result
                .get_item(0)
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            [format!("pid 9: {}", tree.display())]
        );
        assert_eq!(result.get_item(1).unwrap().extract::<usize>().unwrap(), 0);
    });
}

#[test]
fn unlistable_proc_root_is_fatal() {
    let case = Case::new();
    Python::attach(|py| {
        let result = subject(py)
            .getattr("active_process_cwds")
            .unwrap()
            .call1((path(py, case.root()), path(py, &case.root().join("absent"))))
            .unwrap();
        let found = result
            .get_item(0)
            .unwrap()
            .extract::<Vec<String>>()
            .unwrap();
        assert!(found[0].starts_with("unknown process state: cannot inspect"));
        assert_eq!(result.get_item(1).unwrap().extract::<usize>().unwrap(), 0);
    });
}

#[test]
fn primary_checkout_is_never_eligible() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    Python::attach(|py| {
        let reap = subject(py);
        let row = state(py, &decisions(py, &reap, &repo, None), &repo);
        assert_eq!(str_field(&row, "state"), "PRIMARY");
        assert!(!eligible(&row));
    });
}

#[test]
fn live_process_inside_tree_blocks_removal() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "busy", "topic/busy", "origin/master");
    age(&tree, 48);
    let proc = proc_root(case.root(), "31337", Some(&tree));
    Python::attach(|py| {
        let reap = subject(py);
        let row = state(
            py,
            &fixture::decide(py, &reap, &repo, &repo, &proc, None),
            &tree,
        );
        assert_eq!(str_field(&row, "state"), "ACTIVE");
        assert!(!eligible(&row));
    });
}

#[test]
fn process_cwd_deeper_inside_tree_blocks_removal() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "deep", "topic/deep", "origin/master");
    let inner = tree.join("research/reports");
    fs::create_dir_all(&inner).unwrap();
    age(&tree, 48);
    let proc = proc_root(case.root(), "31338", Some(&inner));
    Python::attach(|py| {
        let reap = subject(py);
        let row = state(
            py,
            &fixture::decide(py, &reap, &repo, &repo, &proc, None),
            &tree,
        );
        assert_eq!(str_field(&row, "state"), "ACTIVE");
    });
}

#[test]
fn locked_worktree_is_kept() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "locked", "topic/locked", "origin/master");
    git(&repo, &["worktree", "lock", &tree.display().to_string()]);
    age(&tree, 48);
    Python::attach(|py| {
        let reap = subject(py);
        let row = state(py, &decisions(py, &reap, &repo, None), &tree);
        assert_eq!(str_field(&row, "state"), "LOCKED");
        assert!(!eligible(&row));
    });
}

#[test]
fn current_directory_is_kept() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "here", "topic/here", "origin/master");
    age(&tree, 48);
    let proc = proc_root(&case.root().join("empty"), "4242", None);
    Python::attach(|py| {
        let reap = subject(py);
        let row = state(
            py,
            &fixture::decide(py, &reap, &repo, &tree, &proc, None),
            &tree,
        );
        assert_eq!(str_field(&row, "state"), "CURRENT");
    });
}

#[test]
fn live_lease_keeps_idle_worktree() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "leased", "topic/leased", "origin/master");
    lease(&tree, "topic/leased", Duration::ZERO, 4 * 3600);
    age(&tree, 48);
    Python::attach(|py| {
        let reap = subject(py);
        let row = state(py, &decisions(py, &reap, &repo, None), &tree);
        assert_eq!(str_field(&row, "state"), "LEASED");
        assert!(!eligible(&row));
    });
}

#[test]
fn expired_lease_makes_dirty_unlanded_tree_eligible() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "expired", "topic/expired", "origin/master");
    commit(&tree, "work.txt", "uncommitted\n");
    write(&tree.join("scratch.txt"), "dirty\n");
    lease(&tree, "topic/expired", Duration::from_secs(9 * 3600), -3600);
    Python::attach(|py| {
        let reap = subject(py);
        let row = state(py, &decisions(py, &reap, &repo, None), &tree);
        assert!(eligible(&row));
        assert_eq!(str_field(&row, "state"), "REMOVE");
        assert!(reasons(&row)
            .iter()
            .any(|reason| reason.contains("expired")));
    });
}

#[test]
fn idle_unlanded_worktree_is_eligible() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "idle", "topic/idle", "origin/master");
    commit(&tree, "work.txt", "unlanded\n");
    age(&tree, 48);
    Python::attach(|py| {
        let reap = subject(py);
        let row = state(py, &decisions(py, &reap, &repo, Some(6.0)), &tree);
        assert!(eligible(&row));
        assert!(reasons(&row)
            .iter()
            .any(|reason| reason.starts_with("idle:")));
    });
}

#[test]
fn busy_unlanded_tree_without_lease_is_held() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(
        &repo,
        "busy-unlanded",
        "topic/busy-unlanded",
        "origin/master",
    );
    commit(&tree, "work.txt", "fresh\n");
    Python::attach(|py| {
        let reap = subject(py);
        let row = state(py, &decisions(py, &reap, &repo, Some(6.0)), &tree);
        assert!(!eligible(&row));
        assert_eq!(str_field(&row, "state"), "HELD");
        assert_eq!(reasons(&row), vec!["run is not over".to_owned()]);
    });
}

#[test]
fn merged_head_is_eligible_even_while_busy() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "merged", "topic/merged", "origin/master");
    write(&tree.join("fresh.txt"), "modified just now\n");
    Python::attach(|py| {
        let reap = subject(py);
        let row = state(py, &decisions(py, &reap, &repo, Some(6.0)), &tree);
        assert!(eligible(&row));
        assert!(reasons(&row)
            .iter()
            .any(|reason| reason.contains("contained in origin/master")));
    });
}

#[test]
fn stale_registration_is_eligible_without_a_directory() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "gone", "topic/gone", "origin/master");
    fs::remove_dir_all(&tree).unwrap();
    Python::attach(|py| {
        let reap = subject(py);
        let row = state(py, &decisions(py, &reap, &repo, None), &tree);
        assert!(eligible(&row));
        assert_eq!(
            reasons(&row),
            vec!["stale registration: worktree directory is absent".to_owned()]
        );
    });
}

#[test]
fn remote_branch_gone_needs_tracking_ref() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "never-pushed", "topic/never-pushed", "origin/master");
    Python::attach(|py| {
        let reap = subject(py);
        let gone = reap.getattr("_remote_branch_gone").unwrap();
        assert!(!gone
            .call1((path(py, &repo), "topic/never-pushed"))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        git(&tree, &["push", "-u", "origin", "topic/never-pushed"]);
        assert!(!gone
            .call1((path(py, &repo), "topic/never-pushed"))
            .unwrap()
            .extract::<bool>()
            .unwrap());
        git(&repo, &["push", "origin", "--delete", "topic/never-pushed"]);
        assert!(gone
            .call1((path(py, &repo), "topic/never-pushed"))
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn failed_idle_probe_is_not_read_as_idle() {
    let case = Case::new();
    Python::attach(|py| {
        let result = subject(py)
            .getattr("recent_change")
            .unwrap()
            .call1((path(py, &case.root().join("absent")), 6.0))
            .unwrap();
        assert!(!result.is_none());
    });
}

#[test]
fn idle_probe_ignores_hardlinked_venv() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "venv-tree", "topic/venv-tree", "origin/master");
    age(&tree, 48);
    write(&tree.join(".venv/bin/python"), "fresh\n");
    Python::attach(|py| {
        let result = subject(py)
            .getattr("recent_change")
            .unwrap()
            .call1((path(py, &tree), 6.0))
            .unwrap();
        assert!(result.is_none());
    });
}

#[test]
fn apply_keeps_only_moved_checkpoints() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "reap-all", "topic/reap-all", "origin/master");
    commit(&tree, "landed.txt", "committed\n");
    write(&tree.join("landed.txt"), "committed then edited\n");
    write(&tree.join("untracked.txt"), "untracked\n");
    write(&tree.join("research/reports/run.json"), "{}\n");
    write(&tree.join("model.pt"), b"weights");
    write(&tree.join("runs/step/ema.pt"), b"ema");
    write(&tree.join(".venv/cached.pt"), b"venv-owned");
    age(&tree, 48);
    let ckpt = case.root().join("ckpt");
    let proc = proc_root(&case.root().join("empty-proc"), "4242", None);
    Python::attach(|py| {
        let reap = subject(py);
        let rows = fixture::decide(py, &reap, &repo, &repo, &proc, Some(6.0));
        let removed = apply(py, &reap, &repo, &rows, &ckpt, &proc).unwrap();
        assert!(!tree.exists());
        assert_eq!(
            files(&ckpt),
            BTreeSet::from([
                "topic-reap-all/model.pt".to_owned(),
                "topic-reap-all/runs/step/ema.pt".to_owned()
            ])
        );
        assert_eq!(
            fs::read(ckpt.join("topic-reap-all/model.pt")).unwrap(),
            b"weights"
        );
        let record = removed.get_item(0).unwrap();
        let keys: BTreeSet<String> = record
            .call_method0("keys")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|key| key.unwrap().extract::<String>().unwrap())
            .collect();
        assert_eq!(
            keys,
            BTreeSet::from([
                "worktree".into(),
                "branch".into(),
                "moved_checkpoints".into(),
                "branches_deleted".into()
            ])
        );
        let moved: Vec<String> = record
            .get_item("moved_checkpoints")
            .unwrap()
            .extract()
            .unwrap();
        let mut names: Vec<String> = moved
            .iter()
            .map(|value| {
                Path::new(value)
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        names.sort();
        assert_eq!(names, vec!["ema.pt".to_owned(), "model.pt".to_owned()]);
        assert!(!case.root().join("archive").exists());
    });
}

#[test]
fn move_checkpoints_never_overwrites_existing_target() {
    let case = Case::new();
    let tree = case.mkdir("tree");
    write(&tree.join("model.pt"), b"new");
    let dest = case.mkdir("dest");
    write(&dest.join("model.pt"), b"old");
    Python::attach(|py| {
        let moved = subject(py)
            .getattr("move_checkpoints")
            .unwrap()
            .call1((path(py, &tree), path(py, &dest)))
            .unwrap();
        let moved_path = PathBuf::from(moved.get_item(0).unwrap().extract::<String>().unwrap());
        assert_eq!(fs::read(dest.join("model.pt")).unwrap(), b"old");
        assert_eq!(fs::read(&moved_path).unwrap(), b"new");
        assert_eq!(
            moved_path.file_name().unwrap(),
            format!("model.pt.{}", std::process::id()).as_str()
        );
    });
}

#[test]
fn apply_force_removes_dirty_tree_and_deletes_branch() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "reap-me", "topic/reap-me", "origin/master");
    write(&tree.join("dirty.txt"), "uncommitted\n");
    age(&tree, 48);
    let proc = proc_root(&case.root().join("empty-proc"), "4242", None);
    Python::attach(|py| {
        let reap = subject(py);
        let rows = fixture::decide(py, &reap, &repo, &repo, &proc, Some(6.0));
        let removed = apply(py, &reap, &repo, &rows, &case.root().join("ckpt"), &proc).unwrap();
        assert_eq!(
            removed
                .get_item(0)
                .unwrap()
                .get_item("worktree")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            tree.display().to_string()
        );
    });
    assert!(!tree.exists());
    assert!(!git(&repo, &["branch", "--list", "topic/reap-me"]).contains("topic/reap-me"));
    assert!(!git(&repo, &["worktree", "list"]).contains("reap-me"));
}

#[test]
fn apply_refuses_tree_that_became_active_after_decision() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "raced", "topic/raced", "origin/master");
    age(&tree, 48);
    let idle_proc = proc_root(&case.root().join("empty-proc"), "4242", None);
    let active_proc = proc_root(case.root(), "999", Some(&tree));
    Python::attach(|py| {
        let reap = subject(py);
        let rows = fixture::decide(py, &reap, &repo, &repo, &idle_proc, Some(6.0));
        assert!(eligible(&state(py, &rows, &tree)));
        let error = apply(
            py,
            &reap,
            &repo,
            &rows,
            &case.root().join("ckpt"),
            &active_proc,
        )
        .unwrap_err();
        assert_error(
            py,
            error,
            reap.getattr("ReapError").unwrap().as_any(),
            "refusing",
        );
    });
    assert!(tree.exists());
}

#[test]
fn apply_never_touches_ineligible_tree() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "kept", "topic/kept", "origin/master");
    commit(&tree, "fresh.txt", "fresh\n");
    let proc = proc_root(&case.root().join("empty-proc"), "4242", None);
    Python::attach(|py| {
        let reap = subject(py);
        let rows = fixture::decide(py, &reap, &repo, &repo, &proc, Some(6.0));
        assert!(!eligible(&state(py, &rows, &tree)));
        assert_eq!(
            apply(py, &reap, &repo, &rows, &case.root().join("ckpt"), &proc)
                .unwrap()
                .len()
                .unwrap(),
            0
        );
    });
    assert!(tree.exists());
}

#[test]
fn second_apply_is_refused_while_lock_is_held() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    Python::attach(|py| {
        let reap = subject(py);
        let handle = reap
            .getattr("_hold_lock")
            .unwrap()
            .call1((path(py, &repo),))
            .unwrap();
        assert!(!handle.is_none());
        let (stderr, _capture) = capture(py, "stderr");
        let code = reap
            .getattr("main")
            .unwrap()
            .call1((vec![
                "--repo".to_owned(),
                repo.display().to_string(),
                "--apply".into(),
                "--checkpoint-root".into(),
                case.root().join("ckpt").display().to_string(),
            ],))
            .unwrap();
        handle.call_method0("close").unwrap();
        assert_eq!(code.extract::<i32>().unwrap(), 0);
        assert!(text(&stderr.call_method0("getvalue").unwrap()).contains("another reap is running"));
    });
}

#[test]
fn apply_refuses_without_checkpoint_root() {
    let mut case = Case::new();
    case.remove_env("WORKTREE_CHECKPOINT_ROOT");
    let repo = reap_repo(case.root());
    Python::attach(|py| {
        let reap = subject(py);
        let (stderr, _capture) = capture(py, "stderr");
        let code = reap
            .getattr("main")
            .unwrap()
            .call1((vec![
                "--repo".to_owned(),
                repo.display().to_string(),
                "--apply".into(),
            ],))
            .unwrap();
        assert_eq!(code.extract::<i32>().unwrap(), 2);
        assert!(text(&stderr.call_method0("getvalue").unwrap())
            .contains("needs somewhere to move stray checkpoints"));
    });
}

#[test]
fn archive_root_option_is_rejected() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    Python::attach(|py| {
        let reap = subject(py);
        let (_stderr, _capture) = capture(py, "stderr");
        let error = reap
            .getattr("main")
            .unwrap()
            .call1((vec![
                "--repo".to_owned(),
                repo.display().to_string(),
                "--archive-root".into(),
                case.root().display().to_string(),
            ],))
            .unwrap_err();
        assert!(error.is_instance_of::<pyo3::exceptions::PySystemExit>(py));
        assert_eq!(
            error
                .value(py)
                .getattr("code")
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            2
        );
    });
}

#[test]
fn apply_checkpoint_root_comes_from_environment() {
    let mut case = Case::new();
    let ckpt = case.root().join("ckpt");
    case.set_env("WORKTREE_CHECKPOINT_ROOT", ckpt.to_str().unwrap());
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "env-roots", "topic/env-roots", "origin/master");
    commit(&tree, "work.txt", "unlanded\n");
    write(&tree.join("model.pt"), b"weights");
    age(&tree, 48);
    Python::attach(|py| {
        let code = subject(py)
            .getattr("main")
            .unwrap()
            .call1((vec![
                "--repo".to_owned(),
                repo.display().to_string(),
                "--apply".into(),
            ],))
            .unwrap();
        assert_eq!(code.extract::<i32>().unwrap(), 0);
    });
    assert!(!tree.exists());
    assert_eq!(
        files(&ckpt),
        BTreeSet::from(["topic-env-roots/model.pt".to_owned()])
    );
}

#[test]
fn main_is_preview_by_default_and_reports_state() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    let tree = worktree(&repo, "preview", "topic/preview", "origin/master");
    Python::attach(|py| {
        let (stdout, _capture) = capture(py, "stdout");
        let code = subject(py)
            .getattr("main")
            .unwrap()
            .call1((vec![
                "--repo".to_owned(),
                repo.display().to_string(),
                "--json".into(),
            ],))
            .unwrap();
        assert_eq!(code.extract::<i32>().unwrap(), 0);
        let payload: Value =
            serde_json::from_str(&text(&stdout.call_method0("getvalue").unwrap())).unwrap();
        assert_eq!(payload["dry_run"], true);
        assert_eq!(payload["removed"], json!([]));
        let rows = payload["decisions"].as_array().unwrap();
        assert!(rows
            .iter()
            .any(|row| row["worktree"] == repo.display().to_string() && row["state"] == "PRIMARY"));
        assert!(rows
            .iter()
            .any(|row| row["worktree"] == tree.display().to_string()));
    });
}

#[test]
fn main_text_output_names_state_and_dry_run() {
    let case = Case::new();
    let repo = reap_repo(case.root());
    worktree(&repo, "text", "topic/text", "origin/master");
    Python::attach(|py| {
        let (stdout, _capture) = capture(py, "stdout");
        let code = subject(py)
            .getattr("main")
            .unwrap()
            .call1((vec!["--repo".to_owned(), repo.display().to_string()],))
            .unwrap();
        assert_eq!(code.extract::<i32>().unwrap(), 0);
        let rendered = text(&stdout.call_method0("getvalue").unwrap());
        assert!(rendered.contains("PRIMARY"));
        assert!(rendered.contains("pass --apply"));
    });
}

#[test]
fn slug_is_filesystem_safe() {
    let case = Case::new();
    Python::attach(|py| {
        let reap = subject(py);
        let worktree_type = reap.getattr("Worktree").unwrap();
        let row = worktree_type
            .call1((
                path(py, case.root()),
                "a".repeat(40),
                "llm-b0/trident2-sdsm-r9",
            ))
            .unwrap();
        assert_eq!(
            reap.getattr("slug_for")
                .unwrap()
                .call1((row,))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "llm-b0-trident2-sdsm-r9"
        );
        let plain = worktree_type
            .call1((path(py, &case.root().join("plain")),))
            .unwrap();
        assert_eq!(
            reap.getattr("slug_for")
                .unwrap()
                .call1((plain,))
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "plain"
        );
    });
}
