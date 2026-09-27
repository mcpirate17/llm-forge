#![cfg(feature = "python-compat-tests")]
//! Current and legacy mutation-receipt evidence contracts in Rust.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/mutation_testing_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::py_json;
use fixture::{
    constant, equal, git, pass_receipt, py_expected, registry, temporary_campaign, testing, verify,
};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict, PyList, PyString};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, AttrPatch, Case};

fn evidence_with_pass<'py, F>(
    py: Python<'py>,
    case: &Case,
    campaign: Option<Bound<'py, PyAny>>,
    test_path: &str,
    mutate: F,
) -> Bound<'py, PyAny>
where
    F: FnOnce(&Path),
{
    let campaign = campaign.unwrap_or_else(|| temporary_campaign(py, case));
    let reg = registry(py, case);
    let subject = testing(py);
    let load = constant(py, &campaign);
    let _patch = AttrPatch::replace(&subject, "load_campaign", load.as_any());
    let receipt = pass_receipt(py, case, &campaign);
    mutate(&receipt);
    verify(py, case, &reg, &[test_path])
}

fn patch_receipt_field(path: &Path, name: &str, replacement: Option<Value>) {
    let mut payload: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    match replacement {
        Some(value) => {
            payload[name] = value;
        }
        None => {
            payload.as_object_mut().unwrap().remove(name);
        }
    }
    fs::write(path, payload.to_string()).unwrap();
}

struct Anchored {
    receipt: PathBuf,
    payload: Value,
    _commit_patch: AttrPatch,
    _tree_patch: AttrPatch,
}

fn anchored(
    py: Python<'_>,
    case: &Case,
    campaign: &Bound<'_, PyAny>,
    registered: bool,
) -> Anchored {
    let subject = testing(py);
    let receipt = pass_receipt(py, case, campaign);
    let mut payload: Value = serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
    payload["schema_version"] = json!(subject
        .getattr("LEGACY_RECEIPT_SCHEMA")
        .unwrap()
        .extract::<String>()
        .unwrap());
    payload["runner_sha256"] = json!("legacy-runner");
    payload
        .as_object_mut()
        .unwrap()
        .remove("runner_components_sha256");
    let legacy = case
        .root()
        .join("conductor/mutation_campaigns/receipts/legacy_campaign_20260827.json");
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(&legacy, payload.to_string()).unwrap();
    let anchor_registry = case
        .root()
        .join("conductor/mutation_campaigns/registry.json");
    let patterns: Vec<String> = subject
        .getattr("CANONICAL_TEST_PATTERNS")
        .unwrap()
        .extract()
        .unwrap();
    let campaigns = if registered {
        json!([{"manifest":"campaign.json"}])
    } else {
        json!([])
    };
    fs::write(&anchor_registry, json!({"schema_version":1,"enforcement":"changed_tests","test_patterns":patterns,"receipt_directories":["conductor/mutation_campaigns/receipts"],"campaigns":campaigns}).to_string()).unwrap();
    git(case.root(), &["init", "--quiet"]);
    git(case.root(), &["config", "user.name", "Mutation Test"]);
    git(
        case.root(),
        &["config", "user.email", "mutation@example.invalid"],
    );
    git(
        case.root(),
        &[
            "add",
            "--",
            "conductor/mutation_campaigns/receipts/legacy_campaign_20260827.json",
            "conductor/mutation_campaigns/registry.json",
            "campaign.json",
        ],
    );
    git(
        case.root(),
        &["commit", "--quiet", "-m", "legacy receipt anchor"],
    );
    let commit = git(case.root(), &["rev-parse", "HEAD"]);
    let tree = git(case.root(), &["rev-parse", "HEAD^{tree}"]);
    let commit_patch = AttrPatch::replace(
        &subject,
        "LEGACY_RECEIPT_ANCHOR_COMMIT",
        PyString::new(py, &commit).as_any(),
    );
    let tree_patch = AttrPatch::replace(
        &subject,
        "LEGACY_RECEIPT_ANCHOR_TREE",
        PyString::new(py, &tree).as_any(),
    );
    Anchored {
        receipt: legacy,
        payload,
        _commit_patch: commit_patch,
        _tree_patch: tree_patch,
    }
}

fn receipt_errors<'py>(
    py: Python<'py>,
    payload: &Value,
    campaign: &Bound<'py, PyAny>,
    root: &Path,
    receipt: Option<&Path>,
    raw: Option<&[u8]>,
    anchor_root: &Path,
) -> Bound<'py, PyAny> {
    let location = receipt
        .map(|file| path(py, file).unbind())
        .unwrap_or_else(|| py.None());
    let bytes = raw
        .map(|value| PyBytes::new(py, value).into_any().unbind())
        .unwrap_or_else(|| py.None());
    testing(py)
        .getattr("_receipt_errors")
        .unwrap()
        .call1((
            py_json(py, payload.clone()),
            campaign,
            path(py, root),
            location,
            bytes,
            path(py, anchor_root),
        ))
        .unwrap()
}

