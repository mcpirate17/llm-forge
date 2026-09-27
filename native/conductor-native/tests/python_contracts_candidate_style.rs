#![cfg(feature = "python-compat-tests")]
//! Rust assertions for candidate style-scan wrapper contracts.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PyModule};
use std::path::Path;
use support::{module, path, text, AttrPatch, Case};

fn style<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.candidate_review.style_scan")
}

fn finding<'py>(py: Python<'py>, file: &str, line: i64) -> Bound<'py, PyDict> {
    let row = PyDict::new(py);
    for (key, value) in [
        ("path", file),
        ("rule", "dead/empty-function"),
        ("message", "m"),
    ] {
        row.set_item(key, value).unwrap();
    }
    row.set_item("line", line).unwrap();
    row
}

fn mock_return<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("return_value", value).unwrap();
    module(py, "unittest.mock")
        .getattr("Mock")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn mock_raise<'py>(py: Python<'py>, error: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("side_effect", error).unwrap();
    module(py, "unittest.mock")
        .getattr("Mock")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn scope<'py>(
    py: Python<'py>,
    rows: &Bound<'py, PyList>,
    base: &str,
    root: &Path,
) -> Bound<'py, PyAny> {
    style(py)
        .getattr("scope")
        .unwrap()
        .call1((rows, base, path(py, root)))
        .unwrap()
}

fn capture<'py>(py: Python<'py>, stream: &str) -> (Bound<'py, PyAny>, AttrPatch) {
    let sink = module(py, "io")
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let guard = AttrPatch::replace(&module(py, "sys").into_any(), stream, &sink);
    (sink, guard)
}

fn main(py: Python<'_>, args: &[&str]) -> i64 {
    style(py)
        .getattr("main")
        .unwrap()
        .call1((args,))
        .unwrap()
        .extract()
        .unwrap()
}

fn scan<'py>(py: Python<'py>, file: &Path, language: &str) -> Bound<'py, PyAny> {
    style(py)
        .getattr("_scan")
        .unwrap()
        .call1((vec![file.to_string_lossy().to_string()], language))
        .unwrap()
}

fn row_field_strings(rows: &Bound<'_, PyAny>, field: &str) -> Vec<String> {
    rows.try_iter()
        .unwrap()
        .map(|row| text(&row.unwrap().get_item(field).unwrap()))
        .collect()
}

