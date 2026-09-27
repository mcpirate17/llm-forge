#![cfg(feature = "python-compat-tests")]
//! Rust-owned candidate mutation-evidence and grandfather value-gate contracts.

#[path = "python_contracts/candidate_review_support.rs"]
#[allow(dead_code)]
mod candidate_review_support;
#[path = "python_contracts/git_fixture_support.rs"]
#[allow(dead_code)]
mod git_fixture_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use candidate_review_support::{
    added_change, crafted_grandfather_inventory, default_policy, gate_context, isolated_case,
    replace_fields, review_context,
};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use support::{module, path, AttrPatch, Case};

const TEST_PATH: &str = "research/tests/test_unregistered.py";
const RECEIPT_PATH: &str = "conductor/mutation_campaigns/receipts/new_test_value_receipt.json";
const GRANDFATHER: &str = "research/tests/test_legacy_probe.py";

fn json_object<'py>(py: Python<'py>, payload: Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((payload.to_string(),))
        .unwrap()
}

fn mock_verifier(py: Python<'_>, payload: Value) -> AttrPatch {
    let kwargs = PyDict::new(py);
    kwargs
        .set_item("return_value", json_object(py, payload))
        .unwrap();
    let mock = module(py, "unittest.mock")
        .getattr("Mock")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap();
    AttrPatch::replace(
        module(py, "conductor.mutation_testing").as_any(),
        "verify_evidence",
        &mock,
    )
}

fn check<'py>(py: Python<'py>, context: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.checks")
        .getattr("check_mutation_evidence")
        .unwrap()
        .call1((context,))
        .unwrap()
}

fn rules(result: &Bound<'_, PyAny>) -> Vec<String> {
    result
        .getattr("findings")
        .unwrap()
        .try_iter()
        .unwrap()
        .map(|f| f.unwrap().getattr("rule_id").unwrap().extract().unwrap())
        .collect()
}

fn value_context<'py>(
    py: Python<'py>,
    root: &Path,
    inventory: &[(&str, &[&str])],
) -> (Bound<'py, PyAny>, Vec<AttrPatch>, PathBuf) {
    let (original, guards) = gate_context(py, root, root, inventory, &"c".repeat(40));
    let snapshot: PathBuf = original.getattr("snapshot").unwrap().extract().unwrap();
    let file = snapshot.join(TEST_PATH);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, "def test_new_contract():\n    assert True\n").unwrap();
    let candidate = original.getattr("candidate").unwrap();
    let kwargs = PyDict::new(py);
    kwargs
        .set_item(
            "changes",
            PyTuple::new(
                py,
                [added_change(py, TEST_PATH, &["python", "source", "test"])],
            )
            .unwrap(),
        )
        .unwrap();
    let changed = replace_fields(py, &candidate, &kwargs);
    let kwargs = PyDict::new(py);
    kwargs.set_item("candidate", changed).unwrap();
    let context = replace_fields(py, &original, &kwargs);
    let receipt = snapshot.join(RECEIPT_PATH);
    fs::create_dir_all(receipt.parent().unwrap()).unwrap();
    (context, guards, receipt)
}

fn normal_context<'py>(
    py: Python<'py>,
    case: &Case,
    relative: &str,
    classes: &[&str],
) -> Bound<'py, PyAny> {
    let model = module(py, "conductor.candidate_review.model");
    let kwargs = PyDict::new(py);
    kwargs.set_item("kind", "index").unwrap();
    kwargs.set_item("tree_oid", "a".repeat(40)).unwrap();
    kwargs.set_item("base_tree_oid", "b".repeat(40)).unwrap();
    kwargs.set_item("base_commit_oid", "c".repeat(40)).unwrap();
    kwargs.set_item("commit_oid", py.None()).unwrap();
    kwargs.set_item("target_ref", "HEAD").unwrap();
    kwargs
        .set_item(
            "changes",
            PyTuple::new(py, [added_change(py, relative, classes)]).unwrap(),
        )
        .unwrap();
    let candidate = model
        .getattr("Candidate")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap();
    review_context(
        py,
        case.root(),
        case.root(),
        &candidate,
        PyTuple::empty(py).as_any(),
        &default_policy(py),
        "manual",
        "fast",
        None,
        &case.root().join("runtime"),
    )
}

