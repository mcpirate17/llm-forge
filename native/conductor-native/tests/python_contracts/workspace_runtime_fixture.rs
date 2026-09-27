//! Reuse the Python hook repository fixture while Rust owns the assertions.

use pyo3::prelude::*;
use pyo3::types::PyAny;
use std::path::Path;

use super::support::{module, path};

pub struct HookRepo {
    root: Py<PyAny>,
    monkeypatch: Py<PyAny>,
}

impl HookRepo {
    pub fn new(py: Python<'_>, temp_root: &Path) -> Self {
        let pytest = module(py, "pytest");
        let monkeypatch = pytest.getattr("MonkeyPatch").unwrap().call0().unwrap();
        let fixture = module(py, "conductor.conftest")
            .getattr("hook_repo")
            .unwrap()
            .getattr("__wrapped__")
            .unwrap();
        let root = fixture.call1((path(py, temp_root), &monkeypatch)).unwrap();
        Self {
            root: root.unbind(),
            monkeypatch: monkeypatch.unbind(),
        }
    }

    pub fn root<'py>(&self, py: Python<'py>) -> Bound<'py, PyAny> {
        self.root.bind(py).clone()
    }
}

impl Drop for HookRepo {
    fn drop(&mut self) {
        Python::attach(|py| {
            self.monkeypatch.bind(py).call_method0("undo").unwrap();
        });
    }
}
