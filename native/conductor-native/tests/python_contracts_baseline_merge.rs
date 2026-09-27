#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for lossless governance baseline merging.

#[path = "python_contracts/baseline_merge_support.rs"]
mod baseline_support;
#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use baseline_support::{baseline, main, merge, read_baseline, real_src_root, signature, value};
use pyo3::prelude::*;
use pyo3::types::PySet;
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use support::{assert_error, Case};

#[test]
fn secrets_shape_unions_per_path_without_dropping_absent_files() {
    let _case = Case::new();
    Python::attach(|py| {
        let base = value(
            py,
            json!({"results": {
                "research/tools/top10_canary.py": [{"type":"Hex","hashed_secret":"a"}],
                "shared.py": [{"type":"Hex","hashed_secret":"b"}]
            }}),
        );
        let incoming = value(
            py,
            json!({"results": {
                "shared.py": [{"type":"Hex","hashed_secret":"c"}],
                "new_tool.py": [{"type":"Hex","hashed_secret":"d"}]
            }}),
        );
        let merged = merge(py, &base, &incoming);
        let results = merged.get_item("results").unwrap();
        assert!(results.contains("research/tools/top10_canary.py").unwrap());
        assert_eq!(results.get_item("shared.py").unwrap().len().unwrap(), 2);
        assert!(results.contains("new_tool.py").unwrap());
        let lost = signature(py, &base, "results")
            .call_method1("__sub__", (signature(py, &merged, "results"),))
            .unwrap();
        assert_eq!(lost.len().unwrap(), 0);
    });
}

#[test]
fn a_merge_that_would_drop_an_entry_raises_and_names_it() {
    let _case = Case::new();
    Python::attach(|py| {
        let base = value(py, json!({"entries":{"kept":1,"dropped":2}}));
        let merged = merge(py, &base, &value(py, json!({"entries":{"added":3}})));
        let keys = merged
            .get_item("entries")
            .unwrap()
            .call_method0("keys")
            .unwrap();
        let expected = PySet::new(py, ["kept", "dropped", "added"]).unwrap();
        assert!(keys.eq(expected).unwrap());
        let api = baseline(py);
        let error = api
            .getattr("assert_no_loss")
            .unwrap()
            .call1((&base, value(py, json!({"entries":{"kept":1}})), "entries"))
            .unwrap_err();
        assert_error(
            py,
            error,
            &api.getattr("BaselineMergeError").unwrap(),
            "dropped",
        );
        api.getattr("assert_no_loss")
            .unwrap()
            .call1((&base, &merged, "entries"))
            .unwrap();
    });
}

#[test]
fn list_shaped_baselines_deduplicate_by_content_not_order() {
    let _case = Case::new();
    Python::attach(|py| {
        let base = value(
            py,
            json!({"findings":[{"f":"a","rank":"C"},{"f":"b","rank":"D"}]}),
        );
        let incoming = value(
            py,
            json!({"findings":[{"rank":"D","f":"b"},{"f":"c","rank":"E"}]}),
        );
        assert_eq!(
            merge(py, &base, &incoming)
                .get_item("findings")
                .unwrap()
                .len()
                .unwrap(),
            3
        );
    });
}

#[test]
fn count_is_refreshed_and_container_mismatch_is_refused() {
    let _case = Case::new();
    Python::attach(|py| {
        let api = baseline(py);
        let class = api.getattr("BaselineMergeError").unwrap();
        let merged = merge(
            py,
            &value(py, json!({"count":1,"entries":{"a":1}})),
            &value(py, json!({"entries":{"b":2}})),
        );
        assert!(merged.get_item("count").unwrap().eq(2).unwrap());
        let err = api
            .getattr("merge")
            .unwrap()
            .call1((
                value(py, json!({"entries":{}})),
                value(py, json!({"results":{}})),
            ))
            .unwrap_err();
        assert_error(py, err, &class, "different container keys");
        let err = api
            .getattr("container_key")
            .unwrap()
            .call1((value(py, json!({"nothing":1})),))
            .unwrap_err();
        assert_error(py, err, &class, "no known container key");
    });
}

