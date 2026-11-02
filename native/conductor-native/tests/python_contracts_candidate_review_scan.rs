#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for candidate scan coverage and unreadable files.

#[path = "python_contracts/candidate_review_support.rs"]
#[allow(dead_code)]
mod candidate_review_support;
#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use candidate_review_support::{added_change, default_policy, isolated_case, review_context};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use std::fs;
use support::{module, Case};

const UNREADABLE: &[u8] = b"\xff\xfe\x00 not utf-8";

fn scan_context<'py>(
    py: Python<'py>,
    case: &Case,
    files: &[(&str, Option<&str>)],
    changed: &[&str],
    classes: &[&str],
) -> Bound<'py, PyAny> {
    let snapshot = case.root().join("snapshot");
    for (relative, contents) in files {
        let target = snapshot.join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, contents.map_or(UNREADABLE, str::as_bytes)).unwrap();
    }
    let model = module(py, "conductor.candidate_review.model");
    let kwargs = PyDict::new(py);
    kwargs.set_item("kind", "index").unwrap();
    kwargs.set_item("tree_oid", "a".repeat(40)).unwrap();
    kwargs.set_item("base_tree_oid", "b".repeat(40)).unwrap();
    kwargs.set_item("base_commit_oid", "c".repeat(40)).unwrap();
    kwargs.set_item("commit_oid", py.None()).unwrap();
    kwargs.set_item("target_ref", "HEAD").unwrap();
    let changes = changed
        .iter()
        .map(|relative| added_change(py, relative, classes));
    kwargs
        .set_item("changes", PyTuple::new(py, changes).unwrap())
        .unwrap();
    let candidate = model
        .getattr("Candidate")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap();
    review_context(
        py,
        &case.root().join("repo"),
        &snapshot,
        &candidate,
        PyTuple::empty(py).as_any(),
        &default_policy(py),
        "manual",
        "full",
        None,
        &case.root().join("runtime"),
    )
}

fn result<'py>(py: Python<'py>, check: &str, context: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.checks")
        .getattr(check)
        .unwrap()
        .call1((context,))
        .unwrap()
}

fn severity(py: Python<'_>, result: &Bound<'_, PyAny>, rule: &str) -> Option<String> {
    let _ = py;
    result
        .getattr("findings")
        .unwrap()
        .try_iter()
        .unwrap()
        .find_map(|finding| {
            let finding = finding.unwrap();
            let id: String = finding.getattr("rule_id").unwrap().extract().unwrap();
            (id == rule).then(|| finding.getattr("severity").unwrap().extract().unwrap())
        })
}

fn metric<T: for<'a> FromPyObject<'a, 'a>>(result: &Bound<'_, PyAny>, key: &str) -> T {
    result
        .getattr("metrics")
        .unwrap()
        .get_item(key)
        .unwrap()
        .extract()
        .ok()
        .unwrap()
}

#[test]
fn native_source_unreadable_file_is_critical() {
    let case = isolated_case();
    Python::attach(|py| {
        let context = scan_context(
            py,
            &case,
            &[
                ("src/a.c", Some("int main(void) { return 0; }\n")),
                ("src/b.c", None),
            ],
            &["src/a.c", "src/b.c"],
            &["native", "source"],
        );
        let checked = result(py, "check_native_source", &context);
        assert_eq!(
            severity(py, &checked, "unreadable-changed-file").as_deref(),
            Some("critical")
        );
        assert_eq!(metric::<usize>(&checked, "files_skipped"), 1);
    });
}

#[test]
fn native_source_still_flags_readable_unsafe_api() {
    let case = isolated_case();
    Python::attach(|py| {
        let context = scan_context(
            py,
            &case,
            &[(
                "src/a.c",
                Some("void f(char *d, char *s) { strcpy(d, s); }\n"),
            )],
            &["src/a.c"],
            &["native", "source"],
        );
        let checked = result(py, "check_native_source", &context);
        assert_eq!(
            severity(py, &checked, "unsafe-native-api").as_deref(),
            Some("critical")
        );
        assert_eq!(metric::<usize>(&checked, "files_skipped"), 0);
        assert_eq!(metric::<usize>(&checked, "files_read"), 1);
    });
}
