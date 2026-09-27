#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the reuse repository scan and ROI snapshot.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList, PySet};
use serde_json::json;
use std::fs;
use std::path::Path;
use support::{module, path, Case};

#[pyclass]
struct FakeIndex;

#[pymethods]
impl FakeIndex {
    fn status(&self, py: Python<'_>, targets: &Bound<'_, PyAny>) -> PyResult<Py<PyAny>> {
        let expected = PyList::new(py, ["pkg"])?;
        assert!(targets.eq(expected)?);
        Ok(module(py, "conductor.reuse.graph_index")
            .getattr("GraphStatus")?
            .call1((true, true, "ok"))?
            .unbind())
    }

    fn symbols(&self, py: Python<'_>) -> PyResult<Py<PyList>> {
        Ok(PyList::empty(py).unbind())
    }

    fn edge_count(&self, kind: &Bound<'_, PyAny>) -> PyResult<i32> {
        assert!(kind.eq("IMPORTS_FROM")?);
        Ok(0)
    }
}

fn write(root: &Path, relative: &str, contents: &str) {
    let file = root.join(relative);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, contents).unwrap();
}

#[test]
fn native_repository_scan_preserves_filter_loc_and_order() {
    let case = Case::new();
    let repo = case.root().join("skip/repo");
    write(
        &repo,
        "pkg/module.py",
        "# header\nvalue = 1\n// generated note\nvalue += 1\n",
    );
    write(&repo, "pkg/UPPER.RS", "/* generated */\nfn main() {}\n");
    write(
        &repo,
        "pkg/nested/test_helper.rs",
        "* generated\nfn check() {}\n",
    );
    write(
        &repo,
        "pkg/tests/test_module.py",
        "-- generated\ndef test_value():\n    assert True\n",
    );
    write(&repo, "pkg/skip/drop.ts", "drop();\n");
    write(&repo, "pkg/notes.txt", "not source\n");
    Python::attach(|py| {
        let scan = module(py, "conductor.reuse")
            .getattr("core")
            .unwrap()
            .getattr("audit_repository_scan")
            .unwrap();
        let actual = scan
            .call1((
                repo.to_str().unwrap(),
                PyList::new(py, ["pkg", "pkg/nested"]).unwrap(),
                PyList::new(py, [".py", ".rs", ".ts"]).unwrap(),
                PyList::new(py, ["skip"]).unwrap(),
                PyList::new(py, ["#", "//", "/*", "*", "--"]).unwrap(),
            ))
            .unwrap();
        let expected = PyList::new(
            py,
            [
                ("pkg/UPPER.RS", 1),
                ("pkg/module.py", 2),
                ("pkg/nested/test_helper.rs", 1),
                ("pkg/tests/test_module.py", 2),
            ],
        )
        .unwrap();
        assert!(actual.eq(expected).unwrap());
    });
}

#[test]
fn roi_snapshot_passes_excludes_to_native_scan_and_keeps_policy() {
    let case = Case::new();
    write(case.root(), "pkg/keep.py", "value = 1\n");
    write(case.root(), "pkg/tests/test_keep.py", "assert True\n");
    write(case.root(), "pkg/skip/drop.py", "value = 2\n");
    Python::attach(|py| {
        let index = Py::new(py, FakeIndex).unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("duplicate_lines", 7).unwrap();
        kwargs.set_item("native_candidates", 3).unwrap();
        kwargs.set_item("contract_report", PyDict::new(py)).unwrap();
        kwargs
            .set_item("incomplete_sources", PyList::empty(py))
            .unwrap();
        let actual = module(py, "conductor.reuse.roi")
            .getattr("snapshot")
            .unwrap()
            .call(
                (
                    path(py, case.root()),
                    PyList::new(py, ["pkg"]).unwrap(),
                    PySet::new(py, ["skip"]).unwrap(),
                    index,
                ),
                Some(&kwargs),
            )
            .unwrap();
        let expected = py_json(
            py,
            json!({
                "schema_version": 2,
                "snapshot_hash": "3ebd1d159f53fd568fa9df9aca06d6f5901b7aa028765402deae6d734f1f9ab5",
                "production_loc": 1,
                "test_loc": 1,
                "production_files": 1,
                "test_files": 1,
                "modules": 2,
                "symbols": 0,
                "dependency_edges": 0,
                "duplicate_loc": 7,
                "native_reuse_candidates": 3,
                "contract_violations": 0,
                "unclassified_test_leaves": 0,
                "evidence_complete": true,
                "incomplete_reasons": [],
                "completion_status": "audit_exhausted_for_snapshot",
                "graph": {
                    "available": true,
                    "complete": true,
                    "reason": "ok",
                    "graph_head": "",
                    "repo_head": "",
                    "overlay_hash": "",
                    "schema_version": 0
                }
            }),
        );
        assert!(actual.eq(expected).unwrap());
    });
}