#[test]
fn changed_range_includes_first_line() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(style(py)
            .getattr("in_range")
            .unwrap()
            .call1((4, vec![(4, 9)]))
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn changed_range_includes_last_line() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(style(py)
            .getattr("in_range")
            .unwrap()
            .call1((9, vec![(4, 9)]))
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn line_outside_every_range_is_not_in_scope() {
    let _case = Case::new();
    Python::attach(|py| {
        assert!(!style(py)
            .getattr("in_range")
            .unwrap()
            .call1((3, vec![(4, 9), (20, 22)]))
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}

#[test]
fn without_base_every_finding_is_kept() {
    let case = Case::new();
    Python::attach(|py| {
        let rows = PyList::new(py, [finding(py, "a.py", 1), finding(py, "a.py", 900)]).unwrap();
        let result = scope(py, &rows, "", case.root());
        assert!(result.get_item(0).unwrap().eq(&rows).unwrap());
        assert_eq!(result.get_item(1).unwrap().len().unwrap(), 0);
    });
}

#[test]
fn finding_on_unchanged_line_is_dropped() {
    let case = Case::new();
    Python::attach(|py| {
        let style = style(py);
        let ranges = PyList::new(py, [(1, 2)]).unwrap();
        let _patch = AttrPatch::replace(
            &style,
            "changed_line_ranges",
            &mock_return(py, &ranges.into_any()),
        );
        let rows = PyList::new(py, [finding(py, "a.py", 10)]).unwrap();
        let result = scope(py, &rows, "HEAD", case.root());
        assert_eq!(result.get_item(0).unwrap().len().unwrap(), 0);
    });
}

#[test]
fn finding_on_changed_line_is_kept() {
    let case = Case::new();
    Python::attach(|py| {
        let style = style(py);
        let ranges = PyList::new(py, [(9, 11)]).unwrap();
        let _patch = AttrPatch::replace(
            &style,
            "changed_line_ranges",
            &mock_return(py, &ranges.into_any()),
        );
        let rows = PyList::new(py, [finding(py, "a.py", 10)]).unwrap();
        let result = scope(py, &rows, "HEAD", case.root());
        assert!(result.get_item(0).unwrap().eq(&rows).unwrap());
    });
}

#[test]
fn each_file_is_diffed_once_for_many_findings() {
    let case = Case::new();
    Python::attach(|py| {
        let style = style(py);
        let ranges = PyList::new(py, [(1, 999)]).unwrap();
        let mock = mock_return(py, &ranges.into_any());
        let _patch = AttrPatch::replace(&style, "changed_line_ranges", &mock);
        let rows = PyList::new(
            py,
            [
                finding(py, "a.py", 1),
                finding(py, "a.py", 2),
                finding(py, "a.py", 3),
                finding(py, "b.py", 4),
            ],
        )
        .unwrap();
        let result = scope(py, &rows, "HEAD", case.root());
        assert!(result.get_item(0).unwrap().eq(&rows).unwrap());
        let calls = mock.getattr("call_args_list").unwrap();
        assert_eq!(calls.len().unwrap(), 2);
        for (index, expected) in ["a.py", "b.py"].iter().enumerate() {
            let call = calls.get_item(index).unwrap();
            assert_eq!(
                text(&call.getattr("args").unwrap().get_item(0).unwrap()),
                *expected
            );
            let kwargs = call.getattr("kwargs").unwrap();
            assert_eq!(text(&kwargs.get_item("base").unwrap()), "HEAD");
            assert_eq!(
                text(&kwargs.get_item("cwd").unwrap()),
                case.root().to_string_lossy()
            );
        }
    });
}

#[test]
fn undiffable_file_is_reported_rather_than_raised() {
    let case = Case::new();
    Python::attach(|py| {
        let style = style(py);
        let error = module(py, "conductor.candidate_review.diff_ranges")
            .getattr("DiffError")
            .unwrap()
            .call1(("git diff failed on a.py",))
            .unwrap();
        let _patch = AttrPatch::replace(&style, "changed_line_ranges", &mock_raise(py, &error));
        let rows = PyList::new(py, [finding(py, "a.py", 10)]).unwrap();
        let result = scope(py, &rows, "HEAD", case.root());
        assert_eq!(result.get_item(0).unwrap().len().unwrap(), 0);
        assert_eq!(
            result
                .get_item(1)
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            ["git diff failed on a.py"]
        );
    });
}

#[test]
fn only_python_files_reach_scanner() {
    let _case = Case::new();
    Python::attach(|py| {
        let style = style(py);
        let mock = mock_return(py, &PyList::empty(py).into_any());
        let _patch = AttrPatch::replace(&style, "_scan", &mock);
        assert_eq!(main(py, &["a.py", "notes.md", "Makefile"]), 0);
        let args = mock.getattr("call_args").unwrap().getattr("args").unwrap();
        assert_eq!(
            args.get_item(0).unwrap().extract::<Vec<String>>().unwrap(),
            ["a.py"]
        );
        assert_eq!(text(&args.get_item(1).unwrap()), "python");
        assert_eq!(
            mock.getattr("call_count")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            1
        );
    });
}

#[test]
fn no_python_file_never_builds_scanner() {
    let _case = Case::new();
    Python::attach(|py| {
        let style = style(py);
        let mock = mock_raise(
            py,
            &module(py, "builtins")
                .getattr("AssertionError")
                .unwrap()
                .call1(("scanner must not run",))
                .unwrap(),
        );
        let _patch = AttrPatch::replace(&style, "_scan", &mock);
        assert_eq!(main(py, &["CHANGELOG"]), 0);
        assert_eq!(
            mock.getattr("call_count")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            0
        );
    });
}

#[test]
fn version_answers_without_native_extension() {
    let _case = Case::new();
    Python::attach(|py| {
        let style = style(py);
        let mock = mock_raise(
            py,
            &module(py, "builtins")
                .getattr("AssertionError")
                .unwrap()
                .call1(("scanner must not run",))
                .unwrap(),
        );
        let _patch = AttrPatch::replace(&style, "_scan", &mock);
        let (stdout, _capture) = capture(py, "stdout");
        assert_eq!(main(py, &["--version"]), 0);
        assert!(text(&stdout.call_method0("getvalue").unwrap()).contains("style-scan"));
        assert_eq!(
            mock.getattr("call_count")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            0
        );
    });
}

#[test]
fn undiffable_file_fails_check() {
    let _case = Case::new();
    Python::attach(|py| {
        let style = style(py);
        let rows = PyList::new(py, [finding(py, "a.py", 10)]).unwrap();
        let _scan = AttrPatch::replace(&style, "_scan", &mock_return(py, &rows.into_any()));
        let result = (Vec::<i64>::new(), vec!["git diff failed"]);
        let value = pyo3::types::PyTuple::new(
            py,
            [
                PyList::empty(py).into_any(),
                PyList::new(py, result.1).unwrap().into_any(),
            ],
        )
        .unwrap();
        let _scope = AttrPatch::replace(&style, "scope", &mock_return(py, &value.into_any()));
        let (stderr, _capture) = capture(py, "stderr");
        assert_eq!(main(py, &["--base", "HEAD", "a.py"]), 1);
        assert!(text(&stderr.call_method0("getvalue").unwrap()).contains("git diff failed"));
    });
}

#[test]
fn surviving_finding_fails_check() {
    let _case = Case::new();
    Python::attach(|py| {
        let style = style(py);
        let rows = PyList::new(py, [finding(py, "a.py", 10)]).unwrap();
        let _scan = AttrPatch::replace(&style, "_scan", &mock_return(py, &rows.clone().into_any()));
        let value =
            pyo3::types::PyTuple::new(py, [rows.into_any(), PyList::empty(py).into_any()]).unwrap();
        let _scope = AttrPatch::replace(&style, "scope", &mock_return(py, &value.into_any()));
        assert_eq!(main(py, &["a.py"]), 1);
    });
}

#[test]
fn scoped_away_findings_leave_check_passing() {
    let _case = Case::new();
    Python::attach(|py| {
        let style = style(py);
        let rows = PyList::new(py, [finding(py, "a.py", 10)]).unwrap();
        let _scan = AttrPatch::replace(&style, "_scan", &mock_return(py, &rows.into_any()));
        let value = pyo3::types::PyTuple::new(
            py,
            [PyList::empty(py).into_any(), PyList::empty(py).into_any()],
        )
        .unwrap();
        let _scope = AttrPatch::replace(&style, "scope", &mock_return(py, &value.into_any()));
        assert_eq!(main(py, &["a.py"]), 0);
    });
}

#[test]
fn findings_are_printed_on_stderr() {
    let _case = Case::new();
    Python::attach(|py| {
        let style = style(py);
        let rows = PyList::new(py, [finding(py, "a.py", 7)]).unwrap();
        let _scan = AttrPatch::replace(&style, "_scan", &mock_return(py, &rows.clone().into_any()));
        let value =
            pyo3::types::PyTuple::new(py, [rows.into_any(), PyList::empty(py).into_any()]).unwrap();
        let _scope = AttrPatch::replace(&style, "scope", &mock_return(py, &value.into_any()));
        let (stderr, _err) = capture(py, "stderr");
        let (stdout, _out) = capture(py, "stdout");
        assert_eq!(main(py, &["a.py"]), 1);
        assert!(text(&stderr.call_method0("getvalue").unwrap()).contains("a.py:7"));
        assert_eq!(text(&stdout.call_method0("getvalue").unwrap()), "");
    });
}

#[test]
fn published_python_rule_list_matches_scanner() {
    let _case = Case::new();
    Python::attach(|py| {
        let rules: Vec<String> = module(py, "slop_core")
            .getattr("style_scan_rules")
            .unwrap()
            .call0()
            .unwrap()
            .extract()
            .unwrap();
        let published: Vec<String> = style(py).getattr("RULES").unwrap().extract().unwrap();
        assert_eq!(rules, published[..published.len() - 1]);
    });
}

#[test]
fn swallowed_error_uses_published_rule_name() {
    let case = Case::new();
    let source = case.write(
        "swallow.py",
        "try:\n    risky()\nexcept ValueError:\n    pass\n",
    );
    Python::attach(|py| {
        let rows = scan(py, &source, "python");
        assert_eq!(
            row_field_strings(&rows, "rule"),
            ["failure/silent-fallback"]
        );
        let published: Vec<String> = style(py).getattr("RULES").unwrap().extract().unwrap();
        assert!(published.contains(&"failure/silent-fallback".to_owned()));
    });
}

#[test]
fn both_scanners_report_one_pass_down_file() {
    let case = Case::new();
    let source = case.write("mixed.py", "def swallow():\n    try:\n        risky()\n    except ValueError:\n        pass\n\n\ndef stub():\n    pass\n");
    Python::attach(|py| {
        let rows = scan(py, &source, "python");
        assert_eq!(
            row_field_strings(&rows, "rule"),
            ["failure/silent-fallback", "dead/empty-function"]
        );
        assert_eq!(
            rows.try_iter()
                .unwrap()
                .map(|row| row
                    .unwrap()
                    .get_item("line")
                    .unwrap()
                    .extract::<i64>()
                    .unwrap())
                .collect::<Vec<_>>(),
            [4, 8]
        );
    });
}

#[test]
fn only_rust_files_reach_rust_scanner() {
    let _case = Case::new();
    Python::attach(|py| {
        let style = style(py);
        let mock = mock_return(py, &PyList::empty(py).into_any());
        let _patch = AttrPatch::replace(&style, "_scan", &mock);
        assert_eq!(
            main(
                py,
                &[
                    "--language",
                    "rust",
                    "a.rs",
                    "Cargo.toml",
                    "Cargo.lock",
                    "b.py"
                ]
            ),
            0
        );
        let args = mock.getattr("call_args").unwrap().getattr("args").unwrap();
        assert_eq!(
            args.get_item(0).unwrap().extract::<Vec<String>>().unwrap(),
            ["a.rs"]
        );
        assert_eq!(text(&args.get_item(1).unwrap()), "rust");
        assert_eq!(
            mock.getattr("call_count")
                .unwrap()
                .extract::<i64>()
                .unwrap(),
            1
        );
    });
}

#[test]
fn language_reaches_scanner() {
    let _case = Case::new();
    Python::attach(|py| {
        let style = style(py);
        let mock = mock_return(py, &PyList::empty(py).into_any());
        let _patch = AttrPatch::replace(&style, "_scan", &mock);
        assert_eq!(main(py, &["--language", "rust", "a.rs"]), 0);
        assert_eq!(main(py, &["a.py"]), 0);
        let calls = mock.getattr("call_args_list").unwrap();
        assert_eq!(calls.len().unwrap(), 2);
        let languages: Vec<String> = (0..2)
            .map(|index| {
                text(
                    &calls
                        .get_item(index)
                        .unwrap()
                        .getattr("args")
                        .unwrap()
                        .get_item(1)
                        .unwrap(),
                )
            })
            .collect();
        assert_eq!(languages, ["rust", "python"]);
    });
}

#[test]
fn published_rust_rule_list_matches_scanner() {
    let _case = Case::new();
    Python::attach(|py| {
        let mut rules: Vec<String> = module(py, "slop_core")
            .getattr("rust_scan_rules")
            .unwrap()
            .call0()
            .unwrap()
            .extract()
            .unwrap();
        rules.sort();
        let published: Vec<String> = style(py).getattr("RUST_RULES").unwrap().extract().unwrap();
        assert_eq!(rules, published);
    });
}

#[test]
fn production_unwrap_uses_published_rule_name() {
    let case = Case::new();
    let source = case.write(
        "sample.rs",
        "fn run(v: Vec<u32>) -> u32 {\n    *v.first().unwrap()\n}\n",
    );
    Python::attach(|py| {
        assert_eq!(
            row_field_strings(&scan(py, &source, "rust"), "rule"),
            ["failure/rust-unwrap"]
        );
        let published: Vec<String> = style(py).getattr("RUST_RULES").unwrap().extract().unwrap();
        assert!(published.contains(&"failure/rust-unwrap".to_owned()));
    });
}

#[test]
fn hardcoded_endpoint_uses_published_rule_and_binding_exemption() {
    let case = Case::new();
    let source = case.write(
        "client.py",
        "def probe():\n    return get('http://127.0.0.1:7317/v1/embeddings')\n",
    );
    Python::attach(|py| {
        assert_eq!(
            row_field_strings(&scan(py, &source, "python"), "rule"),
            ["config/hardcoded-endpoint"]
        );
        let published: Vec<String> = style(py).getattr("RULES").unwrap().extract().unwrap();
        assert!(published.contains(&"config/hardcoded-endpoint".to_owned()));
        std::fs::write(&source, "EMBEDDINGS = 'http://127.0.0.1:7317/v1/embeddings'\n\ndef probe():\n    return get(EMBEDDINGS)\n").unwrap();
        assert_eq!(scan(py, &source, "python").len().unwrap(), 0);
    });
}