#[test]
fn scalar_bucket_on_both_sides_keeps_the_base_value() {
    let _case = Case::new();
    Python::attach(|py| {
        let merged = merge(
            py,
            &value(py, json!({"entries":{"k":"base"}})),
            &value(py, json!({"entries":{"k":"incoming"}})),
        );
        assert!(merged
            .get_item("entries")
            .unwrap()
            .get_item("k")
            .unwrap()
            .eq("base")
            .unwrap());
    });
}

#[test]
fn a_container_that_is_neither_dict_nor_list_is_refused() {
    let _case = Case::new();
    Python::attach(|py| {
        let api = baseline(py);
        let err = api
            .getattr("signature")
            .unwrap()
            .call1((
                value(py, json!({"entries":"a string is not a baseline"})),
                "entries",
            ))
            .unwrap_err();
        assert_error(
            py,
            err,
            &api.getattr("BaselineMergeError").unwrap(),
            "not dict/list",
        );
    });
}

#[test]
fn mismatched_container_types_are_refused() {
    let _case = Case::new();
    Python::attach(|py| {
        let api = baseline(py);
        let err = api
            .getattr("merge")
            .unwrap()
            .call1((
                value(py, json!({"entries":{"a":1}})),
                value(py, json!({"entries":[{"b":2}]})),
            ))
            .unwrap_err();
        assert_error(
            py,
            err,
            &api.getattr("BaselineMergeError").unwrap(),
            "container types differ",
        );
    });
}

#[test]
fn cli_writes_on_merge_and_writes_nothing_under_check() {
    let case = Case::new();
    let base = case.write("base.json", "{\"entries\": {\"kept\": 1}}");
    let incoming = case.write("incoming.json", "{\"entries\": {\"added\": 2}}");
    let out = case.root().join("out.json");
    let before = fs::read_to_string(&base).unwrap();
    Python::attach(|py| {
        let base_name = base.to_str().unwrap();
        let incoming_name = incoming.to_str().unwrap();
        assert_eq!(main(py, &[base_name, incoming_name, "--check"]), 0);
        assert_eq!(fs::read_to_string(&base).unwrap(), before);
        assert_eq!(main(py, &[base_name, incoming_name]), 0);
        let merged: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&base).unwrap()).unwrap();
        let keys: BTreeSet<_> = merged["entries"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, BTreeSet::from(["kept", "added"]));
        fs::write(&base, &before).unwrap();
        assert_eq!(
            main(
                py,
                &[base_name, incoming_name, "--out", out.to_str().unwrap()]
            ),
            0
        );
        assert_eq!(fs::read_to_string(&base).unwrap(), before);
        let merged: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&out).unwrap()).unwrap();
        let keys: BTreeSet<_> = merged["entries"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, BTreeSet::from(["kept", "added"]));
    });
}

fn self_merge_is_exact_no_op(relative: &str) {
    let _case = Case::new();
    let file = real_src_root().join(relative);
    if !file.is_file() {
        eprintln!("baseline absent from this tree: {relative}");
        return;
    }
    Python::attach(|py| {
        let document = read_baseline(py, &file);
        let api = baseline(py);
        let key: String = api
            .getattr("container_key")
            .unwrap()
            .call1((&document,))
            .unwrap()
            .extract()
            .unwrap();
        let merged = merge(py, &document, &document);
        assert!(signature(py, &merged, &key)
            .eq(signature(py, &document, &key))
            .unwrap());
        assert_eq!(
            merged.get_item(&key).unwrap().len().unwrap(),
            document.get_item(&key).unwrap().len().unwrap()
        );
    });
}

#[test]
fn self_merge_jscpd_entries() {
    self_merge_is_exact_no_op("conductor/jscpd_duplication_baseline.json");
}
#[test]
fn self_merge_pmd_entries() {
    self_merge_is_exact_no_op("conductor/pmd_cpd_duplication_baseline.json");
}
#[test]
fn self_merge_radon_findings() {
    self_merge_is_exact_no_op("conductor/radon_complexity_baseline.json");
}
#[test]
fn self_merge_vulture_entries() {
    self_merge_is_exact_no_op("conductor/vulture_baseline.json");
}
#[test]
fn self_merge_secrets_results() {
    self_merge_is_exact_no_op(".secrets.baseline");
}
