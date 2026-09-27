#![cfg(feature = "python-compat-tests")]
//! Rust-owned mutation receipt and grandfather-inventory hardening contracts.

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
    crafted_grandfather_inventory, gate_context, isolated_case, write_grandfather_inventory,
};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, AttrPatch};

const PROBE: &str = "research/tests/test_probe.py";
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

fn json_object<'py>(py: Python<'py>, payload: Value) -> Bound<'py, PyAny> {
    module(py, "json")
        .getattr("loads")
        .unwrap()
        .call1((payload.to_string(),))
        .unwrap()
}

fn rule_ids(result: &Bound<'_, PyAny>) -> Vec<String> {
    result
        .getattr("findings")
        .unwrap()
        .try_iter()
        .unwrap()
        .map(|item| item.unwrap().getattr("rule_id").unwrap().extract().unwrap())
        .collect()
}

fn finding_rules(findings: &Bound<'_, PyAny>) -> Vec<String> {
    findings
        .try_iter()
        .unwrap()
        .map(|item| item.unwrap().getattr("rule_id").unwrap().extract().unwrap())
        .collect()
}

fn message(findings: &Bound<'_, PyAny>, index: usize) -> String {
    findings
        .get_item(index)
        .unwrap()
        .getattr("message")
        .unwrap()
        .extract()
        .unwrap()
}

fn check<'py>(py: Python<'py>, context: &Bound<'py, PyAny>) -> Bound<'py, PyAny> {
    module(py, "conductor.candidate_review.checks")
        .getattr("check_mutation_evidence")
        .unwrap()
        .call1((context,))
        .unwrap()
}

fn gate<'py>(py: Python<'py>, root: &Path) -> (Bound<'py, PyAny>, Vec<AttrPatch>) {
    gate_context(
        py,
        root,
        root,
        &[(PROBE, &["test_probe_legacy"])],
        &"c".repeat(40),
    )
}

fn patch_verifier(py: Python<'_>, payload: Value) -> AttrPatch {
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

#[test]
fn mutation_evidence_containers_fail_closed() {
    let _case = isolated_case();
    Python::attach(|py| {
        let verification = module(py, "conductor.candidate_review.verification");
        let kwargs = PyDict::new(py);
        kwargs.set_item("waived", Vec::<String>::new()).unwrap();
        let payload = json_object(
            py,
            json!({"missing_evidence":{"path":PROBE},"malformed_receipts":null}),
        );
        let findings = verification
            .getattr("_mutation_receipt_findings")
            .unwrap()
            .call((payload,), Some(&kwargs))
            .unwrap();
        assert_eq!(
            finding_rules(&findings),
            [
                "malformed-evidence-container",
                "malformed-evidence-container"
            ]
        );
        assert!(message(&findings, 0).contains("missing_evidence"));
        assert!(message(&findings, 0).contains("cannot be evaluated"));
        assert!(message(&findings, 1).contains("malformed_receipts"));
        kwargs.set_item("waived", ["waived/path.py"]).unwrap();
        let payload = json_object(
            py,
            json!({"missing_evidence":["not-an-object",{"path":"waived/path.py"}]}),
        );
        let rows = verification
            .getattr("_mutation_receipt_findings")
            .unwrap()
            .call((payload,), Some(&kwargs))
            .unwrap();
        assert_eq!(finding_rules(&rows), ["malformed-mutation-receipt"]);
        assert!(message(&rows, 0).contains("not-an-object"));
    });
}

#[test]
fn mutation_evidence_rows_fail_closed() {
    let case = isolated_case();
    Python::attach(|py| {
        let (context, _anchor) = gate(py, case.root());
        let payload = json_object(
            py,
            json!({"evidence":[42,{"campaign_id":"unidentifiable-row"},
            {"path":PROBE,"receipt":"first.json"},{"path":PROBE,"receipt":"second.json"}]}),
        );
        let nodeids = json_object(
            py,
            json!({"research/tests/test_probe.py":[format!("{PROBE}::test_probe_new")]}),
        );
        let findings = module(py, "conductor.candidate_review.verification")
            .getattr("_new_test_value_findings")
            .unwrap()
            .call1((context, payload, nodeids))
            .unwrap();
        assert_eq!(
            finding_rules(&findings),
            [
                "malformed-evidence-row",
                "malformed-evidence-row",
                "duplicate-evidence-row",
                "test-value-receipt-unavailable"
            ]
        );
        assert!(message(&findings, 0).contains("evidence row 1"));
        assert!(message(&findings, 1).contains("evidence row 2"));
        assert!(message(&findings, 2).contains("rows 3 and 4"));
        assert!(message(&findings, 2).contains(PROBE));
        assert!(message(&findings, 2).contains("must be unique"));
    });
}

#[test]
fn mutation_metrics_survive_malformed_payload() {
    let case = isolated_case();
    Python::attach(|py| {
        let (context, _anchor) = gate(py, case.root());
        let _verifier = patch_verifier(
            py,
            json!({"status":"FAIL","checked_test_paths":[PROBE],
            "evidence":null,"missing_evidence":{"path":"not-a-list"},"malformed_receipts":null}),
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
        let metrics = result.getattr("metrics").unwrap();
        assert_eq!(
            metrics
                .get_item("covered_tests")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            0
        );
        assert_eq!(
            metrics
                .get_item("missing_tests")
                .unwrap()
                .extract::<usize>()
                .unwrap(),
            0
        );
        assert_eq!(
            rule_ids(&result).into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "malformed-evidence-container".to_owned(),
                "test-value-receipt-unavailable".to_owned()
            ])
        );
    });
}

