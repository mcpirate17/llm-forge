#![cfg(feature = "python-compat-tests")]
//! Rust-owned streaming sidecar contracts for the Python memory index API.

#[path = "python_contracts/memory_streaming_support.rs"]
#[allow(dead_code)]
mod memory_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use memory_support::{
    capture_stderr, fixed_embed, json_value, query, schema_version, sidecar_rows, write_rows,
};
use pyo3::prelude::*;
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::Write;
use support::{assert_error, module, Case};

fn stderr_text(buffer: &Bound<'_, pyo3::types::PyAny>) -> String {
    buffer.call_method0("getvalue").unwrap().extract().unwrap()
}

fn clear_stderr(buffer: &Bound<'_, pyo3::types::PyAny>) {
    buffer.call_method1("truncate", (0,)).unwrap();
    buffer.call_method1("seek", (0,)).unwrap();
}

fn paths(hits: &Value) -> Vec<&str> {
    hits.as_array()
        .unwrap()
        .iter()
        .map(|hit| hit["path"].as_str().unwrap())
        .collect()
}

#[test]
fn query_index_file_streams_with_stable_exact_contract() {
    let case = Case::new();
    let file = case.root().join("memory.jsonl");
    let metadata = json!({"fingerprint":"sha256:test-space","dimension":2,"paid":true});
    let schema = schema_version();
    let rows = vec![
        json!({"schema_version":schema,"embedding":metadata,"source_sha256":"a".repeat(64),
            "source":"notes","path":"first.md","title":"first","text":"λ".repeat(501),"vector":[1.0,0.0]}),
        json!({"schema_version":schema,"embedding":metadata,"source_sha256":"b".repeat(64),
            "source":"cards","path":"second.md","title":"second","text":"second","vector":[1.0,0.0]}),
        json!({"schema_version":schema,"embedding":metadata,"source_sha256":"c".repeat(64),
            "source":"notes","path":"low.md","title":"low","text":"low","vector":[0.0,1.0]}),
    ];
    write_rows(&file, &rows);
    Python::attach(|py| {
        let index = module(py, "conductor.memory_index");
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let embed = fixed_embed(py, vec![1.0, 0.0], Some(std::sync::Arc::clone(&calls)));
        let hits = json_value(
            &query(py, &index, "  stable ties  ", &file, Some(2), &embed, false).unwrap(),
        );
        let prefix: String = index.getattr("QUERY_INSTRUCT").unwrap().extract().unwrap();
        assert_eq!(*calls.lock().unwrap(), vec![format!("{prefix}stable ties")]);
        assert_eq!(paths(&hits), ["first.md", "second.md"]);
        assert_eq!(hits[0]["score"], json!(1.0));
        assert_eq!(hits[1]["score"], json!(1.0));
        assert_eq!(hits[0]["text"], json!("λ".repeat(500)));
        let fields = hits[0]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            fields,
            ["path", "score", "source", "text", "title"]
                .into_iter()
                .collect()
        );

        let wrong_dim = fixed_embed(py, vec![1.0], None);
        assert_error(
            py,
            query(py, &index, "q", &file, None, &wrong_dim, false).unwrap_err(),
            &index.getattr("RetrieveError").unwrap(),
            "query dimension 1 !=",
        );
    });
}

#[test]
fn first_query_builds_the_sidecar_and_second_query_reuses_it() {
    let case = Case::new();
    let file = case.root().join("memory.jsonl");
    write_rows(&file, &sidecar_rows(3, 2, schema_version()));
    Python::attach(|py| {
        let index = module(py, "conductor.memory_index");
        let embed = fixed_embed(py, vec![1.0, 0.0], None);
        let (stderr, _patch) = capture_stderr(py);
        let first = json_value(&query(py, &index, "q", &file, Some(2), &embed, false).unwrap());
        assert_eq!(
            stderr_text(&stderr)
                .matches("memory_index: building sidecar (3 rows)")
                .count(),
            1
        );
        assert!(case.root().join("memory.jsonl.sidecar").is_file());
        clear_stderr(&stderr);
        let second = json_value(&query(py, &index, "q", &file, Some(2), &embed, false).unwrap());
        assert_eq!(stderr_text(&stderr), "");
        assert_eq!(first, second);
    });
}

#[test]
fn appending_a_row_rebuilds_the_sidecar_and_ranks_it() {
    let case = Case::new();
    let file = case.root().join("memory.jsonl");
    let mut rows = sidecar_rows(3, 2, schema_version());
    for row in &mut rows {
        row["vector"] = json!([0.0, 1.0]);
    }
    write_rows(&file, &rows);
    Python::attach(|py| {
        let index = module(py, "conductor.memory_index");
        let embed = fixed_embed(py, vec![1.0, 0.0], None);
        let (stderr, _patch) = capture_stderr(py);
        let first = json_value(&query(py, &index, "q", &file, Some(1), &embed, false).unwrap());
        assert_eq!(stderr_text(&stderr).matches("building sidecar").count(), 1);
        assert_eq!(first[0]["score"], json!(0.0));

        let mut appended = sidecar_rows(1, 2, schema_version()).remove(0);
        appended["vector"] = json!([1.0, 0.0]);
        writeln!(
            OpenOptions::new().append(true).open(&file).unwrap(),
            "{appended}"
        )
        .unwrap();
        clear_stderr(&stderr);
        let second = json_value(&query(py, &index, "q", &file, Some(1), &embed, false).unwrap());
        assert_eq!(
            stderr_text(&stderr)
                .matches("building sidecar (4 rows)")
                .count(),
            1
        );
        assert_eq!(second[0]["path"], "row-0000.md");
        assert_eq!(second[0]["score"], json!(1.0));
    });
}

#[test]
fn sidecar_query_matches_the_full_scan_reference() {
    let case = Case::new();
    let file = case.root().join("memory.jsonl");
    let mut rows = sidecar_rows(120, 4, schema_version());
    write_rows(&file, &rows);
    Python::attach(|py| {
        let index = module(py, "conductor.memory_index");
        let embed = fixed_embed(py, vec![0.31, -0.7, 0.11, 0.9], None);
        let first_ref =
            json_value(&query(py, &index, "parity", &file, Some(10), &embed, true).unwrap());
        let winner = first_ref[0]["path"].as_str().unwrap();
        rows.iter_mut().find(|row| row["path"] == winner).unwrap()["text"] = json!("λ".repeat(600));
        write_rows(&file, &rows);
        let sidecar =
            json_value(&query(py, &index, "parity", &file, Some(10), &embed, false).unwrap());
        let reference =
            json_value(&query(py, &index, "parity", &file, Some(10), &embed, true).unwrap());
        assert_eq!(paths(&sidecar), paths(&reference));
        for (actual, expected) in sidecar
            .as_array()
            .unwrap()
            .iter()
            .zip(reference.as_array().unwrap())
        {
            let delta =
                (actual["score"].as_f64().unwrap() - expected["score"].as_f64().unwrap()).abs();
            assert!(delta < 1e-5, "sidecar {actual} vs scan {expected}");
        }
        assert_eq!(sidecar[0]["text"], json!("λ".repeat(500)));
    });
}
