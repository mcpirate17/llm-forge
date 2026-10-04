#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for snapshotting and fast-forwarding fixture checkouts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{assert_error, module, path, text, Case};

fn checkout_case() -> Case {
    let mut case = Case::new();
    case.set_env("CONDUCTOR_INTEGRATION_BRANCH", "master");
    case.set_env("GIT_CONFIG_NOSYSTEM", "1");
    case.set_env("GIT_CONFIG_GLOBAL", "/dev/null");
    for key in ["GIT_INDEX_FILE", "GIT_DIR", "GIT_WORK_TREE"] {
        case.remove_env(key);
    }
    case
}

fn git(repo: &Path, args: &[&str]) -> String {
    let done = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run fixture Git command");
    assert!(
        done.status.success(),
        "git {args:?} in {} failed: {}",
        repo.display(),
        String::from_utf8_lossy(&done.stderr)
    );
    String::from_utf8(done.stdout).expect("UTF-8 fixture Git output")
}

fn pair(case: &Case) -> (PathBuf, PathBuf) {
    let upstream = case.mkdir("upstream");
    git(&upstream, &["init", "-q", "-b", "master"]);
    git(&upstream, &["config", "user.email", "t@example.invalid"]);
    git(&upstream, &["config", "user.name", "test"]);
    git(&upstream, &["config", "commit.gpgsign", "false"]);
    fs::write(upstream.join("tracked.txt"), "one\n").unwrap();
    git(&upstream, &["add", "-A"]);
    git(&upstream, &["commit", "-qm", "first"]);
    let clone = case.root().join("clone");
    git(
        case.root(),
        &[
            "clone",
            "-q",
            upstream.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    git(&clone, &["config", "user.email", "t@example.invalid"]);
    git(&clone, &["config", "user.name", "test"]);
    git(&clone, &["config", "commit.gpgsign", "false"]);
    (upstream, clone)
}

fn advance(upstream: &Path, clone: &Path, name: &str, body: &str) {
    fs::write(upstream.join(name), body).unwrap();
    git(upstream, &["add", "-A"]);
    git(upstream, &["commit", "-qm", &format!("add {name}")]);
    git(clone, &["fetch", "-q", "origin"]);
}

fn sync<'py>(py: Python<'py>, clone: &Path, dry_run: bool) -> Bound<'py, PyDict> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("dry_run", dry_run).unwrap();
    module(py, "conductor.checkout_sync")
        .getattr("sync")
        .unwrap()
        .call((path(py, clone),), Some(&kwargs))
        .unwrap()
        .cast_into::<PyDict>()
        .unwrap()
}

fn field<'py>(result: &Bound<'py, PyDict>, name: &str) -> Bound<'py, pyo3::types::PyAny> {
    result.get_item(name).unwrap().unwrap()
}

const MULTI: &str = "a\nb\nc\nd\ne\nf\ng\n";

fn seed_multi(upstream: &Path, clone: &Path) {
    advance(upstream, clone, "multi.txt", MULTI);
    git(clone, &["merge", "-q", "--ff-only", "origin/master"]);
}

fn classes(result: &Bound<'_, PyDict>) -> BTreeMap<String, String> {
    field(result, "classification")
        .extract::<BTreeMap<String, String>>()
        .unwrap()
}

#[test]
fn checkout_already_even_with_line_is_left_alone() {
    let case = checkout_case();
    let (_, clone) = pair(&case);
    Python::attach(|py| {
        let result = sync(py, &clone, false);
        assert_eq!(text(&field(&result, "outcome")), "already even");
        assert!(field(&result, "snapshot").is_none());
    });
}