#[test]
fn matching_grandfather_inventory_loads_from_anchor() {
    let case = isolated_case();
    Python::attach(|py| {
        let rows = [(
            PROBE,
            &["test_probe_legacy", "TestShaped::test_method"] as &[&str],
        )];
        let (context, _anchor) = gate_context(py, case.root(), case.root(), &rows, &"c".repeat(40));
        let loaded = module(py, "conductor.candidate_review.verification")
            .getattr("_load_grandfathered_nodeids")
            .unwrap()
            .call1((context,))
            .unwrap();
        assert_eq!(loaded.len().unwrap(), 1);
        let labels = loaded.get_item(PROBE).unwrap();
        assert_eq!(labels.len().unwrap(), 2);
        assert!(labels.contains("test_probe_legacy").unwrap());
        assert!(labels.contains("TestShaped::test_method").unwrap());
    });
}

fn assert_grandfather_error(py: Python<'_>, context: &Bound<'_, PyAny>, fragment: &str) {
    let verification = module(py, "conductor.candidate_review.verification");
    let error = verification
        .getattr("_load_grandfathered_nodeids")
        .unwrap()
        .call1((context,))
        .unwrap_err();
    assert_error(
        py,
        error,
        &verification.getattr("_GrandfatherError").unwrap(),
        fragment,
    );
}

#[test]
fn missing_anchor_commit_fails_closed() {
    let case = isolated_case();
    Python::attach(|py| {
        let (context, _anchor) = gate(py, case.root());
        let verification = module(py, "conductor.candidate_review.verification");
        let value = pyo3::types::PyString::new(py, &"b".repeat(40));
        let _drift = AttrPatch::replace(
            verification.as_any(),
            "GRANDFATHER_ANCHOR_COMMIT_OID",
            value.as_any(),
        );
        assert_grandfather_error(py, &context, "cannot be proven from git");
    });
}

#[test]
fn anchor_tree_drift_fails_closed() {
    let case = isolated_case();
    Python::attach(|py| {
        let (context, _anchor) = gate(py, case.root());
        let verification = module(py, "conductor.candidate_review.verification");
        let value = pyo3::types::PyString::new(py, EMPTY_TREE);
        let _drift = AttrPatch::replace(
            verification.as_any(),
            "GRANDFATHER_ANCHOR_TREE_OID",
            value.as_any(),
        );
        assert_grandfather_error(py, &context, "tree drifted");
    });
}

