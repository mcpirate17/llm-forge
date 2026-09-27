#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for cargo-fmt and cargo-clippy crate selection.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use support::{module, path, text, AttrPatch, Case};

const ROSTER: &str = r#"
[crates]
tested = ["a/keep", "a/skip", "a/held"]
linted = ["a/keep", "a/held"]
unstyled = ["a/skip"]
excluded = ["a/old"]

[globs]
manifests = ["a/*/Cargo.toml"]

[prerequisites."a/keep"]
artifact = "build/lib.a"
command = "make kernels"
reason = "needs the C archive"

# No artifact: nothing on disk can clear this one, so it is always blocked.
[prerequisites."a/held"]
command = "ask an operator"
reason = "the vendor SDK is not redistributable"
"#;

const BARE_ROSTER: &str = r#"
[crates]
tested = ["a/only"]

[globs]
manifests = ["a/*/Cargo.toml"]
"#;

fn crate_at(case: &Case, prefix: &str, name: &str) {
    case.write(&format!("{prefix}/{name}/Cargo.toml"), "[package]\n");
    case.write(&format!("{prefix}/{name}/src/lib.rs"), "");
}

fn roster_root(case: &Case) -> PathBuf {
    case.write("repo/tooling/native/crates.toml", ROSTER);
    for name in ["keep", "skip", "old", "held"] {
        crate_at(case, "repo", &format!("a/{name}"));
    }
    case.root().join("repo")
}

fn bare_root(case: &Case) -> PathBuf {
    case.write("repo/tooling/native/crates.toml", BARE_ROSTER);
    crate_at(case, "repo", "a/only");
    case.root().join("repo")
}

fn lint<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.cargo_lint_files").into_any()
}

fn roster<'py>(py: Python<'py>, lint: &Bound<'py, PyAny>, root: &Path) -> Bound<'py, PyAny> {
    lint.getattr("Roster")
        .unwrap()
        .call_method1("load", (path(py, root),))
        .unwrap()
}

fn selected(
    lint: &Bound<'_, PyAny>,
    mode: &str,
    crates: &[&str],
    roster: &Bound<'_, PyAny>,
) -> Vec<String> {
    lint.getattr("_selected")
        .unwrap()
        .call1((mode, crates, roster))
        .unwrap()
        .get_item(0)
        .unwrap()
        .extract()
        .unwrap()
}

fn owning_crate(
    py: Python<'_>,
    lint: &Bound<'_, PyAny>,
    file: &Path,
    root: &Path,
) -> Option<String> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("root", path(py, root)).unwrap();
    lint.getattr("owning_crate")
        .unwrap()
        .call((path(py, file),), Some(&kwargs))
        .unwrap()
        .extract()
        .unwrap()
}

fn assert_roster_error(py: Python<'_>, lint: &Bound<'_, PyAny>, root: &Path) -> String {
    let error = lint
        .getattr("Roster")
        .unwrap()
        .call_method1("load", (path(py, root),))
        .unwrap_err();
    assert!(error
        .matches(py, &lint.getattr("RosterError").unwrap())
        .unwrap());
    error.to_string()
}

#[test]
fn absent_roster_refuses() {
    let case = Case::new();
    Python::attach(|py| {
        let lint = lint(py);
        assert_roster_error(py, &lint, case.root());
    });
}

#[test]
fn malformed_roster_refuses() {
    let case = Case::new();
    case.write("tooling/native/crates.toml", "[crates\n");
    Python::attach(|py| {
        let lint = lint(py);
        assert_roster_error(py, &lint, case.root());
    });
}

#[test]
fn roster_without_manifest_globs_refuses() {
    let case = Case::new();
    case.write("tooling/native/crates.toml", "[crates]\ntested = []\n");
    Python::attach(|py| {
        let lint = lint(py);
        assert_roster_error(py, &lint, case.root());
    });
}

#[test]
fn unclassified_crate_is_reported() {
    let case = Case::new();
    let root = roster_root(&case);
    crate_at(&case, "repo", "a/new");
    Python::attach(|py| {
        let lint = lint(py);
        let unclassified: Vec<String> = roster(py, &lint, &root)
            .call_method0("unclassified")
            .unwrap()
            .extract()
            .unwrap();
        assert!(unclassified.contains(&"a/new".to_owned()));
    });
}

