//! Local JSONL and callback fixtures for the memory sidecar Python contracts.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PyModule, PyTuple};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use super::support::{module, path, AttrPatch};

pub fn schema_version() -> u64 {
    Python::attach(|py| {
        module(py, "conductor.memory_index")
            .getattr("SCHEMA_VERSION")
            .unwrap()
            .extract()
            .unwrap()
    })
}

pub fn py_json<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    PyModule::import(py, "json")
        .unwrap()
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn json_value(value: &Bound<'_, PyAny>) -> Value {
    let encoded: String = PyModule::import(value.py(), "json")
        .unwrap()
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&encoded).unwrap()
}

pub fn kwargs<'py>(py: Python<'py>, pairs: &[(&str, Bound<'py, PyAny>)]) -> Bound<'py, PyDict> {
    let out = PyDict::new(py);
    for (key, value) in pairs {
        out.set_item(key, value).unwrap();
    }
    out
}

pub fn fixed_embed<'py>(
    py: Python<'py>,
    vector: Vec<f64>,
    calls: Option<Arc<Mutex<Vec<String>>>>,
) -> Bound<'py, PyCFunction> {
    PyCFunction::new_closure(
        py,
        None,
        None,
        move |args: &Bound<'_, PyTuple>, kw: Option<&Bound<'_, PyDict>>| -> PyResult<Vec<f64>> {
            let parameter = if calls.is_some() { "text" } else { "_text" };
            let argument = single_argument(args, kw, parameter, true)?.unwrap();
            if let Some(calls) = &calls {
                calls.lock().unwrap().push(argument.extract::<String>()?);
            }
            Ok(vector.clone())
        },
    )
    .unwrap()
}

fn single_argument<'py>(
    args: &Bound<'py, PyTuple>,
    kw: Option<&Bound<'py, PyDict>>,
    parameter: &str,
    required: bool,
) -> PyResult<Option<Bound<'py, PyAny>>> {
    let keyword = kw.map(|kw| kw.get_item(parameter)).transpose()?.flatten();
    if args.len() > 1
        || kw.is_some_and(|kw| kw.len() != usize::from(keyword.is_some()))
        || (!args.is_empty() && keyword.is_some())
    {
        return Err(PyTypeError::new_err(format!("expected only {parameter}")));
    }
    let value = if args.is_empty() {
        keyword
    } else {
        Some(args.get_item(0)?)
    };
    if value.is_none() && required {
        return Err(PyTypeError::new_err(format!("missing {parameter}")));
    }
    Ok(value)
}

pub fn fixed_rows<'py>(py: Python<'py>, rows: &Value) -> Bound<'py, PyCFunction> {
    let rows = py_json(py, rows).unbind();
    PyCFunction::new_closure(
        py,
        None,
        None,
        move |args: &Bound<'_, PyTuple>, kw: Option<&Bound<'_, PyDict>>| -> PyResult<Py<PyAny>> {
            single_argument(args, kw, "p", false)?;
            Ok(rows.clone_ref(args.py()))
        },
    )
    .unwrap()
}

pub fn capture_stderr(py: Python<'_>) -> (Bound<'_, PyAny>, AttrPatch) {
    let buffer = PyModule::import(py, "io")
        .unwrap()
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let patch = AttrPatch::replace(
        PyModule::import(py, "sys").unwrap().as_any(),
        "stderr",
        &buffer,
    );
    (buffer, patch)
}

pub fn query<'py>(
    py: Python<'py>,
    index: &Bound<'py, PyModule>,
    query_text: &str,
    file: &Path,
    top_k: Option<usize>,
    embed: &Bound<'py, PyCFunction>,
    scan: bool,
) -> PyResult<Bound<'py, PyAny>> {
    let mut pairs = vec![("embedder", embed.as_any().clone())];
    if let Some(top_k) = top_k {
        pairs.push(("top_k", top_k.into_pyobject(py).unwrap().into_any()));
    }
    let name = if scan {
        "_query_index_file_scan"
    } else {
        "query_index_file"
    };
    index
        .getattr(name)?
        .call((query_text, path(py, file)), Some(&kwargs(py, &pairs)))
}

pub fn write_rows(file: &Path, rows: &[Value]) {
    let mut body = rows
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    body.push('\n');
    fs::write(file, body).unwrap();
}

fn lcg(seed: u64, step: u64) -> f64 {
    let state = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407 + step.wrapping_mul(7))
        .rotate_left(13);
    (state >> 40) as f64 / (1u64 << 24) as f64 * 2.0 - 1.0
}

pub fn sidecar_rows(count: usize, dimension: usize, schema_version: u64) -> Vec<Value> {
    (0..count)
        .map(|index| {
            let vector: Vec<f64> = (0..dimension)
                .map(|step| (lcg(index as u64, step as u64) * 1e9).round() / 1e9)
                .collect();
            json!({
                "schema_version": schema_version,
                "embedding": {
                    "fingerprint": "sha256:sidecar-parity",
                    "dimension": dimension,
                    "paid": true,
                },
                "source_sha256": format!("{index:064x}"),
                "source": "notes",
                "path": format!("row-{index:04}.md"),
                "title": format!("row {index}"),
                "text": format!("chunk {index}"),
                "vector": vector,
            })
        })
        .collect()
}