#[test]
fn crafted_grandfather_inventory_fails_closed() {
    let case = isolated_case();
    Python::attach(|py| {
        let (context, _anchor) = gate(py, case.root());
        let crafted = crafted_grandfather_inventory(
            py,
            &[(PROBE, &["test_probe_legacy", "test_extra_not_real"])],
        );
        let snapshot: PathBuf = context.getattr("snapshot").unwrap().extract().unwrap();
        write_grandfather_inventory(py, &snapshot, Some(&crafted));
        let digest = format!("{:x}", Sha256::digest(crafted.as_bytes()));
        let value = pyo3::types::PyString::new(py, &digest);
        let verification = module(py, "conductor.candidate_review.verification");
        let _digest = AttrPatch::replace(
            verification.as_any(),
            "GRANDFATHER_INVENTORY_SHA256",
            value.as_any(),
        );
        assert_grandfather_error(py, &context, "does not match the inventory");
    });
}

#[test]
fn shipped_grandfather_inventory_has_expected_size() {
    let _case = isolated_case();
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../src/conductor/candidate_review/grandfathered_test_nodeids_61343f57.json");
    let payload: Value = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
    let tests = payload["tests"].as_object().unwrap();
    assert_eq!(tests.len(), 1026);
    assert_eq!(
        tests
            .values()
            .map(|labels| labels.as_array().unwrap().len())
            .sum::<usize>(),
        9202
    );
}

#[test]
fn dead_inventory_entries_are_pruned() {
    let case = isolated_case();
    Python::attach(|py| {
        let repo = case.root().join("anchor");
        let pruned = "research/tests/test_pruned_probe.py";
        let rows = [
            (PROBE, &["test_probe_legacy"] as &[&str]),
            (pruned, &["test_pruned_legacy"] as &[&str]),
        ];
        let (context, _anchor) = gate_context(py, case.root(), &repo, &rows, &"c".repeat(40));
        fs::remove_file(repo.join(pruned)).unwrap();
        let verification = module(py, "conductor.candidate_review.verification");
        let loaded = verification
            .getattr("_load_grandfathered_nodeids")
            .unwrap()
            .call1((&context,))
            .unwrap();
        assert_eq!(loaded.len().unwrap(), 1);
        assert!(loaded.contains(PROBE).unwrap());
        let value = pyo3::types::PyString::new(py, EMPTY_TREE);
        let _drift = AttrPatch::replace(
            verification.as_any(),
            "GRANDFATHER_ANCHOR_TREE_OID",
            value.as_any(),
        );
        assert_grandfather_error(py, &context, "tree drifted");
    });
}