#[test]
fn fully_classified_tree_reports_nothing() {
    let case = Case::new();
    let root = roster_root(&case);
    Python::attach(|py| {
        let lint = lint(py);
        let unclassified: Vec<String> = roster(py, &lint, &root)
            .call_method0("unclassified")
            .unwrap()
            .extract()
            .unwrap();
        assert!(unclassified.is_empty());
    });
}

#[test]
fn prerequisite_blocks_only_until_artifact_exists() {
    let case = Case::new();
    let root = roster_root(&case);
    Python::attach(|py| {
        let lint = lint(py);
        let blocked: Option<String> = roster(py, &lint, &root)
            .call_method1("blocked_by_prerequisite", ("a/keep",))
            .unwrap()
            .extract()
            .unwrap();
        assert!(blocked.unwrap().contains("make kernels"));
        case.mkdir("repo/build");
        case.write("repo/build/lib.a", "");
        let cleared: Option<String> = roster(py, &lint, &root)
            .call_method1("blocked_by_prerequisite", ("a/keep",))
            .unwrap()
            .extract()
            .unwrap();
        assert!(cleared.is_none());
    });
}

#[test]
fn crate_without_prerequisite_is_never_blocked() {
    let case = Case::new();
    let root = roster_root(&case);
    Python::attach(|py| {
        let lint = lint(py);
        let blocked: Option<String> = roster(py, &lint, &root)
            .call_method1("blocked_by_prerequisite", ("a/skip",))
            .unwrap()
            .extract()
            .unwrap();
        assert!(blocked.is_none());
    });
}

#[test]
fn owning_crate_walks_up_to_manifest() {
    let case = Case::new();
    let root = roster_root(&case);
    Python::attach(|py| {
        let lint = lint(py);
        assert_eq!(
            owning_crate(py, &lint, &root.join("a/keep/src/lib.rs"), &root),
            Some("a/keep".to_owned())
        );
    });
}

#[test]
fn changed_crates_sorts_owners_and_keeps_orphans() {
    let case = Case::new();
    let root = roster_root(&case);
    case.write("repo/loose.rs", "");
    Python::attach(|py| {
        let lint = lint(py);
        let kwargs = PyDict::new(py);
        kwargs.set_item("root", path(py, &root)).unwrap();
        let result = lint
            .getattr("changed_crates")
            .unwrap()
            .call(
                (vec!["a/skip/Cargo.toml", "a/keep/Cargo.toml", "loose.rs"],),
                Some(&kwargs),
            )
            .unwrap();
        assert_eq!(
            result
                .get_item(0)
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            ["a/keep", "a/skip"]
        );
        assert_eq!(
            result
                .get_item(1)
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            ["loose.rs"]
        );
    });
}

#[test]
fn owning_crate_respects_root_boundary() {
    let case = Case::new();
    let root = roster_root(&case);
    case.write("Cargo.toml", "[package]\n");
    case.write("repo/loose.rs", "");
    Python::attach(|py| {
        let lint = lint(py);
        assert_eq!(owning_crate(py, &lint, &root.join("loose.rs"), &root), None);
        assert_eq!(
            owning_crate(py, &lint, &case.root().join("outer.rs"), &root),
            None
        );
    });
}

#[test]
fn owning_crate_prefers_nearest_manifest() {
    let case = Case::new();
    let root = roster_root(&case);
    case.write("repo/Cargo.toml", "[package]\n");
    Python::attach(|py| {
        let lint = lint(py);
        assert_eq!(
            owning_crate(py, &lint, &root.join("a/keep/src/lib.rs"), &root),
            Some("a/keep".to_owned())
        );
    });
}

#[test]
fn fmt_covers_every_crate_except_unstyled() {
    let case = Case::new();
    let root = roster_root(&case);
    Python::attach(|py| {
        let lint = lint(py);
        assert_eq!(
            selected(
                &lint,
                "fmt",
                &["a/keep", "a/skip", "a/old"],
                &roster(py, &lint, &root)
            ),
            ["a/keep", "a/old"]
        );
    });
}

