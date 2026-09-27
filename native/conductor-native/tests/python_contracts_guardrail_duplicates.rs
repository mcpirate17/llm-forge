#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the shipped indexed Pylint duplicate scanner.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use std::fs;
use support::{assert_error, module, path, text, Case};

type Similarity = (usize, Vec<(String, usize, usize)>);

fn isolated_case() -> Case {
    let mut case = Case::new();
    case.remove_env("PYLINT_HOME");
    let pylint_config = case.write("pylint-default.rc", "[MASTER]\n");
    case.set_env("PYLINTRC", pylint_config.to_str().unwrap());
    case.set_env("CUDA_VISIBLE_DEVICES", "");
    case
}

fn duplicates<'py>(py: Python<'py>) -> Bound<'py, PyModule> {
    module(py, "conductor.guardrail_duplicates")
}

fn body(count: usize) -> String {
    (0..count)
        .map(|number| format!("    value_{number} = {number}\n"))
        .collect()
}

fn checker<'py>(py: Python<'py>, indexed: bool) -> Bound<'py, PyAny> {
    let class = if indexed {
        duplicates(py).getattr("_IndexedSymilar").unwrap()
    } else {
        module(py, "pylint.checkers.symilar")
            .getattr("Symilar")
            .unwrap()
    };
    let kwargs = PyDict::new(py);
    kwargs.set_item("min_lines", 10).unwrap();
    for name in [
        "ignore_comments",
        "ignore_docstrings",
        "ignore_imports",
        "ignore_signatures",
    ] {
        kwargs.set_item(name, true).unwrap();
    }
    class.call((), Some(&kwargs)).unwrap()
}

fn result(
    py: Python<'_>,
    checker: &Bound<'_, PyAny>,
    sources: &[(String, String)],
) -> Vec<Similarity> {
    let string_io = module(py, "io").getattr("StringIO").unwrap();
    for (name, source) in sources {
        let stream = string_io.call1((source,)).unwrap();
        checker
            .call_method1("append_stream", (name, stream))
            .unwrap();
    }
    let mut found = Vec::new();
    for row in checker
        .call_method0("_compute_sims")
        .unwrap()
        .try_iter()
        .unwrap()
    {
        let row = row.unwrap();
        let count: usize = row.get_item(0).unwrap().extract().unwrap();
        let mut entries = Vec::new();
        for entry in row.get_item(1).unwrap().try_iter().unwrap() {
            let entry = entry.unwrap();
            entries.push((
                entry
                    .get_item(0)
                    .unwrap()
                    .getattr("name")
                    .unwrap()
                    .extract()
                    .unwrap(),
                entry.get_item(1).unwrap().extract().unwrap(),
                entry.get_item(2).unwrap().extract().unwrap(),
            ));
        }
        entries.sort();
        found.push((count, entries));
    }
    found.sort();
    found
}

fn scan<'py>(py: Python<'py>, case: &Case, targets: &[&str]) -> Bound<'py, PyAny> {
    duplicates(py)
        .getattr("scan_duplicates")
        .unwrap()
        .call1((path(py, case.root()), targets))
        .unwrap()
}

#[test]
fn indexed_matches_pylint_across_directories_and_ignored_lines() {
    let _case = isolated_case();
    let repeated = body(16);
    let unrelated: String = (0..16).map(|i| format!("    other_{i} = {i}\n")).collect();
    let sources = vec![
        (
            "first/a.py".to_owned(),
            format!("import os\ndef first():\n    \"\"\"first doc\"\"\"\n{repeated}"),
        ),
        ("unrelated/z.py".to_owned(), format!("def unrelated():\n{unrelated}")),
        (
            "distant/b.py".to_owned(),
            format!("import sys\ndef second(argument):\n    \"\"\"other doc\"\"\"\n# comment\n{repeated}"),
        ),
    ];
    Python::attach(|py| {
        let indexed = checker(py, true);
        let actual = result(py, &indexed, &sources);
        assert_eq!(actual, result(py, &checker(py, false), &sources));
        assert_eq!(actual.len(), 1);
        let names: std::collections::HashSet<_> =
            actual[0].1.iter().map(|entry| entry.0.as_str()).collect();
        assert_eq!(names, ["first/a.py", "distant/b.py"].into_iter().collect());
        assert_eq!(
            indexed
                .getattr("candidate_count")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            1
        );
    });
}

fn threshold_case(count: usize) {
    let _case = isolated_case();
    let sources = vec![
        ("a.py".to_owned(), format!("def a():\n{}", body(count))),
        ("b.py".to_owned(), format!("def b():\n{}", body(count))),
    ];
    Python::attach(|py| {
        assert_eq!(
            result(py, &checker(py, true), &sources),
            result(py, &checker(py, false), &sources)
        );
    });
}

#[test]
fn threshold_9_matches_pylint() {
    threshold_case(9);
}
#[test]
fn threshold_10_matches_pylint() {
    threshold_case(10);
}
#[test]
fn threshold_11_matches_pylint() {
    threshold_case(11);
}
#[test]
fn threshold_25_matches_pylint() {
    threshold_case(25);
}

