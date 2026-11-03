#![cfg(feature = "python-compat-tests")]
//! Retained source changed-line contracts; review runtime and sharding cases removed.

#[path = "python_contracts/candidate_review_support.rs"]
#[allow(dead_code)]
mod candidate_review_support;
#[path = "python_contracts/candidate_runtime_support.rs"]
#[allow(dead_code)]
mod candidate_runtime_support;
#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use candidate_review_support as fixture;
use candidate_runtime_support as runtime;
use pyo3::prelude::*;
use pyo3::types::PySet;
use support::{module, path};

#[test]
fn changed_lines_score_a_move_by_its_edited_hunks_only() {
    let case = fixture::isolated_case();
    let repo = runtime::repo(&case);
    for (name, prefix) in [
        ("pure.py", "PURE"),
        ("edited.py", "EDIT"),
        ("touched.py", "TOUCH"),
    ] {
        runtime::write(&repo, name, &runtime::numbered(prefix, 8));
    }
    fixture::commit_all(&repo, "baseline");
    fixture::git(&repo, &["mv", "pure.py", "moved_pure.py"]);
    fixture::git(&repo, &["mv", "edited.py", "moved_edited.py"]);
    runtime::write(
        &repo,
        "moved_edited.py",
        &runtime::numbered("EDIT", 8).replace("EDIT_4 = 4\n", "EDIT_4 = 40\n"),
    );
    runtime::write(&repo, "added.py", &runtime::numbered("ADD", 4));
    runtime::write(
        &repo,
        "touched.py",
        &runtime::numbered("TOUCH", 8).replace("TOUCH_1 = 1\n", "TOUCH_1 = 11\n"),
    );
    fixture::git(&repo, &["add", "--all"]);
    Python::attach(|py| {
        let candidate = fixture::resolve_candidate(py, &repo, "index", None, None);
        let paths: Vec<String> = candidate
            .getattr("changes")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|row| row.unwrap().getattr("path").unwrap().extract().unwrap())
            .collect();
        let expected = ["moved_pure.py", "moved_edited.py", "added.py", "touched.py"];
        assert_eq!(
            paths
                .iter()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            expected.into_iter().collect()
        );
        let changed = module(py, "conductor.candidate_review.git_source")
            .getattr("changed_line_numbers")
            .unwrap()
            .call1((path(py, &repo), candidate, paths))
            .unwrap();
        let pure = changed
            .call_method1("get", ("moved_pure.py", PySet::empty(py).unwrap()))
            .unwrap();
        assert_eq!(pure.len().unwrap(), 0);
        assert!(!changed.contains("pure.py").unwrap());
        for (name, lines) in [
            ("moved_edited.py", vec![4]),
            ("added.py", vec![1, 2, 3, 4]),
            ("touched.py", vec![1]),
        ] {
            let actual: std::collections::BTreeSet<i32> =
                changed.get_item(name).unwrap().extract().unwrap();
            assert_eq!(actual, lines.into_iter().collect());
        }
    });
}
#[test]
fn changed_lines_pair_a_move_that_leaves_an_alias_shim() {
    let case = fixture::isolated_case();
    let repo = runtime::repo(&case);
    runtime::write(&repo, "tools/mod.py", &runtime::numbered("LINE", 40));
    fixture::commit_all(&repo, "baseline");
    runtime::write(
        &repo,
        "pkg/mod.py",
        &runtime::numbered("LINE", 40).replace("LINE_10 = 10\n", "LINE_10 = 100\n"),
    );
    runtime::write(
        &repo,
        "tools/mod.py",
        "from pkg.mod import LINE_1  # moved\n",
    );
    fixture::git(&repo, &["add", "--all"]);
    Python::attach(|py| {
        let candidate = fixture::resolve_candidate(py, &repo, "index", None, None);
        let changed = module(py, "conductor.candidate_review.git_source")
            .getattr("changed_line_numbers")
            .unwrap()
            .call1((path(py, &repo), candidate, ["pkg/mod.py", "tools/mod.py"]))
            .unwrap();
        let moved: std::collections::BTreeSet<i32> =
            changed.get_item("pkg/mod.py").unwrap().extract().unwrap();
        let shim: std::collections::BTreeSet<i32> =
            changed.get_item("tools/mod.py").unwrap().extract().unwrap();
        assert_eq!(moved, [10].into());
        assert_eq!(shim, [1].into());
    });
}