#[test]
fn javascript_and_native_specs_classify_as_tests() {
    let _case = isolated_case();
    Python::attach(|py| {
        let policy = default_policy(py);
        for (file, expected) in [
            ("aria_designer/e2e/designer.spec.js", true),
            ("research/runtime/native/test_kernel.c", true),
            ("research/tools/mixer_fingerprint.py", false),
        ] {
            let change = added_change(py, file, &[]);
            let classified = policy.call_method1("classify_change", (change,)).unwrap();
            let classes = classified.getattr("classes").unwrap();
            assert_eq!(classes.contains("test").unwrap(), expected, "{file}");
        }
    });
}

#[test]
fn mutation_evidence_skips_when_no_tests_changed() {
    let case = isolated_case();
    Python::attach(|py| {
        let context = normal_context(
            py,
            &case,
            "research/tools/mixer_fingerprint.py",
            &["python", "source"],
        );
        let result = check(py, &context);
        assert_eq!(
            result
                .getattr("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "skipped"
        );
    });
}

#[test]
fn mutation_evidence_fails_closed_without_registry() {
    let case = isolated_case();
    Python::attach(|py| {
        let context = normal_context(py, &case, TEST_PATH, &["python", "source", "test"]);
        let result = check(py, &context);
        assert_eq!(
            result
                .getattr("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "failed"
        );
        assert_eq!(rules(&result), ["mutation-registry-missing"]);
    });
}

#[test]
fn mutation_evidence_fails_closed_without_receipt() {
    let case = isolated_case();
    Python::attach(|py| {
        let snapshot = case.root().join("snapshot");
        fs::create_dir_all(snapshot.join("conductor/mutation_campaigns")).unwrap();
        fs::write(
            snapshot.join("conductor/mutation_campaigns/registry.json"),
            "{}",
        )
        .unwrap();
        fs::create_dir_all(snapshot.join("research/tests")).unwrap();
        fs::write(
            snapshot.join(TEST_PATH),
            "def test_new_contract():\n    assert True\n",
        )
        .unwrap();
        let original = normal_context(py, &case, TEST_PATH, &["python", "source", "test"]);
        let kwargs = PyDict::new(py);
        kwargs.set_item("snapshot", path(py, &snapshot)).unwrap();
        let context = replace_fields(py, &original, &kwargs);
        let _verifier = mock_verifier(
            py,
            json!({"status":"FAIL","checked_test_paths":[TEST_PATH],
            "evidence":[],"missing_evidence":[{"path":TEST_PATH,
                "reason":"no registered campaign ranks this test file","receipt_rejections":[]}],
            "malformed_receipts":[]}),
        );
        let result = check(py, &context);
        assert_eq!(
            result
                .getattr("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "failed"
        );
        let first = result.getattr("findings").unwrap().get_item(0).unwrap();
        assert_eq!(
            first
                .getattr("rule_id")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "missing-mutation-receipt"
        );
        assert_eq!(
            first.getattr("path").unwrap().extract::<String>().unwrap(),
            TEST_PATH
        );
    });
}

#[test]
fn unavailable_malformed_and_admitted_receipts() {
    let case = isolated_case();
    Python::attach(|py| {
        let (context, _anchor, receipt) =
            value_context(py, case.root(), &[(GRANDFATHER, &["test_frozen_legacy"])]);
        let mutation = module(py, "conductor.mutation_testing");
        let campaign_error = mutation
            .getattr("CampaignError")
            .unwrap()
            .call1(("broken registry",))
            .unwrap();
        let kwargs = PyDict::new(py);
        kwargs.set_item("side_effect", campaign_error).unwrap();
        let mock = module(py, "unittest.mock")
            .getattr("Mock")
            .unwrap()
            .call((), Some(&kwargs))
            .unwrap();
        let unavailable = AttrPatch::replace(mutation.as_any(), "verify_evidence", &mock);
        let result = check(py, &context);
        assert_eq!(rules(&result)[0], "mutation-evidence-unavailable");
        drop(unavailable);

        let malformed = mock_verifier(
            py,
            json!({"status":"FAIL","checked_test_paths":[TEST_PATH],
            "evidence":[],"missing_evidence":["not-a-dict"],
            "malformed_receipts":["receipt.json: truncated"]}),
        );
        let result = check(py, &context);
        assert_eq!(
            rules(&result).into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "malformed-mutation-receipt".to_owned(),
                "new-test-value-not-admitted".to_owned()
            ])
        );
        assert!(result
            .getattr("findings")
            .unwrap()
            .try_iter()
            .unwrap()
            .any(|f| {
                let f = f.unwrap();
                f.getattr("rule_id").unwrap().extract::<String>().unwrap()
                    == "new-test-value-not-admitted"
                    && f.getattr("message")
                        .unwrap()
                        .extract::<String>()
                        .unwrap()
                        .contains("::test_new_contract")
            }));
        drop(malformed);

        fs::write(
            &receipt,
            json!({"status":"PASS","test_value":null}).to_string(),
        )
        .unwrap();
        let evidence = json!({"status":"PASS","checked_test_paths":[TEST_PATH],
            "evidence":[{"path":TEST_PATH,"campaign_id":"new_test_value","receipt":RECEIPT_PATH,"scope":{}}],
            "missing_evidence":[],"malformed_receipts":[]});
        let _valid_verifier = mock_verifier(py, evidence);
        assert_eq!(rules(&check(py, &context)), ["new-test-value-not-admitted"]);
        fs::write(&receipt, json!({"status":"PASS","test_value":{
            "schema_version":"llm.mutation-testing.test-value.v1","status":"PASS",
            "tests":[{"nodeid":format!("{TEST_PATH}::test_new_contract"),"classification":"CORE"}]}}).to_string()).unwrap();
        let result = check(py, &context);
        assert!(rules(&result).is_empty());
        assert_eq!(
            result
                .getattr("metrics")
                .unwrap()
                .get_item("value_gated_nodeids")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            [format!("{TEST_PATH}::test_new_contract")]
        );
    });
}

#[test]
fn value_gate_anchors_grandfather_exemption() {
    let case = isolated_case();
    Python::attach(|py| {
        let rows = [(TEST_PATH, &["test_old_frozen"] as &[&str])];
        let (context, _anchor, receipt) = value_context(py, case.root(), &rows);
        let snapshot: PathBuf = context.getattr("snapshot").unwrap().extract().unwrap();
        fs::write(snapshot.join(TEST_PATH), "def test_old_frozen():\n    assert True\n\ndef test_new_contract():\n    assert True\n").unwrap();
        let crafted: Value =
            serde_json::from_str(&crafted_grandfather_inventory(py, &rows)).unwrap();
        let relative: String = module(py, "conductor.candidate_review.verification")
            .getattr("GRANDFATHER_INVENTORY_RELPATH")
            .unwrap()
            .extract()
            .unwrap();
        let inventory_path = snapshot.join(relative);
        let actual: Value = serde_json::from_slice(&fs::read(inventory_path).unwrap()).unwrap();
        assert_eq!(crafted, actual);
        fs::write(
            &receipt,
            json!({"status":"PASS","test_value":{
            "schema_version":"llm.mutation-testing.test-value.v1","status":"PASS","tests":[]}})
            .to_string(),
        )
        .unwrap();
        let _verifier = mock_verifier(
            py,
            json!({"status":"FAIL","checked_test_paths":[TEST_PATH],
            "evidence":[{"path":TEST_PATH,"campaign_id":"new_test_value","receipt":RECEIPT_PATH,"scope":{}}],
            "missing_evidence":[],"malformed_receipts":[]}),
        );
        let result = check(py, &context);
        let value_findings: Vec<_> = result
            .getattr("findings")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(Result::unwrap)
            .filter(|f| {
                f.getattr("rule_id").unwrap().extract::<String>().unwrap()
                    == "new-test-value-not-admitted"
            })
            .collect();
        assert_eq!(value_findings.len(), 1);
        let message: String = value_findings[0]
            .getattr("message")
            .unwrap()
            .extract()
            .unwrap();
        assert!(message.contains("::test_new_contract"));
        assert!(!message.contains("::test_old_frozen"));
        assert_eq!(
            result
                .getattr("metrics")
                .unwrap()
                .get_item("value_gated_nodeids")
                .unwrap()
                .extract::<Vec<String>>()
                .unwrap(),
            [format!("{TEST_PATH}::test_new_contract")]
        );
    });
}

fn assert_inventory_invalid(py: Python<'_>, context: &Bound<'_, PyAny>) {
    let _verifier = mock_verifier(
        py,
        json!({"status":"FAIL","code_paths":[],"evidence":[],
        "missing_evidence":[],"malformed_receipts":[]}),
    );
    let result = check(py, context);
    assert!(rules(&result).contains(&"grandfather-inventory-invalid".to_owned()));
    let metrics = result.getattr("metrics").unwrap();
    assert_eq!(
        metrics
            .get_item("value_gated_nodeids")
            .unwrap()
            .len()
            .unwrap(),
        0
    );
}

#[test]
fn grandfather_inventory_failures_fail_closed() {
    let case = isolated_case();
    Python::attach(|py| {
        let rows = [(GRANDFATHER, &["test_frozen_legacy"] as &[&str])];
        for scenario in ["absent", "drift", "whitespace-only", "milestone"] {
            let root = case.root().join(scenario);
            let (context, anchor, _) = value_context(py, &root, &rows);
            let snapshot: PathBuf = context.getattr("snapshot").unwrap().extract().unwrap();
            let relative: String = module(py, "conductor.candidate_review.verification")
                .getattr("GRANDFATHER_INVENTORY_RELPATH")
                .unwrap()
                .extract()
                .unwrap();
            let inventory = snapshot.join(relative);
            let digest_guard = match scenario {
                "absent" => {
                    fs::remove_file(&inventory).unwrap();
                    None
                }
                "drift" => {
                    let mut payload: Value =
                        serde_json::from_slice(&fs::read(&inventory).unwrap()).unwrap();
                    payload["tests"][GRANDFATHER]
                        .as_array_mut()
                        .unwrap()
                        .push(json!("test_extra_not_real"));
                    fs::write(&inventory, payload.to_string()).unwrap();
                    None
                }
                "whitespace-only" => {
                    let original = fs::read(&inventory).unwrap();
                    let mut changed = original.clone();
                    changed.push(b'\n');
                    let a: Value = serde_json::from_slice(&original).unwrap();
                    let b: Value = serde_json::from_slice(&changed).unwrap();
                    assert_eq!(a, b);
                    fs::write(&inventory, changed).unwrap();
                    let digest = format!("{:x}", Sha256::digest(&original));
                    let value = pyo3::types::PyString::new(py, &digest);
                    Some(AttrPatch::replace(
                        module(py, "conductor.candidate_review.verification").as_any(),
                        "GRANDFATHER_INVENTORY_SHA256",
                        value.as_any(),
                    ))
                }
                _ => {
                    let policy = module(py, "conductor.candidate_review.policy");
                    let milestone: String = policy
                        .getattr("W7_TRIDENT_LINEAR_INTEGRATION_MILESTONE")
                        .unwrap()
                        .extract()
                        .unwrap();
                    let changed = fs::read_to_string(&inventory)
                        .unwrap()
                        .replace(&milestone, "w7-bogus");
                    assert!(!changed.contains(&milestone));
                    fs::write(&inventory, &changed).unwrap();
                    let digest = format!("{:x}", Sha256::digest(changed.as_bytes()));
                    let value = pyo3::types::PyString::new(py, &digest);
                    Some(AttrPatch::replace(
                        module(py, "conductor.candidate_review.verification").as_any(),
                        "GRANDFATHER_INVENTORY_SHA256",
                        value.as_any(),
                    ))
                }
            };
            assert_inventory_invalid(py, &context);
            drop(digest_guard);
            drop(anchor);
        }
    });
}