#[test]
fn clippy_reaches_only_linted_crates_when_prerequisite_is_met() {
    let case = Case::new();
    let root = roster_root(&case);
    case.mkdir("repo/build");
    case.write("repo/build/lib.a", "");
    Python::attach(|py| {
        let lint = lint(py);
        assert_eq!(
            selected(
                &lint,
                "clippy",
                &["a/keep", "a/skip"],
                &roster(py, &lint, &root)
            ),
            ["a/keep"]
        );
    });
}

#[test]
fn clippy_skips_linted_crate_with_unmet_prerequisite() {
    let case = Case::new();
    let root = roster_root(&case);
    Python::attach(|py| {
        let lint = lint(py);
        assert!(selected(&lint, "clippy", &["a/held"], &roster(py, &lint, &root)).is_empty());
    });
}

#[test]
fn main_refuses_unclassified_crate() {
    let case = Case::new();
    roster_root(&case);
    let _cwd = case.chdir("repo");
    Python::attach(|py| {
        let lint = lint(py);
        let roster_class = lint.getattr("Roster").unwrap();
        let original = roster_class.getattr("unclassified").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("return_value", vec!["a/new"]).unwrap();
        let fake = module(py, "unittest.mock")
            .getattr("create_autospec")
            .unwrap()
            .call((original,), Some(&kwargs))
            .unwrap();
        let _unclassified = AttrPatch::replace(&roster_class, "unclassified", &fake);
        let status: i64 = lint
            .getattr("main")
            .unwrap()
            .call1((vec!["--mode", "fmt", "a/keep/src/lib.rs"],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(status, 1);
        assert_eq!(
            fake.getattr("call_count")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            1
        );
        let args = fake.getattr("call_args").unwrap().getattr("args").unwrap();
        assert_eq!(args.len().unwrap(), 1);
        assert!(args
            .get_item(0)
            .unwrap()
            .is_instance(&roster_class)
            .unwrap());
    });
}

#[test]
fn main_refuses_rust_file_without_owning_crate() {
    let case = Case::new();
    roster_root(&case);
    case.write("repo/loose.rs", "");
    let _cwd = case.chdir("repo");
    Python::attach(|py| {
        let lint = lint(py);
        let status: i64 = lint
            .getattr("main")
            .unwrap()
            .call1((vec!["--mode", "fmt", "loose.rs"],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(status, 1);
    });
}

#[test]
fn main_passes_when_no_changed_file_maps_to_crate() {
    let case = Case::new();
    bare_root(&case);
    let _cwd = case.chdir("repo");
    Python::attach(|py| {
        let lint = lint(py);
        let status: i64 = lint
            .getattr("main")
            .unwrap()
            .call1((vec!["--mode", "fmt"],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(status, 0);
    });
}

#[test]
fn default_roster_location_matches_project_paths_default() {
    let case = Case::new();
    Python::attach(|py| {
        let lint = lint(py);
        let paths = module(py, "conductor.project_paths");
        let relative = paths
            .getattr("crate_roster_relative")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        let default = paths.getattr("DEFAULT_CRATE_ROSTER").unwrap();
        let roster = lint.getattr("ROSTER").unwrap();
        assert_eq!(text(&relative), text(&roster));
        assert_eq!(text(&roster), text(&default));
    });
}

#[test]
fn host_can_override_roster_location() {
    let case = Case::new();
    case.write(
        "pyproject.toml",
        "[tool.conductor]\ncrate_roster = \"other/place/crates.toml\"\n",
    );
    let expected = case.write("other/place/crates.toml", BARE_ROSTER);
    crate_at(&case, ".", "a/only");
    Python::attach(|py| {
        let lint = lint(py);
        let paths = module(py, "conductor.project_paths");
        let tested: HashSet<String> = roster(py, &lint, case.root())
            .getattr("tested")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(tested, HashSet::from(["a/only".to_owned()]));
        let resolved = paths
            .getattr("crate_roster_path")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert_eq!(text(&resolved), expected.to_str().unwrap());
    });
}

#[test]
fn configured_missing_roster_error_names_resolved_path() {
    let case = Case::new();
    case.write(
        "pyproject.toml",
        "[tool.conductor]\ncrate_roster = \"elsewhere/crates.toml\"\n",
    );
    Python::attach(|py| {
        let lint = lint(py);
        let message = assert_roster_error(py, &lint, case.root());
        let expected = case.root().join("elsewhere/crates.toml");
        assert!(message.contains(expected.to_str().unwrap()));
    });
}
