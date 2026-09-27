//! Rust fixtures for the Python in-place handoff boundary.

use crate::support::{module, path};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use std::path::Path;

pub fn now<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    let datetime = module(py, "datetime");
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("tzinfo", datetime.getattr("UTC").unwrap())
        .unwrap();
    datetime
        .getattr("datetime")
        .unwrap()
        .call((2026, 9, 1), Some(&kwargs))
        .unwrap()
}

pub fn state<'py>(py: Python<'py>, heading: &str) -> Bound<'py, PyAny> {
    let datetime = module(py, "datetime");
    let expiry = now(py)
        .call_method1(
            "__add__",
            (datetime
                .getattr("timedelta")
                .unwrap()
                .call1((0, 7200))
                .unwrap(),),
        )
        .unwrap();
    let claim = PyDict::new(py);
    claim.set_item("claim_id", "claim-owned").unwrap();
    claim.set_item("owner", "codex").unwrap();
    claim
        .set_item("paths", ["conductor/inplace_handoff.py"])
        .unwrap();
    claim.set_item("justification", "handoff test").unwrap();
    claim
        .set_item("expires_at", expiry.call_method0("isoformat").unwrap())
        .unwrap();
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("last_updated", now(py).call_method0("isoformat").unwrap())
        .unwrap();
    kwargs.set_item("active_headings", [heading]).unwrap();
    kwargs.set_item("active_claims", [claim]).unwrap();
    module(py, "conductor.active_state")
        .getattr("ActiveState")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn prepare<'py>(py: Python<'py>, handoff: &Bound<'py, PyModule>) -> Bound<'py, PyAny> {
    let runtime = PyDict::new(py);
    runtime.set_item("adapter", "sidecar").unwrap();
    runtime.set_item("supports_inplace_replace", false).unwrap();
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("task", "continue the controlled task")
        .unwrap();
    kwargs
        .set_item("paths", ("conductor/inplace_handoff.py",))
        .unwrap();
    kwargs.set_item("runtime", runtime).unwrap();
    kwargs.set_item("context", "verified notes").unwrap();
    kwargs
        .set_item("state", state(py, "controlled task"))
        .unwrap();
    kwargs.set_item("now", now(py)).unwrap();
    handoff
        .getattr("prepare_handoff")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

pub fn stage<'py>(
    py: Python<'py>,
    handoff: &Bound<'py, PyModule>,
    root: &Path,
    task: &str,
    context: &str,
) -> Bound<'py, PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("identity", "fable-5").unwrap();
    kwargs.set_item("task", task).unwrap();
    kwargs.set_item("context", context).unwrap();
    kwargs.set_item("root", path(py, root)).unwrap();
    kwargs
        .set_item("state", state(py, "controlled task"))
        .unwrap();
    kwargs.set_item("now", now(py)).unwrap();
    handoff
        .getattr("stage_handoff")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}
