#![cfg(feature = "python-compat-tests")]
//! Python wrapper contracts over the native slim-receipt codec.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
#[path = "python_contracts/mutation_value_support.rs"]
#[allow(dead_code)]
mod value_support;

use pyo3::prelude::*;
use serde_json::{json, Value};
use std::fs;
use support::{assert_error, module, path, Case};
use value_support::{from_python, to_python};

fn receipt(mutants: usize) -> Value {
    json!({
        "campaign_id":"c-slim", "status":"RATCHET_HELD",
        "generated_at":"2026-09-13T00:00:00+00:00",
        "mutation_score":0.9459459459459459,
        "mutants":(0..mutants).map(|i| json!({"id":format!("m{i}"),"outcome":"KILLED","timing_ms":0.5})).collect::<Vec<_>>()
    })
}

#[test]
fn python_codec_round_trips_and_reports_superseded_pointer() {
    let _case = Case::new();
    Python::attach(|py| {
        let codec = module(py, "conductor.mutation_receipt_slim");
        let source = receipt(5);
        let slim = codec
            .getattr("slim_receipt")
            .unwrap()
            .call1((to_python(py, &source),))
            .unwrap();
        assert!(slim
            .is_instance(&py.get_type::<pyo3::types::PyDict>())
            .unwrap());
        let expanded = codec
            .getattr("expand_receipt")
            .unwrap()
            .call1((slim,))
            .unwrap();
        assert_eq!(from_python(&expanded), source);
        let pointer = json!({"campaign_id":"c","status":"PASS","detail":{
            "encoding":"superseded","superseded_by":"newer.json"
        }});
        assert_error(
            py,
            codec
                .getattr("expand_receipt")
                .unwrap()
                .call1((to_python(py, &pointer),))
                .unwrap_err(),
            &codec.getattr("ReceiptDetailError").unwrap(),
            "superseded by newer.json",
        );
    });
}

#[test]
fn field_reader_uses_summary_legacy_and_detail_without_guessing_missing_keys() {
    let _case = Case::new();
    Python::attach(|py| {
        let codec = module(py, "conductor.mutation_receipt_slim");
        let source = receipt(80);
        let slim = codec
            .getattr("slim_receipt")
            .unwrap()
            .call1((to_python(py, &source),))
            .unwrap();
        let field = codec.getattr("expand_receipt_field").unwrap();
        assert_eq!(
            from_python(&field.call1((&slim, "status")).unwrap()),
            json!("RATCHET_HELD")
        );
        assert_eq!(
            from_python(&field.call1((to_python(py, &source), "mutants")).unwrap()),
            source["mutants"]
        );
        assert_eq!(
            from_python(&field.call1((&slim, "mutants")).unwrap()),
            source["mutants"]
        );
        assert!(field.call1((&slim, "no_such_key")).unwrap().is_none());
        assert!(field
            .call1((to_python(py, &json!({"campaign_id":"c"})), "mutants"))
            .unwrap()
            .is_none());
    });
}

#[test]
fn writer_lands_canonical_json_and_expands_all_detail() {
    let case = Case::new();
    let output = case.root().join("slim.json");
    let source = receipt(80);
    Python::attach(|py| {
        let codec = module(py, "conductor.mutation_receipt_slim");
        let written = codec
            .getattr("write_slim_receipt")
            .unwrap()
            .call1((path(py, &output), to_python(py, &source)))
            .unwrap();
        assert_eq!(from_python(&written)["detail"]["encoding"], "zstd+base64");
        let disk_text = fs::read_to_string(&output).unwrap();
        let disk: Value = serde_json::from_str(&disk_text).unwrap();
        assert_eq!(disk["detail"]["encoding"], "zstd+base64");
        assert_eq!(
            from_python(
                &codec
                    .getattr("expand_receipt")
                    .unwrap()
                    .call1((to_python(py, &disk),))
                    .unwrap()
            ),
            source
        );
        assert_eq!(
            disk_text,
            format!("{}\n", serde_json::to_string_pretty(&disk).unwrap())
        );
    });
}

#[test]
fn compactor_maps_native_errors_to_receipt_detail_error() {
    let case = Case::new();
    Python::attach(|py| {
        let codec = module(py, "conductor.mutation_receipt_slim");
        let keep = json!({"c-slim":"nope.json"});
        assert_error(
            py,
            codec
                .getattr("compact_directory")
                .unwrap()
                .call1((path(py, case.root()), to_python(py, &keep)))
                .unwrap_err(),
            &codec.getattr("ReceiptDetailError").unwrap(),
            "not a receipt",
        );
    });
}
