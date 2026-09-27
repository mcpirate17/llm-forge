#![cfg(feature = "python-compat-tests")]
//! Rust-owned detector scan boundary and independent AST fallback reference.

#[path = "python_contracts/reuse_ast_support.rs"]
#[allow(dead_code)]
mod ast_ref;
#[path = "python_contracts/reuse_detector_support.rs"]
mod detector_ref;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use detector_ref::{detector_scan, native_scan, reference_scan};
use pyo3::prelude::*;
use pyo3::types::PyList;
use std::fs;
use std::path::Path;
use support::{assert_error, Case};

fn allowlist(repo: &Path, allowed: &[&str]) {
    let directory = repo.join("conductor");
    fs::create_dir_all(&directory).unwrap();
    let data = serde_json::json!({"god_functions":allowed,"god_files":[]});
    fs::write(directory.join("guardrail_allowlist.json"), data.to_string()).unwrap();
}

fn long_function(name: &str, marker: bool) -> String {
    let mut body = vec![format!("def {name}():")];
    if marker {
        body.push("    # guardrail: allow-god-function".into());
    }
    body.extend((0..100 - usize::from(marker)).map(|_| "    pass".to_owned()));
    body.join("\n") + "\n"
}

#[test]
fn native_detector_scan_matches_guardrail_boundaries_and_exemptions() {
    let case = Case::new();
    allowlist(case.root(), &["allowed.py::allowed"]);
    let names = [
        "long.py",
        "allowed.py",
        "marked.py",
        "route.py",
        "exact.rs",
        "oversized.rs",
        "exempt.ts",
        "broken.py",
        "missing.py",
    ];
    let paths: Vec<_> = names.iter().map(|name| case.root().join(name)).collect();
    fs::write(&paths[0], long_function("ordinary", false)).unwrap();
    fs::write(&paths[1], long_function("allowed", false)).unwrap();
    fs::write(&paths[2], long_function("marked", true)).unwrap();
    fs::write(
        &paths[3],
        format!(
            "def register_routes():\n    def route():\n        pass\n{}",
            "    pass\n".repeat(98)
        ),
    )
    .unwrap();
    fs::write(&paths[4], "x\n".repeat(1249)).unwrap();
    fs::write(&paths[5], "x\n".repeat(1250)).unwrap();
    fs::write(
        &paths[6],
        format!("// # guardrail: allow-god-file\n{}", "x\n".repeat(1250)),
    )
    .unwrap();
    fs::write(&paths[7], format!("def broken(:\n{}", "#\n".repeat(1250))).unwrap();
    let borrowed: Vec<&Path> = paths.iter().map(|item| item.as_path()).collect();
    Python::attach(|py| {
        let expected = reference_scan(py, &borrowed, case.root());
        let actual = native_scan(py, &borrowed, case.root());
        assert!(actual.eq(&expected).unwrap());
        assert!(expected.get_item(0).unwrap().eq(2).unwrap());
        assert!(expected.get_item(1).unwrap().eq(1).unwrap());
        let measurement: String = expected
            .get_item(3)
            .unwrap()
            .get_item(0)
            .unwrap()
            .extract()
            .unwrap();
        assert!(measurement.contains("ordinary (101)"));
    });
}

#[test]
fn native_detector_scan_matches_exception_handler_semantics() {
    let case = Case::new();
    let source = case.root().join("fallbacks.py");
    fs::write(&source, b"def probe(value):\n    try:\n        return value()\n    except:\n        pass\n    try:\n        return value()\n    except Exception:\n        return None\n    try:\n        return value()\n    except (ValueError, Exception):\n        result = None\n    try:\n        return value()\n    except ValueError:\n        pass\n    try:\n        return value()\n    except Exception:\n        logger.exception('visible')\n    try:\n        return value()\n    except BaseException:\n        raise\n# invalid byte is replaced during decoding: \xff\n").unwrap();
    Python::attach(|py| {
        let expected = reference_scan(py, &[source.as_path()], case.root());
        let actual = native_scan(py, &[source.as_path()], case.root());
        assert!(actual.eq(&expected).unwrap());
        let fallbacks = expected.get_item(4).unwrap().cast_into::<PyList>().unwrap();
        let confidence = PyList::new(
            py,
            fallbacks
                .iter()
                .map(|row| row.get_item("confidence").unwrap()),
        )
        .unwrap();
        let severity = PyList::new(
            py,
            fallbacks
                .iter()
                .map(|row| row.get_item("severity").unwrap()),
        )
        .unwrap();
        assert!(confidence
            .eq(PyList::new(py, [0.98, 0.75, 0.75, 0.65]).unwrap())
            .unwrap());
        assert!(severity
            .eq(PyList::new(py, ["critical", "medium", "medium", "medium"]).unwrap())
            .unwrap());
    });
}

#[test]
fn audit_refuses_to_report_unparsed_file() {
    let case = Case::new();
    allowlist(case.root(), &[]);
    let broken = case.root().join("broken.py");
    fs::write(&broken, "def run(:\n").unwrap();
    Python::attach(|py| {
        let native = native_scan(py, &[broken.as_path()], case.root());
        assert!(native
            .get_item(5)
            .unwrap()
            .eq(PyList::new(py, ["broken.py"]).unwrap())
            .unwrap());
        let error = detector_scan(py, &[broken.as_path()], case.root()).unwrap_err();
        let class = py
            .import("builtins")
            .unwrap()
            .getattr("ValueError")
            .unwrap();
        assert_error(py, error, &class, "cannot parse");
    });
}