#[test]
fn dirty_checkout_fast_forwards_and_whole_tree_is_recoverable() {
    let case = checkout_case();
    let (upstream, clone) = pair(&case);
    advance(&upstream, &clone, "incoming.txt", "landed\n");
    fs::write(clone.join("tracked.txt"), "edited locally\n").unwrap();
    fs::write(clone.join("untracked.txt"), "never committed\n").unwrap();
    Python::attach(|py| {
        let result = sync(py, &clone, false);
        assert_eq!(text(&field(&result, "outcome")), "fast-forwarded");
        assert_eq!(field(&result, "behind").extract::<i64>().unwrap(), 1);
        assert_eq!(
            fs::read_to_string(clone.join("incoming.txt")).unwrap(),
            "landed\n"
        );
        assert_eq!(
            fs::read_to_string(clone.join("tracked.txt")).unwrap(),
            "edited locally\n"
        );
        let saved = text(&field(&result, "snapshot"));
        let namespace = text(
            &module(py, "conductor.checkout_sync")
                .getattr("SNAPSHOT_NAMESPACE")
                .unwrap(),
        );
        assert!(saved.starts_with(&namespace));
        assert_eq!(
            git(&clone, &["show", &format!("{saved}:untracked.txt")]),
            "never committed\n"
        );
        assert_eq!(
            git(&clone, &["show", &format!("{saved}:tracked.txt")]),
            "edited locally\n"
        );
    });
}

#[test]
fn snapshot_never_stages_anything_in_callers_index() {
    let case = checkout_case();
    let (_, clone) = pair(&case);
    fs::write(clone.join("untracked.txt"), "never committed\n").unwrap();
    Python::attach(|py| {
        module(py, "conductor.checkout_sync")
            .getattr("snapshot")
            .unwrap()
            .call1((path(py, &clone),))
            .unwrap();
    });
    assert_eq!(git(&clone, &["diff", "--cached", "--name-only"]), "");
    assert!(git(&clone, &["status", "--porcelain"]).contains("?? untracked.txt"));
}

#[test]
fn clean_tree_has_nothing_to_snapshot() {
    let case = checkout_case();
    let (_, clone) = pair(&case);
    Python::attach(|py| {
        assert!(module(py, "conductor.checkout_sync")
            .getattr("snapshot")
            .unwrap()
            .call1((path(py, &clone),))
            .unwrap()
            .is_none());
    });
}

#[test]
fn tracked_file_changed_on_both_sides_blocks_merge_and_names_itself() {
    let case = checkout_case();
    let (upstream, clone) = pair(&case);
    advance(&upstream, &clone, "tracked.txt", "changed upstream\n");
    fs::write(clone.join("tracked.txt"), "changed locally\n").unwrap();
    let before = git(&clone, &["rev-parse", "HEAD"]);
    Python::attach(|py| {
        let result = sync(py, &clone, false);
        assert_eq!(text(&field(&result, "outcome")), "blocked");
        assert_eq!(
            field(&result, "blocked_by")
                .extract::<Vec<String>>()
                .unwrap(),
            ["tracked.txt"]
        );
        assert!(field(&result, "snapshot").is_none());
    });
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), before);
}

#[test]
fn checkout_holding_unlanded_commits_is_refused() {
    let case = checkout_case();
    let (upstream, clone) = pair(&case);
    advance(&upstream, &clone, "incoming.txt", "landed\n");
    fs::write(clone.join("local.txt"), "mine\n").unwrap();
    git(&clone, &["add", "-A"]);
    git(&clone, &["commit", "-qm", "local work"]);
    Python::attach(|py| {
        let source = module(py, "conductor.checkout_sync");
        let error = source
            .getattr("sync")
            .unwrap()
            .call1((path(py, &clone),))
            .unwrap_err();
        assert_error(
            py,
            error,
            &source.getattr("SyncError").unwrap(),
            "rather than fast-forwarding over them",
        );
    });
}

#[test]
fn dry_run_reports_move_without_making_it() {
    let case = checkout_case();
    let (upstream, clone) = pair(&case);
    advance(&upstream, &clone, "incoming.txt", "landed\n");
    let before = git(&clone, &["rev-parse", "HEAD"]);
    Python::attach(|py| {
        let result = sync(py, &clone, true);
        assert_eq!(text(&field(&result, "outcome")), "would fast-forward");
        assert!(field(&result, "snapshot").is_none());
    });
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), before);
    assert!(!clone.join("incoming.txt").exists());
}

