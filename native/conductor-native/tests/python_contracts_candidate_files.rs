#![cfg(feature = "python-compat-tests")]
//! Candidate-review file selection, diff range and command expansion contracts.

#[path = "python_contracts/candidate_support.rs"]
#[allow(dead_code)]
mod candidate_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

#[path = "python_contracts/source_dirs_support.rs"]
mod source_dirs_support;

use candidate_support::{git, review_context};
use pyo3::prelude::*;
use pyo3::types::{PyCFunction, PyDict, PyModule};
use std::ffi::CStr;
use std::fs;
use std::path::Path;
use support::{module, path, AttrPatch, Case};

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

const RUN_PROBE: &CStr = c"import subprocess\n\n\ncalls = []\ncodes = []\n\n\ndef run(cmd, **kwargs):\n    calls.append((list(cmd), str(kwargs['cwd'])))\n    return subprocess.CompletedProcess(cmd, codes.pop(0) if codes else 0)\n";

fn probe(py: Python<'_>, codes: Vec<i32>) -> Bound<'_, PyModule> {
    let probe = PyModule::from_code(py, RUN_PROBE, c"run_probe.py", c"run_probe").unwrap();
    probe.setattr("codes", codes).unwrap();
    probe
}

fn call_audit<'py>(
    py: Python<'py>,
    entry: &str,
    root: &Path,
    check: bool,
    codes: Vec<i32>,
) -> Bound<'py, PyModule> {
    let audit = module(py, "conductor.run_duplicate_audit");
    let probe = probe(py, codes);
    let find = PyCFunction::new_closure(py, None, None, |_args, _kwargs| "/fake/nicad").unwrap();
    let _command = AttrPatch::replace(&audit, "command_path", &find);
    let _run = AttrPatch::replace(
        &audit.getattr("subprocess").unwrap(),
        "run",
        &probe.getattr("run").unwrap(),
    );
    let kwargs = PyDict::new(py);
    kwargs.set_item("check", check).unwrap();
    kwargs.set_item("root", path(py, root)).unwrap();
    let code: i32 = audit
        .getattr(entry)
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
        .extract()
        .unwrap();
    probe.setattr("code", code).unwrap();
    probe
}

#[test]
fn pylint_file_list_comes_from_pyproject_source_dirs() {
    let case = Case::new();
    let root = source_dirs_support::host(&case);
    Python::attach(|py| {
        let probe = call_audit(py, "run_pylint", &root, true, vec![]);
        assert_eq!(probe.getattr("code").unwrap().extract::<i32>().unwrap(), 0);
    });
    let listed = fs::read_to_string(root.join("tasks/audit/duplication-python-files.txt")).unwrap();
    assert!(
        listed.contains("a.py") && listed.contains("b.py"),
        "{listed}"
    );
    assert!(
        !listed.contains("c.py") && !listed.contains("research"),
        "{listed}"
    );
}

#[test]
fn nicad_runs_once_per_source_dir_in_its_own_output_directory_and_returns_the_worst_exit() {
    let case = Case::new();
    let root = source_dirs_support::host(&case);
    Python::attach(|py| {
        let probe = call_audit(py, "run_nicad_python", &root, true, vec![0, 3]);
        assert_eq!(probe.getattr("code").unwrap().extract::<i32>().unwrap(), 3);
        let calls: Vec<(Vec<String>, String)> = probe.getattr("calls").unwrap().extract().unwrap();
        let scanned: Vec<&str> = calls.iter().map(|(cmd, _)| cmd[3].as_str()).collect();
        assert_eq!(
            scanned,
            [
                root.join("pkg").to_str().unwrap(),
                root.join("lib/pkg").to_str().unwrap()
            ]
        );
        assert!(calls
            .iter()
            .all(|(cmd, _)| cmd[..3] == ["/fake/nicad", "functions", "py"]));
        // `pkg` and `lib/pkg` share a basename, and NiCad keys its results on it.
        let nicad = root.join("tasks/audit/nicad");
        let cwds: Vec<&str> = calls.iter().map(|(_, cwd)| cwd.as_str()).collect();
        assert_eq!(
            cwds,
            [
                nicad.join("pkg").to_str().unwrap(),
                nicad.join("lib__pkg").to_str().unwrap()
            ]
        );
        assert!(nicad.join("pkg").is_dir() && nicad.join("lib__pkg").is_dir());
        let probe = call_audit(py, "run_nicad_python", &root, false, vec![3, 4]);
        assert_eq!(probe.getattr("code").unwrap().extract::<i32>().unwrap(), 0);
        assert_eq!(probe.getattr("calls").unwrap().len().unwrap(), 2);
    });
}
