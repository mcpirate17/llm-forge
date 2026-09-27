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

const SRC_LAYOUT: &str = "[tool.conductor]\npackage_root = \"src/conductor\"\n";

fn graph_tree(case: &Case, relative: &str, layout: Option<&str>) {
    if let Some(layout) = layout {
        case.write("pyproject.toml", layout);
    }
    case.write(
        &format!("{relative}/widget.py"),
        "def go():\n    return 1\n",
    );
    case.write(
        &format!("{relative}/test_widget.py"),
        "from conductor.widget import go\n\ndef test_go():\n    assert go()\n",
    );
}

fn convention(py: Python<'_>, root: &Path, changed: &str) -> Vec<String> {
    let result = module(py, "conductor.candidate_review.graph_selection")
        .getattr("_convention_tests")
        .unwrap()
        .call1((review_context(py, root), vec![changed]))
        .unwrap();
    let mut files: Vec<String> = result
        .extract::<std::collections::HashSet<String>>()
        .unwrap()
        .into_iter()
        .collect();
    files.sort();
    files
}

#[test]
fn convention_tests_follow_configured_layout_and_source_imports() {
    let declared = Case::new();
    graph_tree(&declared, "src/conductor", Some(SRC_LAYOUT));
    Python::attach(|py| {
        assert_eq!(
            convention(py, declared.root(), "src/conductor/widget.py"),
            ["src/conductor/test_widget.py"]
        );
        declared.write(
            "src/conductor/test_elsewhere.py",
            "import conductor.widget\n",
        );
        assert_eq!(
            convention(py, declared.root(), "src/conductor/widget.py"),
            [
                "src/conductor/test_elsewhere.py",
                "src/conductor/test_widget.py"
            ]
        );
        declared.write(
            "src/conductor/test_other.py",
            "def test_other():\n    pass\n",
        );
        assert!(!convention(py, declared.root(), "src/conductor/widget.py")
            .contains(&"src/conductor/test_other.py".to_owned()));
    });
}

#[test]
fn convention_tests_keep_default_layout_and_skip_absent_roots() {
    let default = Case::new();
    graph_tree(&default, "conductor", None);
    Python::attach(|py| {
        assert_eq!(
            convention(py, default.root(), "conductor/widget.py"),
            ["conductor/test_widget.py"]
        );
    });
    drop(default);
    let unconfigured = Case::new();
    graph_tree(&unconfigured, "src/conductor", None);
    Python::attach(|py| {
        assert!(convention(py, unconfigured.root(), "src/conductor/widget.py").is_empty());
    });
    drop(unconfigured);
    let missing = Case::new();
    missing.write("pyproject.toml", SRC_LAYOUT);
    Python::attach(|py| {
        assert!(convention(py, missing.root(), "src/conductor/widget.py").is_empty());
    });
}

#[test]
fn module_names_strip_only_the_declared_package_prefix() {
    let case = Case::new();
    Python::attach(|py| {
        let names = module(py, "conductor.candidate_review.graph_selection")
            .getattr("_module_names")
            .unwrap();
        let call = |paths: Vec<&str>| -> Vec<String> {
            names
                .call1((path(py, case.root()), paths))
                .unwrap()
                .extract()
                .unwrap()
        };
        assert_eq!(call(vec!["conductor/widget.py"]), ["conductor.widget"]);
        assert_eq!(call(vec!["native/x/y.py"]), ["native.x.y"]);
        case.write("pyproject.toml", SRC_LAYOUT);
        assert_eq!(call(vec!["src/conductor/widget.py"]), ["conductor.widget"]);
        assert_eq!(call(vec!["src/tooling/hooks/a.py"]), ["tooling.hooks.a"]);
        assert_eq!(call(vec!["native/build.py"]), ["native.build"]);
    });
}