#[test]
fn verifier_receives_snapshot_and_anchor_repo() {
    let case = isolated_case();
    Python::attach(|py| {
        let (context, _anchor) = gate(py, case.root());
        let _verifier = patch_verifier(
            py,
            json!({"status":"FAIL","checked_test_paths":[PROBE],
            "evidence":[],"missing_evidence":[{"path":PROBE,"reason":"no receipt"}],
            "malformed_receipts":[]}),
        );
        let verifier = module(py, "conductor.mutation_testing")
            .getattr("verify_evidence")
            .unwrap();
        let result = check(py, &context);
        let call = verifier.getattr("call_args").unwrap();
        let kwargs = call.getattr("kwargs").unwrap();
        assert!(kwargs
            .get_item("repo_root")
            .unwrap()
            .eq(context.getattr("snapshot").unwrap())
            .unwrap());
        assert!(kwargs
            .get_item("anchor_repo")
            .unwrap()
            .eq(context.getattr("repo").unwrap())
            .unwrap());
        assert_eq!(
            result
                .getattr("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "failed"
        );
    });
}

#[test]
fn tombstoned_lane_revival_stays_gated() {
    let case = isolated_case();
    Python::attach(|py| {
        let verification = module(py, "conductor.candidate_review.verification");
        let dead = verification.getattr("GRANDFATHER_DEAD_TEST_PATHS").unwrap();
        assert_eq!(dead.len().unwrap(), 12);
        let mut paths: Vec<String> = dead
            .try_iter()
            .unwrap()
            .map(|item| item.unwrap().extract().unwrap())
            .collect();
        paths.sort();
        let labels: Vec<String> = (0..paths.len()).map(|i| format!("test_dead_{i}")).collect();
        let mut rows: Vec<(&str, &[&str])> = Vec::new();
        let label_refs: Vec<Vec<&str>> = labels.iter().map(|label| vec![label.as_str()]).collect();
        for (path, refs) in paths.iter().zip(&label_refs) {
            rows.push((path, refs));
        }
        rows.push((PROBE, &["test_probe_legacy"]));
        let (context, _anchor) = gate_context(py, case.root(), case.root(), &rows, &"c".repeat(40));
        fs::write(
            case.root()
                .join("research/tests/test_nm_f6_phase22_chinchilla.py"),
            "def test_revived_recreated_def():\n    assert True\n",
        )
        .unwrap();
        let loaded = verification
            .getattr("_load_grandfathered_nodeids")
            .unwrap()
            .call1((context,))
            .unwrap();
        assert_eq!(loaded.len().unwrap(), 1);
        assert!(loaded.contains(PROBE).unwrap());
    });
}

#[test]
fn value_inventory_covers_post_anchor_definitions_when_registered() {
    let _case = isolated_case();
    Python::attach(|py| {
        let project_paths = module(py, "conductor.project_paths");
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pyproject.toml");
        let root: PathBuf = project_paths
            .getattr("host_root")
            .unwrap()
            .call1((path(py, &source),))
            .unwrap()
            .extract()
            .unwrap();
        let registry_path: PathBuf = project_paths
            .getattr("registry_path")
            .unwrap()
            .call1((path(py, &root),))
            .unwrap()
            .extract()
            .unwrap();
        let registry: Value = serde_json::from_slice(&fs::read(registry_path).unwrap()).unwrap();
        let manifests: Vec<Value> = registry["campaigns"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| {
                let relative = row["manifest"].as_str().unwrap();
                let manifest: Value =
                    serde_json::from_slice(&fs::read(root.join(relative)).unwrap()).unwrap();
                (manifest.get("ranked_tests").is_some() && manifest.get("test_scopes").is_some())
                    .then_some(manifest)
            })
            .collect();
        if manifests.is_empty() {
            return;
        }
        let verification = module(py, "conductor.candidate_review.verification");
        let relative: String = verification
            .getattr("GRANDFATHER_INVENTORY_RELPATH")
            .unwrap()
            .extract()
            .unwrap();
        let inventory: Value =
            serde_json::from_slice(&fs::read(root.join("src").join(relative)).unwrap()).unwrap();
        let mut grandfathered = BTreeSet::new();
        for (file, labels) in inventory["tests"].as_object().unwrap() {
            for label in labels.as_array().unwrap() {
                grandfathered.insert(format!("{file}::{}", label.as_str().unwrap()));
            }
        }
        let mut scoped = BTreeSet::new();
        let mut post_anchor = BTreeSet::new();
        let mut ranked = Vec::new();
        for manifest in &manifests {
            for file in manifest["test_scopes"].as_array().unwrap() {
                let file = file.as_str().unwrap();
                assert!(
                    scoped.insert(file.to_owned()),
                    "duplicate campaign scope: {file}"
                );
                let source = fs::read_to_string(root.join(file)).unwrap();
                let labels = verification
                    .getattr("_python_test_labels")
                    .unwrap()
                    .call1((source, file))
                    .unwrap();
                for label in labels.try_iter().unwrap() {
                    let nodeid = format!("{file}::{}", label.unwrap().extract::<String>().unwrap());
                    if !grandfathered.contains(&nodeid) {
                        post_anchor.insert(nodeid);
                    }
                }
            }
            for row in manifest["ranked_tests"].as_array().unwrap() {
                ranked.push(row["nodeid"].as_str().unwrap().to_owned());
            }
        }
        assert_eq!(ranked.len(), ranked.iter().collect::<BTreeSet<_>>().len());
        assert_eq!(
            ranked
                .into_iter()
                .filter(|nodeid| !grandfathered.contains(nodeid))
                .collect::<BTreeSet<_>>(),
            post_anchor
        );
    });
}