#[test]
fn global_index_has_no_pairs_for_unrelated_files() {
    let _case = isolated_case();
    let sources: Vec<_> = (0..200)
        .map(|file| {
            let source: String = (0..15)
                .map(|line| format!("value_{file}_{line} = {line}\n"))
                .collect();
            (format!("source_{file}.py"), source)
        })
        .collect();
    Python::attach(|py| {
        let indexed = checker(py, true);
        assert!(result(py, &indexed, &sources).is_empty());
        assert_eq!(
            indexed
                .getattr("candidate_count")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            0
        );
        assert_eq!(
            indexed
                .getattr("indexed_windows")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            1200
        );
    });
}

#[test]
fn invalid_source_and_encoding_are_errors_not_empty_results() {
    let case = isolated_case();
    case.write("syntax.py", "def broken(:\n");
    fs::write(case.root().join("bytes.py"), b"\xff\xff\n").unwrap();
    Python::attach(|py| {
        let value_error = module(py, "builtins").getattr("ValueError").unwrap();
        for (name, message) in [
            ("syntax.py", "cannot normalize syntax.py"),
            ("bytes.py", "cannot normalize bytes.py"),
        ] {
            let error = duplicates(py)
                .getattr("scan_duplicates")
                .unwrap()
                .call1((path(py, case.root()), vec![name]))
                .unwrap_err();
            assert_error(py, error, &value_error, message);
        }
    });
}

#[test]
fn scan_reports_global_counts_and_source_locations() {
    let case = isolated_case();
    for name in ["a.py", "b.py"] {
        case.write(name, &format!("def function():\n{}", body(16)));
    }
    Python::attach(|py| {
        let result = scan(py, &case, &["b.py", "a.py", "a.py"]);
        assert_eq!(
            result.getattr("files").unwrap().extract::<usize>().unwrap(),
            2
        );
        for name in ["possible_pairs", "candidate_pairs"] {
            assert_eq!(result.getattr(name).unwrap().extract::<usize>().unwrap(), 1);
        }
        let findings = result.getattr("findings").unwrap();
        assert_eq!(findings.len().unwrap(), 1);
        let finding = text(&findings.get_item(0).unwrap());
        assert!(finding.contains("a.py:2-17"));
        assert!(finding.contains("b.py:2-17"));
        assert!(
            result
                .getattr("elapsed_seconds")
                .unwrap()
                .extract::<f64>()
                .unwrap()
                >= 0.0
        );
    });
}

fn suppression_case(directive: &str) {
    let case = isolated_case();
    case.write("a.py", &format!("def first():\n{}", body(16)));
    case.write(
        "b.py",
        &format!("# pylint: {directive}\ndef second():\n{}", body(16)),
    );
    Python::attach(|py| {
        let result = scan(py, &case, &["a.py", "b.py"]);
        assert_eq!(result.getattr("findings").unwrap().len().unwrap(), 0);
        assert_eq!(
            result
                .getattr("candidate_pairs")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            0
        );
    });
}

#[test]
fn suppression_disable_duplicate_code() {
    suppression_case("disable=duplicate-code");
}
#[test]
fn suppression_skip_file() {
    suppression_case("skip-file");
}

#[test]
fn scoped_suppression_respects_reenable() {
    let case = isolated_case();
    case.write("a.py", &format!("def first():\n{}", body(16)));
    case.write("b.py", &format!(
        "# pylint: disable=duplicate-code\nignored = True\n# pylint: enable=duplicate-code\ndef second():\n{}",
        body(16)
    ));
    Python::attach(|py| {
        assert_eq!(
            scan(py, &case, &["a.py", "b.py"])
                .getattr("findings")
                .unwrap()
                .len()
                .unwrap(),
            1
        );
    });
}

fn host_config_case(name: &str, config: &str) {
    let case = isolated_case();
    case.write(name, config);
    let source: String = (0..15).map(|i| format!("import module_{i}\n")).collect();
    for name in ["a.py", "b.py"] {
        case.write(name, &source);
    }
    Python::attach(|py| {
        let result = scan(py, &case, &["a.py", "b.py"]);
        assert!(!result
            .getattr("normalization")
            .unwrap()
            .get_item("ignore_imports")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert_eq!(result.getattr("findings").unwrap().len().unwrap(), 1);
    });
}

#[test]
fn host_pylintrc_normalization_configuration_is_preserved() {
    host_config_case(
        ".pylintrc",
        "[SIMILARITIES]\nignore-imports=no\nmin-similarity-lines=100\n",
    );
}

#[test]
fn host_pyproject_normalization_configuration_is_preserved() {
    host_config_case(
        "pyproject.toml",
        "[tool.pylint.similarities]\nignore-imports=false\nmin-similarity-lines=100\n",
    );
}

#[test]
fn all_host_ignore_options_are_resolved() {
    let case = isolated_case();
    case.write(".pylintrc", "[SIMILARITIES]\nignore-comments=no\nignore-docstrings=no\nignore-imports=yes\nignore-signatures=no\n");
    Python::attach(|py| {
        let options = duplicates(py)
            .getattr("_configured_options")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert!(!options
            .getattr("ignore_comments")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(!options
            .getattr("ignore_docstrings")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(options
            .getattr("ignore_imports")
            .unwrap()
            .extract::<bool>()
            .unwrap());
        assert!(!options
            .getattr("ignore_signatures")
            .unwrap()
            .extract::<bool>()
            .unwrap());
    });
}
