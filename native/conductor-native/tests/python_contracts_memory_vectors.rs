#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the mmap vector sidecar and recency search.

#[path = "python_contracts/memory_streaming_support.rs"]
#[allow(dead_code)]
mod memory_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use memory_support::{fixed_rows, json_value, kwargs};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyModule, PySlice};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use support::{assert_error, module, path, text, AttrPatch, Case};

fn row(file: &Path, vector: &[f64], title: &str) -> Value {
    json!({"source":"notes","path":file.display().to_string(),"title":title,
        "text":"body","vector":vector})
}

fn fixture(py: Python<'_>, case: &Case, vectors: &Bound<'_, PyModule>) -> (PathBuf, AttrPatch) {
    let old = case.write("old.md", "old");
    let new = case.write("new.md", "new");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let os = PyModule::import(py, "os").unwrap();
    for (file, age_days) in [(&old, 200.0), (&new, 7.0)] {
        let stamp = now - age_days * 86_400.0;
        os.getattr("utime")
            .unwrap()
            .call1((path(py, file), (stamp, stamp)))
            .unwrap();
    }
    let rows = json!([
        row(&old, &[1.0, 0.0, 0.0], "old-exact"),
        row(&new, &[0.95, 0.31, 0.0], "new-close"),
        row(&old, &[1.0, 0.001, 0.0], "old-duplicate"),
        row(&case.root().join("gone.md"), &[0.0, 1.0, 0.0], "orthogonal"),
    ]);
    let file = case.write(
        "memory_index.jsonl",
        &format!(
            "{}\n",
            rows.as_array()
                .unwrap()
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        ),
    );
    let mock = fixed_rows(py, &rows);
    let index = vectors.getattr("memory_index").unwrap();
    let patch = AttrPatch::replace(&index, "load_index", &mock);
    (file, patch)
}

fn load_sidecar<'py>(
    py: Python<'py>,
    vectors: &Bound<'py, PyModule>,
    file: &Path,
) -> (Bound<'py, PyAny>, Bound<'py, PyAny>) {
    let result = vectors
        .getattr("load_sidecar")
        .unwrap()
        .call1((path(py, file),))
        .unwrap();
    (result.get_item(0).unwrap(), result.get_item(1).unwrap())
}

fn sidecar_paths(py: Python<'_>, vectors: &Bound<'_, PyModule>, file: &Path) -> (PathBuf, PathBuf) {
    let paths = vectors
        .getattr("sidecar_paths")
        .unwrap()
        .call1((path(py, file),))
        .unwrap();
    let first: String = paths.get_item(0).unwrap().str().unwrap().extract().unwrap();
    let second: String = paths.get_item(1).unwrap().str().unwrap().extract().unwrap();
    (PathBuf::from(first), PathBuf::from(second))
}

fn titles(hits: &Value) -> Vec<&str> {
    hits.as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["title"].as_str().unwrap())
        .collect()
}

#[test]
fn sidecar_builds_once_and_reloads_from_mmap() {
    let case = Case::new();
    Python::attach(|py| {
        let vectors = module(py, "conductor.memory_vectors");
        let (file, _patch) = fixture(py, &case, &vectors);
        let (rows, matrix) = load_sidecar(py, &vectors, &file);
        let (vectors_file, meta_file) = sidecar_paths(py, &vectors, &file);
        assert!(vectors_file.is_file() && meta_file.is_file());
        assert_eq!(
            matrix
                .getattr("shape")
                .unwrap()
                .extract::<(usize, usize)>()
                .unwrap(),
            (4, 3)
        );
        assert_eq!(text(&matrix.getattr("dtype").unwrap()), "float32");
        assert_eq!(json_value(&rows)[0]["title"], "old-exact");
        assert!(json_value(&rows)[0].get("vector").is_none());
        let stamp = fs::metadata(&meta_file).unwrap().modified().unwrap();
        let (second_rows, second_matrix) = load_sidecar(py, &vectors, &file);
        assert_eq!(fs::metadata(&meta_file).unwrap().modified().unwrap(), stamp);
        let numpy = PyModule::import(py, "numpy").unwrap();
        assert!(second_matrix
            .is_instance(&numpy.getattr("memmap").unwrap())
            .unwrap());
        assert_eq!(second_rows.len().unwrap(), 4);
    });
}