#[test]
fn dirty_file_identical_to_upstream_is_clean_after_sync() {
    let case = checkout_case();
    let (upstream, clone) = pair(&case);
    fs::write(clone.join("tracked.txt"), "two\n").unwrap();
    advance(&upstream, &clone, "tracked.txt", "two\n");
    Python::attach(|py| {
        let result = sync(py, &clone, false);
        assert_eq!(text(&field(&result, "outcome")), "fast-forwarded");
        assert_eq!(classes(&result)["tracked.txt"], "identical");
    });
    assert_eq!(git(&clone, &["status", "--porcelain"]), "");
    assert_eq!(
        git(&clone, &["rev-parse", "HEAD"]),
        git(&clone, &["rev-parse", "origin/master"])
    );
}

#[test]
fn non_overlapping_hunk_is_carried_and_other_edits_survive() {
    let case = checkout_case();
    let (upstream, clone) = pair(&case);
    seed_multi(&upstream, &clone);
    fs::write(clone.join("multi.txt"), "A\nb\nc\nd\ne\nf\ng\n").unwrap();
    fs::write(clone.join("tracked.txt"), "edited locally\n").unwrap();
    fs::write(clone.join("staged.txt"), "staged\n").unwrap();
    git(&clone, &["add", "staged.txt"]);
    advance(&upstream, &clone, "multi.txt", "a\nb\nc\nd\ne\nf\nG\n");
    Python::attach(|py| {
        let result = sync(py, &clone, false);
        assert_eq!(text(&field(&result, "outcome")), "fast-forwarded");
        assert_eq!(classes(&result)["multi.txt"], "carried");
        assert!(!field(&result, "snapshot").is_none());
    });
    assert_eq!(
        fs::read_to_string(clone.join("multi.txt")).unwrap(),
        "A\nb\nc\nd\ne\nf\nG\n"
    );
    assert_eq!(
        fs::read_to_string(clone.join("tracked.txt")).unwrap(),
        "edited locally\n"
    );
    assert_eq!(
        git(&clone, &["diff", "--cached", "--name-only"]),
        "staged.txt\n"
    );
    assert_eq!(
        git(&clone, &["rev-parse", "HEAD"]),
        git(&clone, &["rev-parse", "origin/master"])
    );
}

#[test]
fn overlapping_hunk_is_refused_and_leaves_everything_unchanged() {
    let case = checkout_case();
    let (upstream, clone) = pair(&case);
    seed_multi(&upstream, &clone);
    fs::write(clone.join("multi.txt"), "a\nb\nc\nLOCAL\ne\nf\ng\n").unwrap();
    fs::write(clone.join("staged.txt"), "staged\n").unwrap();
    git(&clone, &["add", "staged.txt"]);
    advance(&upstream, &clone, "multi.txt", "a\nb\nc\nUP\ne\nf\ng\n");
    let head = git(&clone, &["rev-parse", "HEAD"]);
    let status = git(&clone, &["status", "--porcelain"]);
    let cached = git(&clone, &["diff", "--cached"]);
    Python::attach(|py| {
        let result = sync(py, &clone, false);
        assert_eq!(text(&field(&result, "outcome")), "blocked");
        assert_eq!(classes(&result)["multi.txt"], "conflict");
        assert!(field(&result, "snapshot").is_none());
    });
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&clone, &["status", "--porcelain"]), status);
    assert_eq!(git(&clone, &["diff", "--cached"]), cached);
    assert_eq!(
        fs::read_to_string(clone.join("multi.txt")).unwrap(),
        "a\nb\nc\nLOCAL\ne\nf\ng\n"
    );
}

#[test]
fn untracked_file_identical_to_incoming_is_absorbed() {
    let case = checkout_case();
    let (upstream, clone) = pair(&case);
    fs::write(clone.join("incoming.txt"), "landed\n").unwrap();
    advance(&upstream, &clone, "incoming.txt", "landed\n");
    Python::attach(|py| {
        let result = sync(py, &clone, false);
        assert_eq!(text(&field(&result, "outcome")), "fast-forwarded");
        assert_eq!(classes(&result)["incoming.txt"], "untracked-identical");
    });
    assert_eq!(git(&clone, &["status", "--porcelain"]), "");
    assert_eq!(
        fs::read_to_string(clone.join("incoming.txt")).unwrap(),
        "landed\n"
    );
}

