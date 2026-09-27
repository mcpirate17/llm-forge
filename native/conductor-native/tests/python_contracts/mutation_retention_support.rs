//! Rust-owned synthetic corpus fixtures for retention contracts.

use pyo3::exceptions::PyTypeError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PyDict, PySet};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use super::support::{module, path, AttrPatch, Case};

pub const SUBJECT: &str = "src/conductor/retained_subject.py";
pub const SUBJECT_TEST: &str = "src/conductor/test_retained_subject.py";
pub const EARLY: &str = "2026-09-01T00:00:00+00:00";
pub const LATE: &str = "2026-09-05T00:00:00+00:00";

pub fn retention<'py>(py: Python<'py>) -> Bound<'py, PyAny> {
    module(py, "conductor.mutation_retention").into_any()
}

pub fn json_to_py<'py>(py: Python<'py>, value: &Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((value.to_string(),))
        .unwrap()
}

pub fn py_to_json(value: &Bound<'_, PyAny>) -> Value {
    let py = value.py();
    let text: String = module(py, "json")
        .getattr("dumps")
        .unwrap()
        .call1((value,))
        .unwrap()
        .extract()
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

pub fn write_json(file: &Path, value: &Value) -> PathBuf {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
    file.to_path_buf()
}

fn digest(file: &Path) -> String {
    let hash = Sha256::digest(fs::read(file).unwrap());
    format!("{hash:x}")
}

pub fn fixture(py: Python<'_>, case: &Case) {
    let root = case.root();
    let relative: String = retention(py)
        .getattr("RECEIPT_DIRECTORY")
        .unwrap()
        .extract()
        .unwrap();
    fs::create_dir_all(root.join(relative)).unwrap();
}

pub fn manifest(py: Python<'_>, root: &Path, id: &str, filename: Option<&str>) -> PathBuf {
    let source = root.join(SUBJECT);
    let test = root.join(SUBJECT_TEST);
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "VALUE = 1\n").unwrap();
    fs::write(
        &test,
        "def test_retained_subject() -> None:\n    assert True\n",
    )
    .unwrap();
    let relative: String = retention(py)
        .getattr("CAMPAIGN_DIRECTORY")
        .unwrap()
        .extract()
        .unwrap();
    write_json(
        &root
            .join(relative)
            .join(format!("{}.json", filename.unwrap_or(id))),
        &json!({
            "campaign_id":id,"title":id,"language":"python","mutation_engine":"fest",
            "schema_version":1,
            "generator":{"source":[SUBJECT],"exclude":["**/test_*.py"],"operators":[],"seed":0,"run_timeout_seconds":60},
            "source_sha256":{(SUBJECT):digest(&source)},"test_sha256":{(SUBJECT_TEST):digest(&test)},
            "survivor_baseline":[],"test_argv":["python","-m","pytest",SUBJECT_TEST],"environment":{}
        }),
    )
}

pub fn receipt(
    py: Python<'_>,
    root: &Path,
    name: &str,
    id: &str,
    status: &str,
    at: &str,
    pad: usize,
) -> PathBuf {
    let relative: String = retention(py)
        .getattr("RECEIPT_DIRECTORY")
        .unwrap()
        .extract()
        .unwrap();
    let file = write_json(
        &root.join(relative).join(format!("{name}.json")),
        &json!({
            "campaign_id":id,"status":status,"generated_at":at,"name":name
        }),
    );
    if pad > 0 {
        let mut body = fs::read_to_string(&file).unwrap();
        body.push_str(&" ".repeat(pad));
        fs::write(&file, body).unwrap();
    }
    file
}

pub fn empty_set(py: Python<'_>) -> Py<PyAny> {
    PySet::empty(py).unwrap().into_any().unbind()
}

pub fn no_citations(py: Python<'_>) -> (AttrPatch, AttrPatch) {
    let m = retention(py);
    let cited = PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
        Ok(empty_set(args.py()))
    })
    .unwrap();
    let audited = PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
        Ok(empty_set(args.py()))
    })
    .unwrap();
    (
        AttrPatch::replace(&m, "cited_receipts", cited.as_any()),
        AttrPatch::replace(&m, "audited_receipts", audited.as_any()),
    )
}

pub fn cites(py: Python<'_>, files: &[PathBuf]) -> (AttrPatch, AttrPatch) {
    let m = retention(py);
    let files = files.to_vec();
    let cited = PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
        let py = args.py();
        let set = PySet::empty(py)?;
        for file in &files {
            set.add(path(py, file))?;
        }
        Ok(set.into_any().unbind())
    })
    .unwrap();
    let audited = PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
        Ok(empty_set(args.py()))
    })
    .unwrap();
    (
        AttrPatch::replace(&m, "cited_receipts", cited.as_any()),
        AttrPatch::replace(&m, "audited_receipts", audited.as_any()),
    )
}

pub fn audit_accepts(py: Python<'_>, names: &[&str]) -> AttrPatch {
    let names: Vec<String> = names.iter().map(|value| (*value).to_owned()).collect();
    let judge = module(py, "conductor.mutation_patch_audit")
        .getattr("ReceiptJudge")
        .unwrap();
    let callback =
        PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
            let py = args.py();
            // PyCFunction is stored on the class as an unbound callable. A
            // production method invocation must pass exactly receipt, campaign.
            if args.len() != 2 {
                return Err(PyTypeError::new_err(format!(
                    "ReceiptJudge.rejection double expected receipt, campaign; got {} arguments",
                    args.len()
                )));
            }
            let row = args.get_item(0)?;
            let name: String = row.get_item("name")?.extract()?;
            if names.contains(&name) {
                Ok(py.None())
            } else {
                Ok("rejected by the test"
                    .into_pyobject(py)?
                    .into_any()
                    .unbind())
            }
        })
        .unwrap();
    AttrPatch::replace(&judge, "rejection", callback.as_any())
}

pub fn plan<'py>(py: Python<'py>, root: &Path, protect: &[&str]) -> PyResult<Bound<'py, PyAny>> {
    let kw = PyDict::new(py);
    kw.set_item("protect", protect)?;
    retention(py)
        .getattr("plan")?
        .call((path(py, root),), Some(&kw))
}

pub fn path_set(values: &Bound<'_, PyAny>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for value in values.try_iter().unwrap() {
        let text: String = value.unwrap().str().unwrap().extract().unwrap();
        paths.push(PathBuf::from(text));
    }
    paths.sort();
    paths
}

pub fn capture(py: Python<'_>, stream: &str) -> (AttrPatch, Py<PyAny>) {
    let target = module(py, "sys");
    let buffer = module(py, "io")
        .getattr("StringIO")
        .unwrap()
        .call0()
        .unwrap();
    let patch = AttrPatch::replace(target.as_any(), stream, &buffer);
    (patch, buffer.unbind())
}

pub fn output(py: Python<'_>, buffer: &Py<PyAny>) -> String {
    buffer
        .bind(py)
        .call_method0("getvalue")
        .unwrap()
        .extract()
        .unwrap()
}