#[test]
fn sidecar_rebuilds_when_index_changes_and_metadata_is_corrupt() {
    let case = Case::new();
    Python::attach(|py| {
        let vectors = module(py, "conductor.memory_vectors");
        let (file, _patch) = fixture(py, &case, &vectors);
        load_sidecar(py, &vectors, &file);
        let (_, meta_file) = sidecar_paths(py, &vectors, &file);
        let before = fs::read_to_string(&meta_file).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut contents = fs::read_to_string(&file).unwrap();
        contents.push('\n');
        fs::write(&file, contents).unwrap();
        load_sidecar(py, &vectors, &file);
        assert_ne!(fs::read_to_string(&meta_file).unwrap(), before);
        fs::write(&meta_file, "{broken").unwrap();
        let (rows, _) = load_sidecar(py, &vectors, &file);
        assert_eq!(rows.len().unwrap(), 4);
    });
}

#[test]
fn search_applies_recency_and_dedup() {
    let case = Case::new();
    Python::attach(|py| {
        let vectors = module(py, "conductor.memory_vectors");
        let (file, _patch) = fixture(py, &case, &vectors);
        let (rows, matrix) = load_sidecar(py, &vectors, &file);
        let search = vectors.getattr("search").unwrap();
        let query = vec![1.0, 0.0, 0.0];
        let hits = json_value(
            &search
                .call(
                    (query.clone(), &rows, &matrix),
                    Some(&kwargs(
                        py,
                        &[("top_k", 3i32.into_pyobject(py).unwrap().into_any())],
                    )),
                )
                .unwrap(),
        );
        assert_eq!(titles(&hits), ["new-close", "old-exact", "orthogonal"]);
        assert_eq!(hits[1]["score"], json!(1.0));
        assert!(
            hits[0]["weighted_score"].as_f64().unwrap()
                > hits[1]["weighted_score"].as_f64().unwrap()
        );
        let plain = json_value(
            &search
                .call(
                    (query, &rows, &matrix),
                    Some(&kwargs(
                        py,
                        &[
                            ("top_k", 3i32.into_pyobject(py).unwrap().into_any()),
                            ("boost", 0.0f64.into_pyobject(py).unwrap().into_any()),
                            (
                                "dedup_cosine",
                                1.01f64.into_pyobject(py).unwrap().into_any(),
                            ),
                        ],
                    )),
                )
                .unwrap(),
        );
        assert_eq!(titles(&plain), ["old-exact", "old-duplicate", "new-close"]);
    });
}

#[test]
fn search_validates_top_k_dimension_and_row_count() {
    let case = Case::new();
    Python::attach(|py| {
        let vectors = module(py, "conductor.memory_vectors");
        let (file, _patch) = fixture(py, &case, &vectors);
        let (rows, matrix) = load_sidecar(py, &vectors, &file);
        let search = vectors.getattr("search").unwrap();
        let value_error = PyModule::import(py, "builtins")
            .unwrap()
            .getattr("ValueError")
            .unwrap();
        let zero = kwargs(py, &[("top_k", 0i32.into_pyobject(py).unwrap().into_any())]);
        assert_error(
            py,
            search
                .call((vec![1.0, 0.0, 0.0], &rows, &matrix), Some(&zero))
                .unwrap_err(),
            &value_error,
            "top_k",
        );
        assert_error(
            py,
            search.call1((vec![1.0, 0.0], &rows, &matrix)).unwrap_err(),
            &value_error,
            "dimension",
        );
        let one_row = rows.get_item(PySlice::new(py, 0, 1, 1)).unwrap();
        assert_error(
            py,
            search
                .call1((vec![1.0, 0.0, 0.0], &one_row, &matrix))
                .unwrap_err(),
            &value_error,
            "mismatch",
        );
    });
}

#[test]
fn recency_weights_bounds_and_missing_file() {
    let case = Case::new();
    Python::attach(|py| {
        let vectors = module(py, "conductor.memory_vectors");
        let (file, _patch) = fixture(py, &case, &vectors);
        let (rows, _) = load_sidecar(py, &vectors, &file);
        let weights: Vec<f64> = vectors
            .getattr("recency_weights")
            .unwrap()
            .call(
                (rows,),
                Some(&kwargs(
                    py,
                    &[
                        ("boost", 0.5f64.into_pyobject(py).unwrap().into_any()),
                        (
                            "half_life_days",
                            30i32.into_pyobject(py).unwrap().into_any(),
                        ),
                    ],
                )),
            )
            .unwrap()
            .call_method0("tolist")
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(weights.len(), 4);
        assert_eq!(weights[3], 1.0);
        assert!(1.0 < weights[0] && weights[0] < weights[1] && weights[1] <= 1.5);
    });
}
