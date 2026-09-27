#![cfg(feature = "python-compat-tests")]
//! Rust-controlled differential fixtures for native memory chunking.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyModule};
use serde_json::{json, Value};
use support::{module, path, Case};

fn json_value(value: &Bound<'_, PyAny>) -> Value {
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

fn kwargs<'py>(py: Python<'py>, pairs: &[(&str, Bound<'py, PyAny>)]) -> Bound<'py, PyDict> {
    let out = PyDict::new(py);
    for (key, value) in pairs {
        out.set_item(key, value).unwrap();
    }
    out
}

fn py_strip(py: Python<'_>, value: &str) -> String {
    value
        .into_pyobject(py)
        .unwrap()
        .call_method0("strip")
        .unwrap()
        .extract()
        .unwrap()
}

fn py_prefix(py: Python<'_>, value: &str, max: usize) -> String {
    value
        .into_pyobject(py)
        .unwrap()
        .get_item(pyo3::types::PySlice::new(py, 0, max as isize, 1))
        .unwrap()
        .extract()
        .unwrap()
}

fn reference_chunks(py: Python<'_>, raw: &str, mode: &str) -> Value {
    let input = py_strip(py, raw);
    if input.is_empty() {
        return json!([]);
    }
    if mode == "whole" {
        return json!([{"source":"s","path":"docs/n.md","title":"n.md","text":py_prefix(py,&input,3000)}]);
    }
    let lines: Vec<String> = input
        .as_str()
        .into_pyobject(py)
        .unwrap()
        .call_method0("splitlines")
        .unwrap()
        .extract()
        .unwrap();
    let mut chunks = Vec::<Value>::new();
    let mut buf = Vec::<String>::new();
    let mut title = "n.md".to_owned();
    let mut size = 0usize;
    let flush = |chunks: &mut Vec<Value>, buf: &mut Vec<String>, size: &mut usize, title: &str| {
        let body = py_strip(py, &buf.join("\n"));
        if !body.is_empty() {
            chunks.push(json!({"source":"s","path":"docs/n.md","title":title,"text":py_prefix(py,&body,3000)}));
        }
        buf.clear();
        *size = 0;
    };
    for line in lines {
        if ["# ", "## ", "### ", "#### "]
            .iter()
            .any(|p| line.starts_with(p))
            && size >= 400
        {
            flush(&mut chunks, &mut buf, &mut size, &title);
            let stripped = py_strip(py, line.trim_start_matches('#'));
            title = if stripped.is_empty() {
                "n.md".to_owned()
            } else {
                stripped
            };
        }
        size += line.chars().count() + 1;
        buf.push(line);
        if size >= 1500 {
            flush(&mut chunks, &mut buf, &mut size, &title);
        }
    }
    flush(&mut chunks, &mut buf, &mut size, &title);
    if chunks.is_empty() {
        json!([{"source":"s","path":"docs/n.md","title":"n.md","text":py_prefix(py,&input,1500)}])
    } else {
        Value::Array(chunks)
    }
}

fn actual_chunks(py: Python<'_>, mi: &Bound<'_, PyModule>, body: &str, mode: &str) -> Value {
    let kw = kwargs(
        py,
        &[
            ("source_id", "s".into_pyobject(py).unwrap().into_any()),
            ("path", path(py, std::path::Path::new("docs/n.md"))),
            ("mode", mode.into_pyobject(py).unwrap().into_any()),
        ],
    );
    json_value(
        &mi.getattr("chunk_text")
            .unwrap()
            .call((body,), Some(&kw))
            .unwrap(),
    )
}

#[test]
fn chunk_text_randomized_and_exotic_boundaries_match_reference() {
    let _case = Case::new();
    Python::attach(|py| {
        let mi = module(py, "conductor.memory_index");
        let rng = PyModule::import(py, "random")
            .unwrap()
            .getattr("Random")
            .unwrap()
            .call1((20260906,))
            .unwrap();
        let topics = ["alpha", "beta", "gamma", "delta"];
        for round in 0..60 {
            let count: usize = rng
                .call_method1("choice", (vec![1, 4, 40, 120],))
                .unwrap()
                .extract()
                .unwrap();
            let mut lines = Vec::new();
            for _ in 0..count {
                let kind: String = rng
                    .call_method1("choice", (vec!["text", "heading", "blank", "space"],))
                    .unwrap()
                    .extract()
                    .unwrap();
                match kind.as_str() {
                    "text" => {
                        let topic: String = rng
                            .call_method1("choice", (topics.to_vec(),))
                            .unwrap()
                            .extract()
                            .unwrap();
                        let times: usize = rng
                            .call_method1("choice", (vec![1, 20, 80],))
                            .unwrap()
                            .extract()
                            .unwrap();
                        lines.push(topic.repeat(times));
                    }
                    "heading" => {
                        let level: String = rng
                            .call_method1(
                                "choice",
                                (vec!["", "#", "##", "###", "#### ", "##### "],),
                            )
                            .unwrap()
                            .extract()
                            .unwrap();
                        let topic: String = rng
                            .call_method1("choice", (topics.to_vec(),))
                            .unwrap()
                            .extract()
                            .unwrap();
                        lines.push(format!("{level} {topic}"));
                    }
                    "blank" => lines.push(String::new()),
                    "space" => lines.push("   ".to_owned()),
                    _ => unreachable!(),
                }
            }
            let document = lines.join("\n");
            let mode: String = rng
                .call_method1("choice", (vec!["chunk", "whole"],))
                .unwrap()
                .extract()
                .unwrap();
            for mode in [mode.as_str(), "chunk"] {
                assert_eq!(
                    actual_chunks(py, &mi, &document, mode),
                    reference_chunks(py, &document, mode),
                    "round {round} mode {mode}"
                );
            }
        }
        for sample in [
            "\x1cA\x1dB\x1eC",
            "a\r\nb\rc\x0bd\x0cf",
            "a\u{2028}b\u{2029}c",
            "\u{a0}x\u{a0}",
        ] {
            for mode in ["chunk", "whole"] {
                assert_eq!(
                    actual_chunks(py, &mi, sample, mode),
                    reference_chunks(py, sample, mode),
                    "{sample:?} {mode}"
                );
            }
        }
        let kw = kwargs(
            py,
            &[
                ("source_id", "src".into_pyobject(py).unwrap().into_any()),
                ("path", path(py, std::path::Path::new("docs/n.md"))),
                ("mode", "whole".into_pyobject(py).unwrap().into_any()),
            ],
        );
        assert_eq!(
            json_value(
                &mi.getattr("chunk_text")
                    .unwrap()
                    .call(("body",), Some(&kw))
                    .unwrap()
            ),
            json!([{"source":"src","path":"docs/n.md","title":"n.md","text":"body"}])
        );
    });
}
