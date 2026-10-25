#![cfg(feature = "python-compat-tests")]
//! Candidate-review file selection, diff range and command expansion contracts.

#[path = "python_contracts/candidate_support.rs"]
#[allow(dead_code)]
mod candidate_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use candidate_support::{git, review_context};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::fs;
use std::path::Path;
use support::{module, path, Case};

fn ranges(value: &Bound<'_, PyAny>) -> Vec<(i64, i64)> {
    value.extract().unwrap()
}

#[test]
fn diff_hunks_keep_counts_deletions_anchoring_and_all_hunks() {
    let _case = Case::new();
    Python::attach(|py| {
        let parse = module(py, "conductor.candidate_review.diff_ranges")
            .getattr("parse_hunks")
            .unwrap();
        for (diff, expected) in [
            ("@@ -4 +7 @@ void f()", vec![(7, 7)]),
            ("@@ -1,1 +10,3 @@", vec![(10, 12)]),
            ("@@ -5,4 +4,0 @@", vec![]),
            ("@@ -1,2 +1,2 @@\n@@ -9,2 +20,2 @@", vec![(1, 2), (20, 21)]),
        ] {
            assert_eq!(ranges(&parse.call1((diff,)).unwrap()), expected, "{diff}");
        }
        let embedded = "@@ -1,1 +1,1 @@\n-int a;\n+const char *s = \"@@ -9,1 +99,1 @@\";\n";
        assert_eq!(ranges(&parse.call1((embedded,)).unwrap()), vec![(1, 1)]);
    });
}

#[test]
fn diff_ranges_merge_touching_nested_and_unsorted_inputs() {
    let _case = Case::new();
    Python::attach(|py| {
        let merge = module(py, "conductor.candidate_review.diff_ranges")
            .getattr("merge_ranges")
            .unwrap();
        for (input, expected) in [
            (vec![(1, 3), (4, 5), (9, 9)], vec![(1, 5), (9, 9)]),
            (vec![(1, 20), (5, 6)], vec![(1, 20)]),
            (vec![(9, 9), (1, 3)], vec![(1, 3), (9, 9)]),
        ] {
            assert_eq!(
                ranges(&merge.call1((input.clone(),)).unwrap()),
                expected,
                "{input:?}"
            );
        }
    });
}

#[test]
fn changed_line_ranges_uses_zero_context_and_refuses_bad_base() {
    let case = Case::new();
    let repo = case.mkdir("repo");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "t"]);
    fs::write(repo.join("f.c"), "a\nb\nc\nd\ne\nf\ng\n").unwrap();
    git(&repo, &["add", "f.c"]);
    git(&repo, &["commit", "-qm", "base"]);
    fs::write(repo.join("f.c"), "a\nB\nC\nd\ne\nf\ng\n").unwrap();
    Python::attach(|py| {
        let diff = module(py, "conductor.candidate_review.diff_ranges");
        let changed = diff.getattr("changed_line_ranges").unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("base", "HEAD").unwrap();
        kwargs.set_item("cwd", path(py, &repo)).unwrap();
        assert_eq!(
            ranges(&changed.call(("f.c",), Some(&kwargs)).unwrap()),
            vec![(2, 3)]
        );
        kwargs.set_item("base", "does-not-exist").unwrap();
        let error = changed.call(("f.c",), Some(&kwargs)).unwrap_err();
        assert!(error
            .matches(py, &diff.getattr("DiffError").unwrap())
            .unwrap());
    });
}

fn expand<'py>(
    py: Python<'py>,
    root: &Path,
    tokens: &[&str],
    files: &[&str],
    check_id: &str,
) -> Vec<String> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("check_id", check_id).unwrap();
    module(py, "conductor.candidate_review.command_runner")
        .getattr("_expand_command")
        .unwrap()
        .call((tokens, review_context(py, root), files), Some(&kwargs))
        .unwrap()
        .extract()
        .unwrap()
}

#[test]
fn command_expansion_writes_scoped_changed_file_lists() {
    let case = Case::new();
    Python::attach(|py| {
        let command = expand(
            py,
            case.root(),
            &["vulture", "--changed-files-from", "{changed_files_file}"],
            &["conductor/a.py", "conductor/b.py"],
            "vulture",
        );
        assert_eq!(&command[..2], ["vulture", "--changed-files-from"]);
        let written = Path::new(&command[2]);
        assert_eq!(written.parent().unwrap(), case.root().join("runtime"));
        assert_eq!(
            fs::read_to_string(written).unwrap(),
            "conductor/a.py\nconductor/b.py\n"
        );
        let empty = expand(
            py,
            case.root(),
            &["vulture", "{changed_files_file}"],
            &[],
            "empty",
        );
        assert_eq!(fs::read_to_string(&empty[1]).unwrap(), "");
        let other = expand(
            py,
            case.root(),
            &["jscpd", "{changed_files_file}"],
            &["a.py"],
            "jscpd",
        );
        assert_ne!(command[2], other[1]);
        assert_eq!(fs::read_to_string(&other[1]).unwrap().trim(), "a.py");
    });
}

#[test]
fn command_expansion_keeps_inline_files_and_skips_unused_scratch() {
    let case = Case::new();
    Python::attach(|py| {
        let normal = expand(
            py,
            case.root(),
            &["{python}", "-m", "conductor.run_duplicate_audit", "--check"],
            &["conductor/a.py"],
            "jscpd",
        );
        assert!(!normal.iter().any(|token| token == "{changed_files_file}"));
        assert!(!case.root().join("runtime/changed-files-jscpd.txt").exists());
        let inline = expand(
            py,
            case.root(),
            &["cppcheck", "{files}"],
            &["conductor/a.py", "conductor/b.py"],
            "cppcheck",
        );
        assert_eq!(inline, ["cppcheck", "conductor/a.py", "conductor/b.py"]);
    });
}