#[test]
fn test_mandatory_evidence_accepts_current_complete_receipts() {
    let case = Case::new();
    Python::attach(|py| {
        let result = evidence_with_pass(py, &case, None, "test_one.py", |_| {});
        assert_eq!(
            result
                .get_item("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "PASS"
        );
        assert_eq!(result.get_item("evidence").unwrap().len().unwrap(), 1);
        equal(
            &result.get_item("missing_evidence").unwrap(),
            &PyList::empty(py),
        );
    });
}

#[test]
fn test_mandatory_evidence_rejects_non_utf8_receipts() {
    let case = Case::new();
    Python::attach(|py| {
        let result = evidence_with_pass(py, &case, None, "test_one.py", |receipt| {
            let text = fs::read_to_string(receipt).unwrap();
            let utf16: Vec<u8> = PyString::new(py, &text)
                .call_method1("encode", ("utf-16",))
                .unwrap()
                .extract()
                .unwrap();
            fs::write(receipt, utf16).unwrap();
        });
        assert_eq!(
            result
                .get_item("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "FAIL"
        );
        equal(&result.get_item("evidence").unwrap(), &PyList::empty(py));
        let malformed = result.get_item("malformed_receipts").unwrap();
        assert_eq!(malformed.len().unwrap(), 1);
        assert!(malformed
            .get_item(0)
            .unwrap()
            .extract::<String>()
            .unwrap()
            .contains("utf-8"));
    });
}

#[test]
fn test_legacy_receipt_requires_exact_git_anchored_path_and_bytes() {
    let case = Case::new();
    Python::attach(|py| {
        let campaign = temporary_campaign(py, &case);
        let anchored = anchored(py, &case, &campaign, true);
        let raw = fs::read(&anchored.receipt).unwrap();
        equal(
            &receipt_errors(
                py,
                &anchored.payload,
                &campaign,
                case.root(),
                Some(&anchored.receipt),
                Some(&raw),
                case.root(),
            ),
            &PyList::empty(py),
        );
        let text = fs::read_to_string(&anchored.receipt).unwrap();
        fs::write(&anchored.receipt, format!("{text}\n")).unwrap();
        equal(
            &receipt_errors(
                py,
                &anchored.payload,
                &campaign,
                case.root(),
                Some(&anchored.receipt),
                Some(&raw),
                case.root(),
            ),
            &PyList::empty(py),
        );
        let changed = fs::read(&anchored.receipt).unwrap();
        let errors = receipt_errors(
            py,
            &anchored.payload,
            &campaign,
            case.root(),
            Some(&anchored.receipt),
            Some(&changed),
            case.root(),
        );
        assert!(errors
            .contains("legacy receipt parsed bytes differ from the anchor")
            .unwrap());
        fs::write(&anchored.receipt, &raw).unwrap();
        let alias = anchored.receipt.parent().unwrap().join("alias");
        std::os::unix::fs::symlink(anchored.receipt.parent().unwrap(), &alias).unwrap();
        let aliased = alias.join(anchored.receipt.file_name().unwrap());
        let errors = receipt_errors(
            py,
            &anchored.payload,
            &campaign,
            case.root(),
            Some(&aliased),
            Some(&raw),
            case.root(),
        );
        assert!(errors
            .contains("legacy receipt path has a symlink component")
            .unwrap());
    });
}

fn anchor_failure(case_name: &str) {
    let case = Case::new();
    Python::attach(|py| {
        let campaign = temporary_campaign(py, &case);
        let anchored = anchored(py, &case, &campaign, case_name != "unregistered");
        let mut receipt = Some(anchored.receipt.clone());
        let mut raw = fs::read(&anchored.receipt).unwrap();
        let mut anchor_root = case.root().to_path_buf();
        let expected = match case_name {
            "missing_path" => {
                receipt = None;
                "legacy receipt path or parsed bytes are unavailable"
            }
            "wrong_tree" => {
                let subject = testing(py);
                let _patch = AttrPatch::replace(
                    &subject,
                    "LEGACY_RECEIPT_ANCHOR_TREE",
                    PyString::new(py, &"0".repeat(40)).as_any(),
                );
                let errors = receipt_errors(
                    py,
                    &anchored.payload,
                    &campaign,
                    case.root(),
                    receipt.as_deref(),
                    Some(&raw),
                    case.root(),
                );
                assert!(errors
                    .contains("legacy receipt anchor tree mismatch")
                    .unwrap());
                return;
            }
            "outside_prefix" => {
                let outside = case.root().join("receipts/legacy.json");
                fs::create_dir_all(outside.parent().unwrap()).unwrap();
                fs::write(&outside, anchored.payload.to_string()).unwrap();
                raw = fs::read(&outside).unwrap();
                receipt = Some(outside);
                "legacy receipt path is outside the anchored receipt directory"
            }
            "wrong_repo" => {
                anchor_root = case.mkdir("not-a-repository");
                "legacy receipt anchor repository is unavailable"
            }
            "unregistered" => "legacy receipt campaign was not registered at the anchor",
            "replace_ref" => {
                raw.push(b'\n');
                fs::write(&anchored.receipt, &raw).unwrap();
                git(case.root(), &["add", "--", "."]);
                let tree = git(case.root(), &["write-tree"]);
                let original: String = testing(py)
                    .getattr("LEGACY_RECEIPT_ANCHOR_TREE")
                    .unwrap()
                    .extract()
                    .unwrap();
                git(case.root(), &["replace", &original, &tree]);
                "legacy receipt parsed bytes differ from the anchor"
            }
            _ => panic!("unknown row {case_name}"),
        };
        let errors = receipt_errors(
            py,
            &anchored.payload,
            &campaign,
            case.root(),
            receipt.as_deref(),
            receipt.as_ref().map(|_| raw.as_slice()),
            &anchor_root,
        );
        assert!(
            errors.contains(expected).unwrap(),
            "missing {expected}: {errors:?}"
        );
    });
}

#[test]
fn test_legacy_receipt_anchor_fails_closed() {
    for case_name in [
        "missing_path",
        "wrong_tree",
        "outside_prefix",
        "wrong_repo",
        "unregistered",
        "replace_ref",
    ] {
        anchor_failure(case_name);
    }
}

#[test]
fn test_mandatory_evidence_rejects_runner_provenance_drift() {
    for (field, replacement, expected) in [
        (
            "runner_sha256",
            Some(json!("0".repeat(64))),
            "runner hash mismatch",
        ),
        (
            "runner_components_sha256",
            None,
            "runner component hash map mismatch",
        ),
        (
            "runner_components_sha256",
            Some(json!({})),
            "runner component hash map mismatch",
        ),
        (
            "runner_components_sha256",
            Some(json!([])),
            "runner component hash map mismatch",
        ),
    ] {
        let case = Case::new();
        Python::attach(|py| {
            let result = evidence_with_pass(py, &case, None, "test_one.py", |receipt| {
                patch_receipt_field(receipt, field, replacement)
            });
            assert_eq!(
                result
                    .get_item("status")
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "FAIL"
            );
            let detail: String = result
                .get_item("missing_evidence")
                .unwrap()
                .get_item(0)
                .unwrap()
                .get_item("receipt_rejections")
                .unwrap()
                .get_item(0)
                .unwrap()
                .get_item("detail")
                .unwrap()
                .extract()
                .unwrap();
            assert!(detail.contains(expected), "{detail}");
        });
    }
}

#[test]
fn test_mandatory_evidence_rejects_legacy_file_scope() {
    let case = Case::new();
    Python::attach(|py| {
        let campaign = temporary_campaign(py, &case);
        let kw = PyDict::new(py);
        kw.set_item("test_scopes", PyDict::new(py)).unwrap();
        let replaced = module(py, "dataclasses")
            .getattr("replace")
            .unwrap()
            .call((&campaign,), Some(&kw))
            .unwrap();
        let result = evidence_with_pass(py, &case, Some(replaced), "test_one.py", |_| {});
        assert_eq!(
            result
                .get_item("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "FAIL"
        );
        let row = result
            .get_item("missing_evidence")
            .unwrap()
            .get_item(0)
            .unwrap();
        assert_eq!(
            row.get_item("reason").unwrap().extract::<String>().unwrap(),
            "no current complete PASS receipt"
        );
        equal(
            &row.get_item("receipt_rejections").unwrap(),
            &py_expected(
                py,
                json!([{"receipt":"temporary_campaign","kind":"scope_error","detail":"campaign lacks explicit test scope"}]),
            ),
        );
    });
}

#[test]
fn test_mandatory_evidence_rejects_unregistered_changed_test() {
    let case = Case::new();
    Python::attach(|py| {
        let campaign = temporary_campaign(py, &case);
        let reg = registry(py, &case);
        let subject = testing(py);
        let callback = constant(py, &campaign);
        let _patch = AttrPatch::replace(&subject, "load_campaign", callback.as_any());
        let result = verify(py, &case, &reg, &["example/tests/test_unregistered.py"]);
        assert_eq!(
            result
                .get_item("status")
                .unwrap()
                .extract::<String>()
                .unwrap(),
            "FAIL"
        );
        equal(
            &result.get_item("missing_evidence").unwrap(),
            &py_expected(
                py,
                json!([{"path":"example/tests/test_unregistered.py","reason":"no registered campaign ranks this test file","reason_kind":"no_campaign","campaigns":[],"receipt_rejections":[]}]),
            ),
        );
        let mut narrow: Value = serde_json::from_slice(&fs::read(&reg).unwrap()).unwrap();
        narrow["test_patterns"] = json!(["never-a-test"]);
        fs::write(&reg, narrow.to_string()).unwrap();
        let kw = PyDict::new(py);
        kw.set_item("repo_root", path(py, case.root())).unwrap();
        let result = subject.getattr("verify_evidence").unwrap().call(
            (
                path(py, &reg),
                PyList::new(py, ["example/tests/test_unregistered.py"]).unwrap(),
            ),
            Some(&kw),
        );
        assert_error(
            py,
            result.unwrap_err(),
            &subject.getattr("CampaignError").unwrap(),
            "canonical inventory",
        );
    });
}
