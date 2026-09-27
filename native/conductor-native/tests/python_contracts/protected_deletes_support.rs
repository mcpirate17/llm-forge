//! Python API and output capture for protected deletion contracts.

use crate::support::module;
use pyo3::prelude::*;
use pyo3::types::{PyList, PyModule};

pub fn checker(py: Python<'_>) -> Bound<'_, PyModule> {
    module(py, "conductor.check_protected_deletes")
}

pub fn main(py: Python<'_>, args: &[&str]) -> i64 {
    checker(py)
        .getattr("main")
        .unwrap()
        .call1((PyList::new(py, args).unwrap(),))
        .unwrap()
        .extract()
        .unwrap()
}

pub fn captured_main(py: Python<'_>, args: &[&str]) -> (i64, String, String) {
    let io = module(py, "io");
    let contextlib = module(py, "contextlib");
    let stdout = io.getattr("StringIO").unwrap().call0().unwrap();
    let stderr = io.getattr("StringIO").unwrap().call0().unwrap();
    let out_context = contextlib
        .getattr("redirect_stdout")
        .unwrap()
        .call1((&stdout,))
        .unwrap();
    let err_context = contextlib
        .getattr("redirect_stderr")
        .unwrap()
        .call1((&stderr,))
        .unwrap();
    out_context.call_method0("__enter__").unwrap();
    err_context.call_method0("__enter__").unwrap();
    let result = checker(py)
        .getattr("main")
        .unwrap()
        .call1((PyList::new(py, args).unwrap(),));
    err_context
        .call_method1("__exit__", (py.None(), py.None(), py.None()))
        .unwrap();
    out_context
        .call_method1("__exit__", (py.None(), py.None(), py.None()))
        .unwrap();
    let status = result.unwrap().extract().unwrap();
    let stdout = stdout.call_method0("getvalue").unwrap().extract().unwrap();
    let stderr = stderr.call_method0("getvalue").unwrap().extract().unwrap();
    (status, stdout, stderr)
}
