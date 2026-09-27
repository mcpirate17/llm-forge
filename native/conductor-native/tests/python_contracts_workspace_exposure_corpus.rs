#![cfg(feature = "python-compat-tests")]
//! Python exposure-line side of the shared frozen corpus, driven by Rust.

#[path = "fixtures/workspace_exposure_corpus.rs"]
mod corpus_fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use serde_json::Value;
use std::collections::BTreeMap;
use support::{module, path, Case};

const CORPUS: &str = include_str!("../../forge/tests/fixtures/workspace_exposure_corpus.json");
const EXPECTED: &str = include_str!("../../forge/tests/fixtures/workspace_exposure_expected.json");

fn corpus() -> Vec<Value> {
    serde_json::from_str(CORPUS).expect("shared workspace exposure corpus")
}

fn expected() -> BTreeMap<String, String> {
    serde_json::from_str(EXPECTED).expect("frozen Python exposure lines")
}

#[test]
fn fixture_files_exist_and_are_shared_with_native_parity() {
    let rows = corpus();
    let frozen = expected();
    assert_eq!(rows.len(), frozen.len());
    assert!(
        rows.len() >= 12,
        "expected 12+ corpus cases, got {}",
        rows.len()
    );
}

#[test]
fn python_exposure_line_matches_frozen_corpus() {
    let case = Case::new();
    let rows = corpus();
    let frozen = expected();
    let mut failures = Vec::new();
    for recipe in &rows {
        let id = recipe["id"].as_str().unwrap();
        let repo = corpus_fixture::build_case(case.root(), recipe);
        let actual = Python::attach(|py| {
            module(py, "conductor.workspace_hygiene")
                .getattr("exposure_line")
                .unwrap()
                .call1((path(py, &repo),))
                .unwrap()
                .extract::<String>()
                .unwrap()
        });
        let expected = frozen
            .get(id)
            .unwrap_or_else(|| panic!("missing expected line for {id}"));
        if actual != *expected {
            failures.push(format!(
                "case {id:?}: python={actual:?} expected={expected:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} parity mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