#[test]
fn untracked_file_differing_from_incoming_blocks() {
    let case = checkout_case();
    let (upstream, clone) = pair(&case);
    fs::write(clone.join("incoming.txt"), "mine\n").unwrap();
    advance(&upstream, &clone, "incoming.txt", "landed\n");
    let head = git(&clone, &["rev-parse", "HEAD"]);
    Python::attach(|py| {
        let result = sync(py, &clone, false);
        assert_eq!(text(&field(&result, "outcome")), "blocked");
        assert_eq!(classes(&result)["incoming.txt"], "untracked-differs");
        assert_eq!(
            field(&result, "blocked_by")
                .extract::<Vec<String>>()
                .unwrap(),
            ["incoming.txt"]
        );
    });
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read_to_string(clone.join("incoming.txt")).unwrap(),
        "mine\n"
    );
}

#[test]
fn dry_run_classifies_every_overlap_without_touching_the_tree() {
    let case = checkout_case();
    let (upstream, clone) = pair(&case);
    seed_multi(&upstream, &clone);
    fs::write(clone.join("multi.txt"), "A\nb\nc\nd\ne\nf\ng\n").unwrap();
    fs::write(clone.join("tracked.txt"), "two\n").unwrap();
    fs::write(clone.join("incoming.txt"), "landed\n").unwrap();
    fs::write(upstream.join("multi.txt"), "a\nb\nc\nd\ne\nf\nG\n").unwrap();
    fs::write(upstream.join("tracked.txt"), "two\n").unwrap();
    advance(&upstream, &clone, "incoming.txt", "landed\n");
    let head = git(&clone, &["rev-parse", "HEAD"]);
    let status = git(&clone, &["status", "--porcelain"]);
    Python::attach(|py| {
        let result = sync(py, &clone, true);
        assert_eq!(text(&field(&result, "outcome")), "would fast-forward");
        assert!(field(&result, "snapshot").is_none());
        let expected: BTreeMap<String, String> = [
            ("incoming.txt", "untracked-identical"),
            ("multi.txt", "carried"),
            ("tracked.txt", "identical"),
        ]
        .into_iter()
        .map(|(path, kind)| (path.to_owned(), kind.to_owned()))
        .collect();
        assert_eq!(classes(&result), expected);
    });
    assert_eq!(git(&clone, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&clone, &["status", "--porcelain"]), status);
}

#[test]
fn render_lists_every_blocker_then_capped_rest_then_class_counts() {
    Python::attach(|py| {
        let classification = PyDict::new(py);
        for n in 0..60 {
            classification
                .set_item(format!("a{n:02}.txt"), "carried")
                .unwrap();
        }
        classification.set_item("b_same.txt", "identical").unwrap();
        classification
            .set_item("z_conflict.txt", "conflict")
            .unwrap();
        classification
            .set_item("z_untracked.txt", "untracked-differs")
            .unwrap();
        let result = PyDict::new(py);
        result.set_item("outcome", "blocked").unwrap();
        result.set_item("remote", "origin/master").unwrap();
        result.set_item("behind", 3).unwrap();
        result
            .set_item("blocked_by", vec!["z_conflict.txt", "z_untracked.txt"])
            .unwrap();
        result.set_item("classification", classification).unwrap();
        let rendered: String = module(py, "conductor.checkout_sync")
            .getattr("render")
            .unwrap()
            .call1((result,))
            .unwrap()
            .extract()
            .unwrap();
        let lines: Vec<&str> = rendered.lines().collect();
        let position = |needle: &str| lines.iter().position(|l| l.contains(needle));
        let conflict = position("z_conflict.txt").expect("conflict line kept");
        let untracked = position("z_untracked.txt").expect("untracked-differs line kept");
        assert!(conflict < position("a00.txt").unwrap());
        assert!(untracked < position("a00.txt").unwrap());
        assert!(position("a39.txt").is_some() && position("a40.txt").is_none());
        assert!(position("20 more not shown").is_some());
        assert!(position("b_same.txt").is_none());
        assert!(rendered.contains("60 carried, 1 conflict, 1 identical, 1 untracked-differs"));
        assert!(rendered.contains("2 local edit(s) conflict"));
    });
}
