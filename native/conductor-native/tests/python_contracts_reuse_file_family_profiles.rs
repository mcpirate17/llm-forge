#![cfg(feature = "python-compat-tests")]
//! Rust-owned independent profile feature reference and batch contracts.

#[path = "python_contracts/reuse_ast_support.rs"]
#[allow(dead_code)]
mod ast_ref;
#[path = "python_contracts/reuse_profile_support.rs"]
mod profile_ref;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use profile_ref::reference_profile;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::fs;
use std::path::Path;
use support::{module, path, Case};

const SOURCES: [&str; 3] = [
    r#"from alpha.beta import item
import gamma.delta as gd

class Lane:
    @decorator(flag=True, mode="fast")
    async def run(self, value: int = 1, *, scale=2, **options) -> None:
        payload = {"alpha": value, "beta": scale, "gamma": options}
        self.state = factory(value, scale=scale, mode="fast")
        async with manager() as handle:
            if payload:
                return None

        def nested(local):
            return local + 1

    def build(self) -> Product:
        from factory import Product

        return Product()
"#,
    r#"def classify(value):
    match value:
        case {"kind": kind}:
            return kind
        case _:
            return "unknown"

try:
    RESULT = classify({"alpha": 1, "beta": True, "gamma": None})
except (TypeError, ValueError) as error:
    RESULT = str(error)
"#,
    "class UnicodeLane:\n    def résumé(self, entrée=\"café\"):\n        \"\"\"A docstring excluded from the normalized method hash.\"\"\"\n        sortie = entrée.strip()\n        return sortie\n",
];

fn production_profile<'py>(
    py: Python<'py>,
    source: &Path,
    repo: &Path,
) -> Bound<'py, pyo3::types::PyAny> {
    module(py, "conductor.reuse.file_families")
        .getattr("profile_file")
        .unwrap()
        .call1((path(py, source), path(py, repo)))
        .unwrap()
}

#[test]
fn native_profiles_match_cpython_reference_for_all_feature_fields() {
    let case = Case::new();
    Python::attach(|py| {
        for (index, source) in SOURCES.iter().enumerate() {
            let file = case.root().join(format!("case_{index}.py"));
            fs::write(&file, source).unwrap();
            let expected = reference_profile(py, &file, case.root()).unwrap();
            let actual = production_profile(py, &file, case.root());
            assert!(actual.eq(expected).unwrap(), "source index {index}");
        }
    });
}

#[test]
fn native_batch_preserves_order_and_counts_syntax_errors() {
    let case = Case::new();
    let first = case.root().join("first.py");
    let broken = case.root().join("broken.py");
    let second = case.root().join("second.py");
    fs::write(&first, SOURCES[0]).unwrap();
    fs::write(&broken, "def broken(:\n").unwrap();
    fs::write(&second, SOURCES[1]).unwrap();
    Python::attach(|py| {
        let core = module(py, "conductor.reuse").getattr("core").unwrap();
        let options = PyDict::new(py);
        options
            .set_item(
                "paths",
                [first.as_path(), broken.as_path(), second.as_path()]
                    .iter()
                    .map(|value| value.to_string_lossy().to_string())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        options
            .set_item("repo", case.root().to_string_lossy().to_string())
            .unwrap();
        let result = core
            .getattr("audit_file_family_profiles")
            .unwrap()
            .call((), Some(&options))
            .unwrap();
        let rows = result.get_item(0).unwrap().cast_into::<PyList>().unwrap();
        let names = PyList::new(py, rows.iter().map(|row| row.get_item("file").unwrap())).unwrap();
        assert!(names
            .eq(PyList::new(py, ["first.py", "second.py"]).unwrap())
            .unwrap());
        assert!(result.get_item(1).unwrap().eq(1).unwrap());
    });
}

#[test]
fn native_profiles_preserve_invalid_utf8_replacement_semantics() {
    let case = Case::new();
    let file = case.root().join("replacement.py");
    fs::write(&file, b"VALUE = 'ok'\n# invalid: \xff\n").unwrap();
    Python::attach(|py| {
        let expected = reference_profile(py, &file, case.root()).unwrap();
        let actual = production_profile(py, &file, case.root());
        assert!(actual.eq(expected).unwrap());
    });
}
