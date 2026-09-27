//! Rust-owned source fixture and bounded callback mocks for import ablation.

use crate::comm_support::{bind_signature, signature};
use crate::support::{module, path, AttrPatch, Case};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyList};
use std::fs;
use std::path::{Path, PathBuf};

pub const MODULE: &str = "import os  # noqa: F401 - side effect\nimport sys\nfrom collections import (  # noqa: F401\n    OrderedDict,\n    defaultdict,\n)\n\nVALUE = 1\n";

pub fn ia(py: Python<'_>) -> Bound<'_, pyo3::types::PyModule> {
    module(py, "conductor.import_ablation")
}

pub fn sample(case: &Case) -> PathBuf {
    let source = case.root().join("sample.py");
    fs::write(&source, MODULE).unwrap();
    source
}

pub fn sites<'py>(py: Python<'py>, source: &Path, all: bool) -> Bound<'py, PyList> {
    let importer = ia(py).getattr("import_sites").unwrap();
    let result = if all {
        importer.call1((path(py, source), false)).unwrap()
    } else {
        importer.call1((path(py, source),)).unwrap()
    };
    result.cast_into::<PyList>().unwrap()
}

pub fn collection_site<'py>(py: Python<'py>, source: &Path) -> Bound<'py, PyAny> {
    sites(py, source, false)
        .iter()
        .find(|site| {
            let statement: String = site.getattr("statement").unwrap().extract().unwrap();
            statement.contains("collections")
        })
        .unwrap()
}

pub fn simple_site<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    ia(py)
        .getattr("ImportSite")
        .unwrap()
        .call1(("m.py", 1, "import os  # noqa: F401", ("os",), true))
        .unwrap()
}

pub fn classify_patches(py: Python<'_>, returncode: i32, found: &[&str]) -> Vec<AttrPatch> {
    let ablate_sig = signature(py, &["src", "site"], &[]);
    let ablate =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<String> {
            bind_signature(&ablate_sig, args, kwargs)?;
            Ok(String::new())
        })
        .unwrap();
    let consumers_sig = signature(py, &["m", "n", "r"], &[]);
    let found: Vec<String> = found.iter().map(|value| (*value).to_owned()).collect();
    let consumers =
        PyCFunction::new_closure(py, None, None, move |args, kwargs| -> PyResult<Py<PyAny>> {
            bind_signature(&consumers_sig, args, kwargs)?;
            Ok(PyList::new(args.py(), &found)?.into_any().unbind())
        })
        .unwrap();
    let run = PyCFunction::new_closure(
        py,
        None,
        None,
        move |args, _kwargs| -> PyResult<Py<PyAny>> {
            let process = module(args.py(), "subprocess")
                .getattr("CompletedProcess")?
                .call1((args, returncode, "", ""))?;
            Ok(process.unbind())
        },
    )
    .unwrap();
    vec![
        AttrPatch::replace(ia(py).as_any(), "ablate_source", ablate.as_any()),
        AttrPatch::replace(ia(py).as_any(), "consumers", consumers.as_any()),
        AttrPatch::replace(module(py, "subprocess").as_any(), "run", run.as_any()),
    ]
}
