#![cfg(feature = "python-compat-tests")]
//! Rust-owned assertions for the Python policy dataclass and native rules boundary.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use conductor_native::candidate_policy::{classify, fragment};
use pyo3::prelude::*;
use pyo3::types::PyTuple;
use serde_json::json;
use support::{assert_error, attr_text, module, path, text, Case};

const POLICY: &str = r#"schema_version = 1
block_at = "high"
max_workers = 2
cache_ttl_days = 7
claim_max_age_hours = 24
max_file_bytes = 100000
max_binary_bytes = 200000
coverage_threshold = 80.0
high_risk_coverage_threshold = 90.0
baseline_expires = 2099-01-01

[classes]
governance = ["conductor/**"]

[risk]
high = ["conductor/**"]

[paths]
protected_deletes = []
hot = []
generated = ["generated/**"]

[checks.candidate-integrity]
kind = "builtin"
profiles = ["fast", "full"]
classes = []
severity = "critical"
always = true
"#;

#[test]
fn native_iso_date_parser_agrees_with_python_date_fromisoformat() {
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

#[test]
fn load_policy_keeps_dates_enums_tuples_defaults_and_order() {
    let case = Case::new();
    let fixture = case.write("candidate_policy.toml", POLICY);
    Python::attach(|py| {
        let policy_module = module(py, "conductor.candidate_review.policy");
        let policy = policy_module
            .getattr("load_policy")
            .unwrap()
            .call1((path(py, &fixture),))
            .unwrap();
        assert_eq!(attr_text(&policy, "block_at"), "high");
        assert_eq!(attr_text(&policy, "baseline_expires"), "2099-01-01");
        assert_eq!(
            policy
                .getattr("max_workers")
                .unwrap()
                .extract::<u32>()
                .unwrap(),
            2
        );
        assert_eq!(
            policy
                .getattr("checks")
                .unwrap()
                .cast::<PyTuple>()
                .unwrap()
                .len(),
            1
        );
        let check = policy.getattr("checks").unwrap().get_item(0).unwrap();
        assert_eq!(attr_text(&check, "check_id"), "candidate-integrity");
        assert_eq!(
            policy
                .call_method1("active_checks", ("fast",))
                .unwrap()
                .len()
                .unwrap(),
            1
        );
        assert_eq!(attr_text(&check, "severity"), "critical");
        assert_eq!(
            check
                .getattr("timeout_seconds")
                .unwrap()
                .extract::<u32>()
                .unwrap(),
            60
        );
        assert!(check.getattr("cache").unwrap().extract::<bool>().unwrap());
        assert_eq!(
            check
                .getattr("profiles")
                .unwrap()
                .cast::<PyTuple>()
                .unwrap()
                .len(),
            2
        );
    });
}

#[test]
fn policy_errors_and_change_classification_keep_public_shapes() {
    let case = Case::new();
    let fixture = case.write("candidate_policy.toml", POLICY);
    let bad = case.write(
        "bad.toml",
        &POLICY.replace("max_workers = 2", "max_workers = 17"),
    );
    Python::attach(|py| {
        let policy_module = module(py, "conductor.candidate_review.policy");
        let error_class = policy_module.getattr("PolicyError").unwrap();
        let load = policy_module.getattr("load_policy").unwrap();
        assert_error(
            py,
            load.call1((path(py, &bad),)).unwrap_err(),
            &error_class,
            "max_workers must be <= 16, got 17",
        );
        let policy = load.call1((path(py, &fixture),)).unwrap();
        let model = module(py, "conductor.candidate_review.model");
        let change = model
            .getattr("Change")
            .unwrap()
            .call1((
                "R",
                "generated/renamed.py",
                "conductor/old.rs",
                "120000",
                "100644",
                "old",
                "new",
            ))
            .unwrap();
        let classified = policy.call_method1("classify_change", (change,)).unwrap();
        assert_eq!(attr_text(&classified, "risk"), "high");
        let classes = classified.getattr("classes").unwrap();
        assert!(classes.cast::<PyTuple>().unwrap().len() >= 5);
        for expected in ["generated", "governance", "python", "rust", "symlink"] {
            assert!(classes.contains(expected).unwrap(), "missing {expected}");
        }
        assert_eq!(text(&classified.getattr("status").unwrap()), "R");
    });
}

#[test]
fn value_waiver_parser_keeps_exact_nodeids_and_date_objects() {
    let _case = Case::new();
    Python::attach(|py| {
        let policy_module = module(py, "conductor.candidate_review.policy");
        let json_module = module(py, "json");
        let raw = json_module.getattr("loads").unwrap().call1((
            r#"[{"integration_base":"dddddddddddddddddddddddddddddddddddddddd","nodeids":["src/test_probe.py::test_one"],"reason":"Temporary bounded value waiver","approved_by":"Tim","approved_on":"2026-09-01","expires":"2026-10-01"}]"#,
        )).unwrap();
        let parsed = policy_module
            .getattr("_parse_value_waivers")
            .unwrap()
            .call1((raw,))
            .unwrap();
        let waivers = parsed.cast::<PyTuple>().unwrap();
        assert_eq!(waivers.len(), 1);
        let waiver = waivers.get_item(0).unwrap();
        assert_eq!(attr_text(&waiver, "approved_on"), "2026-09-01");
        assert_eq!(attr_text(&waiver, "expires"), "2026-10-01");
        assert_eq!(
            waiver
                .getattr("nodeids")
                .unwrap()
                .cast::<PyTuple>()
                .unwrap()
                .len(),
            1
        );
        let invalid = json_module.getattr("loads").unwrap().call1((
            r#"[{"integration_base":"dddddddddddddddddddddddddddddddddddddddd","nodeids":["src/test_probe.py::test_*"],"reason":"Temporary bounded value waiver","approved_by":"Tim","approved_on":"2026-09-01"}]"#,
        )).unwrap();
        let error = policy_module
            .getattr("_parse_value_waivers")
            .unwrap()
            .call1((invalid,))
            .unwrap_err();
        assert_error(
            py,
            error,
            &policy_module.getattr("PolicyError").unwrap(),
            "value_waivers.nodeids are exact, never patterns",
        );
    });
}
