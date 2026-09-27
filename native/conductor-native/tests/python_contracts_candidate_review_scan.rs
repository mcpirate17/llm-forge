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
use support::{module, AttrPatch, Case};

const UNREADABLE: &[u8] = b"\xff\xfe\x00 not utf-8";
const CLEAN: &str = "def alpha() -> int:\n    return 1\n";

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
    let owner = if check == "check_structure_audit" {
        "conductor.candidate_review.quality_checks"
    } else {
        "conductor.candidate_review.checks"
    };
    module(py, owner)
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
fn structure_audit_fails_closed_on_unreadable_changed_file() {
    let case = isolated_case();
    Python::attach(|py| {
        let context = scan_context(
            py,
            &case,
            &[("pkg/good.py", Some(CLEAN)), ("pkg/broken.py", None)],
            &["pkg/good.py", "pkg/broken.py"],
            &["python", "source"],
        );
        let checked = result(py, "check_structure_audit", &context);
        assert_eq!(
            severity(py, &checked, "unreadable-changed-file").as_deref(),
            Some("high")
        );
        assert_eq!(metric::<usize>(&checked, "files_skipped"), 1);
        let skipped = checked
            .getattr("metrics")
            .unwrap()
            .get_item("skipped_files")
            .unwrap();
        assert!(skipped.contains("pkg/broken.py").unwrap());
    });
}

#[test]
fn unreadable_unchanged_file_stays_advisory() {
    let case = isolated_case();
    Python::attach(|py| {
        let context = scan_context(
            py,
            &case,
            &[("pkg/good.py", Some(CLEAN)), ("vendor/broken.py", None)],
            &["pkg/good.py"],
            &["python", "source"],
        );
        let checked = result(py, "check_structure_audit", &context);
        assert_eq!(
            severity(py, &checked, "incomplete-scan").as_deref(),
            Some("medium")
        );
        assert_eq!(severity(py, &checked, "unreadable-changed-file"), None);
        assert_eq!(metric::<usize>(&checked, "files_skipped"), 1);
    });
}

#[test]
fn complete_scan_reports_coverage() {
    let case = isolated_case();
    Python::attach(|py| {
        let context = scan_context(
            py,
            &case,
            &[("pkg/good.py", Some(CLEAN))],
            &["pkg/good.py"],
            &["python", "source"],
        );
        let checked = result(py, "check_structure_audit", &context);
        assert_eq!(metric::<usize>(&checked, "files_skipped"), 0);
        assert_eq!(metric::<usize>(&checked, "files_read"), 1);
        assert_eq!(metric::<usize>(&checked, "files_expected"), 1);
        assert_eq!(
            checked
                .getattr("metrics")
                .unwrap()
                .get_item("skipped_files")
                .unwrap()
                .len()
                .unwrap(),
            0
        );
        assert_eq!(severity(py, &checked, "incomplete-scan"), None);
    });
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

#[test]
fn duplicate_bodies_fail_closed_on_unreadable_changed_file() {
    let case = isolated_case();
    Python::attach(|py| {
        let checks = module(py, "conductor.candidate_review.checks");
        let kwargs = PyDict::new(py);
        kwargs.set_item("return_value", PyDict::new(py)).unwrap();
        let mock = module(py, "unittest.mock")
            .getattr("Mock")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let _patch = AttrPatch::replace(checks.as_any(), "changed_line_numbers", &mock);
        let context = scan_context(
            py,
            &case,
            &[("pkg/good.py", Some(CLEAN)), ("pkg/broken.py", None)],
            &["pkg/good.py", "pkg/broken.py"],
            &["python", "source"],
        );
        let checked = result(py, "check_duplicate_function_bodies", &context);
        assert_eq!(
            severity(py, &checked, "unreadable-changed-file").as_deref(),
            Some("high")
        );
        assert_eq!(metric::<usize>(&checked, "files_skipped"), 1);
    });
}
