#![cfg(feature = "python-compat-tests")]
//! Native date and glob compatibility with Python standard-library behavior.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use conductor_native::candidate_policy::{classify, fragment};
use pyo3::prelude::*;
use serde_json::json;
use support::{module, text, Case};

#[test]
fn native_iso_date_parser_agrees_with_python_date_fromisoformat() {
    let _case = Case::new();
    // Policy TOML may contain bare dates or strings. The previous Python parser
    // accepted every form date.fromisoformat accepts and normalized it to a date.
    let cases = [
        "2026-09-27",
        "20260927",
        "2026-W39-7",
        "2026W397",
        "2026-W39",
        "2026W39",
        "2020-W53-7",
        "2020W537",
        "0001-01-01",
        "9999-12-31",
        "2025-02-29",
        "2026-00-01",
        "2026-9-27",
        "026-09-27",
        "2021-W53-1",
        "2026-W00-1",
        "2026-W39-0",
        "2026-W39-8",
        "0000-01-01",
        "10000-01-01",
    ];
    Python::attach(|py| {
        let date = pyo3::types::PyModule::import(py, "datetime")
            .unwrap()
            .getattr("date")
            .unwrap();
        for input in cases {
            let expected = date
                .call_method1("fromisoformat", (input,))
                .map(|parsed| text(&parsed));
            let actual = fragment(
                "date",
                &json!({"field": "expiry", "value": input}),
                "2026-09-27",
            );
            match expected {
                Ok(expected) => assert_eq!(actual.unwrap(), json!(expected), "{input}"),
                Err(_) => assert_eq!(actual.unwrap_err(), "expiry must be an ISO date", "{input}"),
            }
        }
    });
}
#[test]
fn native_custom_globs_agree_with_python_fnmatchcase() {
    let _case = Case::new();
    let cases = [
        ("]", "[]]"),
        ("x", "[!]]"),
        ("]", "[!]]"),
        ("[", "[[]"),
        ("ab7.py", "ab[0-9].py"),
        ("abx.py", "ab[!0-9].py"),
        ("src/a/b.py", "src/*.py"),
        ("literal[", "literal["),
        ("a", "[a-]"),
        ("x", "[!a]"),
        ("a", "[!a]"),
        ("α", "[α-ω]"),
        ("a\nb", "a?b"),
    ];
    Python::attach(|py| {
        let fnmatchcase = module(py, "fnmatch").getattr("fnmatchcase").unwrap();
        for (candidate_path, pattern) in cases {
            let expected = fnmatchcase
                .call1((candidate_path, pattern))
                .unwrap()
                .extract::<bool>()
                .unwrap();
            let result = classify(
                &json!({"path": candidate_path, "new_mode": "100644"}),
                &json!({"class_globs": {"matched": [pattern]},
                        "generated_globs": [], "high_risk_globs": []}),
            )
            .unwrap();
            let actual = result["classes"]
                .as_array()
                .unwrap()
                .contains(&json!("matched"));
            assert_eq!(
                actual, expected,
                "path={candidate_path:?}, pattern={pattern:?}"
            );
        }
    });
}
