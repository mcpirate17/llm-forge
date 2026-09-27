#![cfg(feature = "python-compat-tests")]
//! Rust-owned retention contracts over synthetic campaigns and receipts.

#[path = "python_contracts/mutation_retention_support.rs"]
mod retention_support;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::{PyAny, PyCFunction, PySet};
use retention_support::{
    audit_accepts, capture, cites, fixture, json_to_py, manifest, no_citations, output, path_set,
    plan, py_to_json, receipt, retention, write_json, EARLY, LATE,
};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use support::{assert_error, module, path, AttrPatch, Case};

fn deleted(value: &Bound<'_, PyAny>) -> Vec<PathBuf> {
    path_set(&value.getattr("delete").unwrap())
}

fn kept<'py>(py: Python<'py>, value: &Bound<'py, PyAny>, file: &Path) -> Bound<'py, PyAny> {
    value
        .getattr("keep")
        .unwrap()
        .get_item(path(py, file))
        .unwrap()
}

fn retention_error(py: Python<'_>, err: PyErr, message: &str) {
    assert_error(
        py,
        err,
        &retention(py).getattr("RetentionError").unwrap(),
        message,
    );
}

fn pair(py: Python<'_>, case: &Case, id: &str) -> (PathBuf, PathBuf) {
    manifest(py, case.root(), id, None);
    let old = receipt(py, case.root(), &format!("{id}_old"), id, "PASS", EARLY, 0);
    let new = receipt(py, case.root(), &format!("{id}_new"), id, "PASS", LATE, 0);
    (old, new)
}

#[test]
fn superseded_pass_receipts_are_swept_and_newest_survives() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        let (old, new) = pair(py, &case, "alpha");
        let _patches = no_citations(py);
        let result = plan(py, case.root(), &[]).unwrap();
        assert_eq!(deleted(&result), [old]);
        assert_eq!(
            kept(py, &result, &new).extract::<String>().unwrap(),
            "newest PASS for alpha"
        );
    });
}

#[test]
fn newest_pass_uses_gate_ordering_not_filename() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", None);
        let newest = receipt(py, case.root(), "alpha_a", "alpha", "PASS", LATE, 0);
        let older = receipt(py, case.root(), "alpha_z", "alpha", "PASS", EARLY, 0);
        let _patches = no_citations(py);
        let result = plan(py, case.root(), &[]).unwrap();
        assert_eq!(deleted(&result), [older]);
        assert!(path_set(&result.getattr("keep").unwrap()).contains(&newest));
    });
}

#[test]
fn cited_receipt_survives_newer_pass() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        let (cited, newer) = pair(py, &case, "alpha");
        let _patches = cites(py, std::slice::from_ref(&cited));
        let result = plan(py, case.root(), &[]).unwrap();
        assert!(deleted(&result).is_empty());
        assert_eq!(
            kept(py, &result, &cited).extract::<String>().unwrap(),
            "read by the coverage gate or the corpus audit"
        );
        assert!(path_set(&result.getattr("keep").unwrap()).contains(&newer));
    });
}

#[test]
fn citation_outranks_missing_manifest() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        let cited = receipt(py, case.root(), "ghost", "ghost", "PASS", EARLY, 0);
        let _patches = cites(py, std::slice::from_ref(&cited));
        let result = plan(py, case.root(), &[]).unwrap();
        assert!(deleted(&result).is_empty());
        assert!(path_set(&result.getattr("keep").unwrap()).contains(&cited));
    });
}

#[test]
fn receipts_without_manifest_are_swept() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        let orphan = receipt(py, case.root(), "ghost", "ghost", "PASS", EARLY, 0);
        let kept_file = receipt(py, case.root(), "alpha", "alpha", "PASS", EARLY, 0);
        manifest(py, case.root(), "alpha", None);
        let _patches = no_citations(py);
        let result = plan(py, case.root(), &[]).unwrap();
        assert_eq!(deleted(&result), [orphan]);
        assert!(path_set(&result.getattr("keep").unwrap()).contains(&kept_file));
    });
}

#[test]
fn manifest_is_matched_by_campaign_id_not_filename() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", Some("2026-09-07-alpha-rerun"));
        let kept_file = receipt(py, case.root(), "alpha", "alpha", "PASS", EARLY, 0);
        let _patches = no_citations(py);
        let result = plan(py, case.root(), &[]).unwrap();
        assert!(deleted(&result).is_empty());
        assert!(path_set(&result.getattr("keep").unwrap()).contains(&kept_file));
    });
}

#[test]
fn broken_registry_does_not_refuse_sweep() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        let (old, _) = pair(py, &case, "alpha");
        fs::write(
            case.root()
                .join("conductor/mutation_campaigns/registry.json"),
            "{ not json",
        )
        .unwrap();
        let _patches = no_citations(py);
        assert_eq!(deleted(&plan(py, case.root(), &[]).unwrap()), [old]);
    });
}

#[test]
fn missing_campaign_directory_refuses_sweep() {
    let case = Case::new();
    Python::attach(|py| {
        retention_error(
            py,
            plan(py, case.root(), &[]).unwrap_err(),
            "no campaign directory",
        )
    });
}

#[test]
fn campaign_with_no_pass_keeps_every_receipt() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", None);
        let first = receipt(py, case.root(), "alpha_a", "alpha", "FAIL", EARLY, 0);
        let second = receipt(py, case.root(), "alpha_b", "alpha", "ERROR", LATE, 0);
        let _patches = no_citations(py);
        let result = plan(py, case.root(), &[]).unwrap();
        assert!(deleted(&result).is_empty());
        let keep = path_set(&result.getattr("keep").unwrap());
        assert!(keep.contains(&first) && keep.contains(&second));
    });
}

#[test]
fn failing_receipt_is_swept_after_pass() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", None);
        let failed = receipt(py, case.root(), "alpha_a", "alpha", "FAIL", EARLY, 0);
        let passed = receipt(py, case.root(), "alpha_b", "alpha", "PASS", LATE, 0);
        let _patches = no_citations(py);
        let result = plan(py, case.root(), &[]).unwrap();
        assert_eq!(deleted(&result), [failed]);
        assert!(path_set(&result.getattr("keep").unwrap()).contains(&passed));
    });
}

#[test]
fn unparseable_receipt_is_kept_not_swept() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", None);
        receipt(py, case.root(), "alpha_new", "alpha", "PASS", LATE, 0);
        let broken = case
            .root()
            .join("conductor/mutation_campaigns/receipts/broken.json");
        fs::write(&broken, "{ not json").unwrap();
        let _patches = no_citations(py);
        let result = plan(py, case.root(), &[]).unwrap();
        assert!(!deleted(&result).contains(&broken));
        assert!(path_set(&result.getattr("keep").unwrap()).contains(&broken));
        assert_eq!(path_set(&result.getattr("unreadable").unwrap()), [broken]);
    });
}

#[test]
fn receipt_without_campaign_id_is_kept_not_swept() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", None);
        receipt(py, case.root(), "alpha_new", "alpha", "PASS", LATE, 0);
        let anonymous = write_json(
            &case
                .root()
                .join("conductor/mutation_campaigns/receipts/anonymous.json"),
            &json!({"status":"PASS","generated_at":LATE}),
        );
        let _patches = no_citations(py);
        let result = plan(py, case.root(), &[]).unwrap();
        assert!(!deleted(&result).contains(&anonymous));
        assert_eq!(
            path_set(&result.getattr("unreadable").unwrap()),
            [anonymous]
        );
    });
}

#[test]
fn gate_failure_refuses_sweep_and_preserves_files() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        let (old, _) = pair(py, &case, "alpha");
        let fail = PyCFunction::new_closure(py, None, None, |args, _| -> PyResult<Py<PyAny>> {
            let py = args.py();
            let class = retention(py).getattr("RetentionError")?;
            Err(PyErr::from_value(
                class.call1(("coverage gate did not run: boom",))?,
            ))
        })
        .unwrap();
        let _patch = AttrPatch::replace(&retention(py), "cited_receipts", fail.as_any());
        retention_error(
            py,
            plan(py, case.root(), &[]).unwrap_err(),
            "coverage gate did not run",
        );
        assert!(old.exists());
    });
}

#[test]
fn unreadable_manifest_refuses_sweep_and_preserves_receipt() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        let file = manifest(py, case.root(), "alpha", None);
        let ghost = receipt(py, case.root(), "ghost", "ghost", "PASS", EARLY, 0);
        fs::write(file, "{").unwrap();
        let _patches = no_citations(py);
        retention_error(
            py,
            plan(py, case.root(), &[]).unwrap_err(),
            "manifest alpha.json is unreadable",
        );
        assert!(ghost.exists());
    });
}

#[test]
fn missing_receipt_directory_refuses_sweep() {
    let case = Case::new();
    Python::attach(|py| {
        let relative: String = retention(py)
            .getattr("CAMPAIGN_DIRECTORY")
            .unwrap()
            .extract()
            .unwrap();
        fs::create_dir_all(case.root().join(relative)).unwrap();
        retention_error(
            py,
            plan(py, case.root(), &[]).unwrap_err(),
            "no receipt directory",
        );
    });
}

#[test]
fn protect_overrides_deletion_rule() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        let (old, _) = pair(py, &case, "alpha");
        let _patches = no_citations(py);
        let result = plan(
            py,
            case.root(),
            &[old.file_name().unwrap().to_str().unwrap()],
        )
        .unwrap();
        assert!(deleted(&result).is_empty());
        assert_eq!(
            kept(py, &result, &old).extract::<String>().unwrap(),
            "explicitly protected"
        );
    });
}

#[test]
fn plan_touches_nothing_and_apply_removes_exactly_planned_files() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", None);
        let old = receipt(py, case.root(), "alpha_old", "alpha", "PASS", EARLY, 0);
        let new = receipt(py, case.root(), "alpha_new", "alpha", "PASS", LATE, 512);
        let _patches = no_citations(py);
        let result = plan(py, case.root(), &[]).unwrap();
        assert!(old.exists());
        let freed: u64 = result.getattr("freed_bytes").unwrap().extract().unwrap();
        assert_eq!(freed, fs::metadata(&old).unwrap().len());
        assert_eq!(
            retention(py)
                .getattr("apply")
                .unwrap()
                .call1((&result,))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            1
        );
        assert!(!old.exists() && new.exists());
    });
}

#[test]
fn cli_reports_plan_then_applies_only_when_told() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", None);
        let old = receipt(py, case.root(), "alpha_old", "alpha", "PASS", EARLY, 0);
        receipt(py, case.root(), "alpha_new", "alpha", "PASS", LATE, 512);
        let _patches = no_citations(py);
        let (_capture, stdout) = capture(py, "stdout");
        let args = vec!["--repo-root", case.root().to_str().unwrap()];
        assert_eq!(
            retention(py)
                .getattr("main")
                .unwrap()
                .call1((args.clone(),))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        let first: Value = serde_json::from_str(&output(py, &stdout)).unwrap();
        assert_eq!(first["deleted"], 1);
        assert_eq!(first["kept"], 1);
        assert_eq!(first["freed_bytes"], fs::metadata(&old).unwrap().len());
        assert!(first.get("removed").is_none() && old.exists());
        stdout.bind(py).call_method1("seek", (0,)).unwrap();
        stdout.bind(py).call_method1("truncate", (0,)).unwrap();
        let mut apply = args;
        apply.push("--apply");
        assert_eq!(
            retention(py)
                .getattr("main")
                .unwrap()
                .call1((apply,))
                .unwrap()
                .extract::<i32>()
                .unwrap(),
            0
        );
        let second: Value = serde_json::from_str(&output(py, &stdout)).unwrap();
        assert_eq!(second["removed"], 1);
        assert!(!old.exists());
    });
}

#[test]
fn cli_reports_undecidable_corpus_as_exit_two() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        let manifest_file = manifest(py, case.root(), "alpha", None);
        let old = receipt(py, case.root(), "alpha_old", "alpha", "PASS", EARLY, 0);
        fs::write(manifest_file, "{").unwrap();
        let (_capture, stderr) = capture(py, "stderr");
        let code: i32 = retention(py)
            .getattr("main")
            .unwrap()
            .call1((vec!["--repo-root", case.root().to_str().unwrap()],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 2);
        assert!(output(py, &stderr).contains("alpha.json is unreadable"));
        assert!(old.exists());
    });
}

#[test]
fn cited_receipts_uses_receipt_field_of_every_evidence_row() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        let relative = "conductor/mutation_campaigns/receipts/alpha.json";
        let callback =
            PyCFunction::new_closure(py, None, None, move |args, _| -> PyResult<Py<PyAny>> {
                Ok(json_to_py(
                    args.py(),
                    &json!({"evidence":[{"path":"conductor/test_alpha.py","receipt":relative}]}),
                )
                .unbind())
            })
            .unwrap();
        let coverage = module(py, "conductor.mutation_coverage");
        let _patch = AttrPatch::replace(coverage.as_any(), "coverage_report", callback.as_any());
        let citations = retention(py)
            .getattr("cited_receipts")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap();
        assert_eq!(path_set(&citations), [case.root().join(relative)]);
    });
}

#[test]
fn coverage_gate_exception_becomes_retention_error() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        let callback = PyCFunction::new_closure(py, None, None, |_, _| -> PyResult<Py<PyAny>> {
            Err(pyo3::exceptions::PyRuntimeError::new_err(
                "registry is unreadable",
            ))
        })
        .unwrap();
        let coverage = module(py, "conductor.mutation_coverage");
        let _patch = AttrPatch::replace(coverage.as_any(), "coverage_report", callback.as_any());
        let err = retention(py)
            .getattr("cited_receipts")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap_err();
        retention_error(py, err, "coverage gate did not run");
    });
}

#[test]
fn evidence_row_without_receipt_path_refuses() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        let callback = PyCFunction::new_closure(py, None, None, |args, _| -> PyResult<Py<PyAny>> {
            Ok(json_to_py(
                args.py(),
                &json!({"evidence":[{"path":"conductor/test_alpha.py"}]}),
            )
            .unbind())
        })
        .unwrap();
        let coverage = module(py, "conductor.mutation_coverage");
        let _patch = AttrPatch::replace(coverage.as_any(), "coverage_report", callback.as_any());
        let err = retention(py)
            .getattr("cited_receipts")
            .unwrap()
            .call1((path(py, case.root()),))
            .unwrap_err();
        retention_error(py, err, "without a receipt path");
    });
}

#[test]
fn receipt_read_by_corpus_audit_survives_newer_pass() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", None);
        let old = receipt(py, case.root(), "alpha_old", "alpha", "PASS", EARLY, 0);
        let new = receipt(
            py,
            case.root(),
            "alpha_new",
            "alpha",
            "PASS",
            "2026-09-02T00:00:00+00:00",
            0,
        );
        let empty = PyCFunction::new_closure(py, None, None, |args, _| -> PyResult<Py<PyAny>> {
            Ok(PySet::empty(args.py())?.into_any().unbind())
        })
        .unwrap();
        let m = retention(py);
        let _citation_patch = AttrPatch::replace(&m, "cited_receipts", empty.as_any());
        let _audit_patch = audit_accepts(py, &["alpha_old"]);
        let result = plan(py, case.root(), &[]).unwrap();
        let keep = path_set(&result.getattr("keep").unwrap());
        assert!(keep.contains(&old) && keep.contains(&new));
        assert!(!deleted(&result).contains(&old));
    });
}

fn audited(py: Python<'_>, root: &Path) -> Vec<PathBuf> {
    let m = retention(py);
    let directory: String = m.getattr("RECEIPT_DIRECTORY").unwrap().extract().unwrap();
    let rows = m
        .getattr("_load_receipts")
        .unwrap()
        .call1((path(py, &root.join(directory)),))
        .unwrap()
        .get_item(0)
        .unwrap();
    path_set(
        &m.getattr("audited_receipts")
            .unwrap()
            .call1((rows, path(py, root)))
            .unwrap(),
    )
}

#[test]
fn audit_keeps_only_receipt_it_would_read() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", None);
        let stale = receipt(py, case.root(), "alpha_a", "alpha", "PASS", EARLY, 0);
        let first = receipt(
            py,
            case.root(),
            "alpha_b",
            "alpha",
            "PASS",
            "2026-09-02T00:00:00+00:00",
            0,
        );
        let second = receipt(
            py,
            case.root(),
            "alpha_c",
            "alpha",
            "PASS",
            "2026-09-02T00:00:00+00:00",
            0,
        );
        let _patch = audit_accepts(py, &["alpha_a", "alpha_b", "alpha_c"]);
        let read = audited(py, case.root());
        assert_eq!(read, [first]);
        assert!(!read.contains(&stale) && !read.contains(&second));
    });
}

#[test]
fn receipt_rejected_by_audit_is_never_read() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", None);
        receipt(py, case.root(), "alpha_only", "alpha", "PASS", EARLY, 0);
        let _patch = audit_accepts(py, &[]);
        assert!(audited(py, case.root()).is_empty());
    });
}

#[test]
fn audit_runner_failure_refuses_sweep() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", None);
        let ghost = receipt(py, case.root(), "ghost", "alpha", "PASS", EARLY, 0);
        let empty = PyCFunction::new_closure(py, None, None, |args, _| -> PyResult<Py<PyAny>> {
            Ok(PySet::empty(args.py())?.into_any().unbind())
        })
        .unwrap();
        let _cited = AttrPatch::replace(&retention(py), "cited_receipts", empty.as_any());
        let fail = PyCFunction::new_closure(py, None, None, |_, _| -> PyResult<Py<PyAny>> {
            Err(pyo3::exceptions::PyRuntimeError::new_err(
                "runner components unreadable",
            ))
        })
        .unwrap();
        let audit = module(py, "conductor.mutation_patch_audit");
        let _hashes =
            AttrPatch::replace(audit.as_any(), "_runner_components_sha256", fail.as_any());
        retention_error(
            py,
            plan(py, case.root(), &[]).unwrap_err(),
            "corpus audit did not run",
        );
        assert!(ghost.exists());
    });
}

#[test]
fn audit_predicate_seam_has_production_signature() {
    let _case = Case::new();
    Python::attach(|py| {
        let judge = module(py, "conductor.mutation_patch_audit")
            .getattr("ReceiptJudge")
            .unwrap();
        let signature = module(py, "inspect")
            .getattr("signature")
            .unwrap()
            .call1((judge.getattr("rejection").unwrap(),))
            .unwrap();
        let names: Vec<String> = signature
            .getattr("parameters")
            .unwrap()
            .call_method0("keys")
            .unwrap()
            .try_iter()
            .unwrap()
            .map(|item| item.unwrap().extract::<String>().unwrap())
            .collect();
        assert_eq!(names, ["self", "receipt", "campaign"]);
        let _patch = audit_accepts(py, &["accepted"]);
        let double = judge.getattr("rejection").unwrap();
        assert!(double
            .call1((json_to_py(py, &json!({"name":"accepted"})), py.None()))
            .unwrap()
            .is_none());
        assert_error(
            py,
            double
                .call1((
                    json_to_py(py, &json!({"name":"accepted"})),
                    py.None(),
                    py.None(),
                ))
                .unwrap_err(),
            &module(py, "builtins").getattr("TypeError").unwrap(),
            "expected receipt, campaign",
        );
    });
}

#[test]
fn audited_receipts_uses_real_manifest_and_rejection_predicate() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", None);
        let audit = module(py, "conductor.mutation_patch_audit");
        let hashes = PyCFunction::new_closure(py, None, None, |args, _| -> PyResult<Py<PyAny>> {
            Ok(json_to_py(args.py(), &json!({"runner":"sha"})).unbind())
        })
        .unwrap();
        let _patch =
            AttrPatch::replace(audit.as_any(), "_runner_components_sha256", hashes.as_any());
        let mut good_payload = json!({"campaign_id":"alpha","status":"PASS","generated_at":LATE,"name":"alpha_good","runner_components_sha256":{"runner":"sha"}});
        let good = write_json(
            &case
                .root()
                .join("conductor/mutation_campaigns/receipts/alpha_good.json"),
            &good_payload,
        );
        good_payload["name"] = json!("alpha_drift");
        good_payload["source_sha256"] = json!({"src/conductor/retained_subject.py":"0".repeat(64)});
        let drifted = write_json(
            &case
                .root()
                .join("conductor/mutation_campaigns/receipts/alpha_drift.json"),
            &good_payload,
        );
        let read = audited(py, case.root());
        assert_eq!(read, [good]);
        assert!(!read.contains(&drifted));
    });
}

#[test]
fn orphan_campaign_does_not_stop_audit_of_later_receipts() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", None);
        receipt(py, case.root(), "aaa_ghost", "ghost", "PASS", EARLY, 0);
        let good = receipt(
            py,
            case.root(),
            "alpha_good",
            "alpha",
            "PASS",
            "2026-09-02T00:00:00+00:00",
            0,
        );
        let _patch = audit_accepts(py, &["alpha_good"]);
        assert_eq!(audited(py, case.root()), [good]);
    });
}

#[test]
fn report_counts_receipts_saved_by_explicit_protection() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        let (old, _) = pair(py, &case, "alpha");
        let _patches = no_citations(py);
        let (_capture, stdout) = capture(py, "stdout");
        let code: i32 = retention(py)
            .getattr("main")
            .unwrap()
            .call1((vec![
                "--repo-root",
                case.root().to_str().unwrap(),
                "--protect",
                old.file_name().unwrap().to_str().unwrap(),
            ],))
            .unwrap()
            .extract()
            .unwrap();
        assert_eq!(code, 0);
        let report: Value = serde_json::from_str(&output(py, &stdout)).unwrap();
        assert_eq!(report["protected"], 1);
        assert_eq!(report["deleted"], 0);
    });
}

#[test]
fn no_pass_campaign_is_skipped_while_later_campaign_is_swept() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        let broken = receipt(
            py,
            case.root(),
            "aaa_broken_a",
            "aaa_broken",
            "FAIL",
            EARLY,
            0,
        );
        manifest(py, case.root(), "aaa_broken", None);
        let (old, new) = pair(py, &case, "alpha");
        let _patches = no_citations(py);
        let result = plan(py, case.root(), &[]).unwrap();
        assert_eq!(deleted(&result), [old]);
        assert_eq!(
            kept(py, &result, &new).extract::<String>().unwrap(),
            "newest PASS for alpha"
        );
        assert_eq!(
            kept(py, &result, &broken).extract::<String>().unwrap(),
            "campaign aaa_broken has no PASS receipt to supersede"
        );
    });
}

#[test]
fn compacted_receipts_are_judged_by_summary_alone() {
    let case = Case::new();
    Python::attach(|py| {
        fixture(py, &case);
        manifest(py, case.root(), "alpha", None);
        let slim = module(py, "conductor.mutation_receipt_slim")
            .getattr("slim_receipt")
            .unwrap();
        let mutants: Vec<Value> = (0..80)
            .map(|i| json!({"id":format!("m{i}"),"outcome":"KILLED"}))
            .collect();
        let old_payload =
            json!({"campaign_id":"alpha","status":"PASS","generated_at":EARLY,"mutants":mutants});
        let mut new_payload = old_payload.clone();
        new_payload["generated_at"] = json!(LATE);
        let mut old_slim = py_to_json(&slim.call1((json_to_py(py, &old_payload),)).unwrap());
        old_slim["detail"] = json!({"encoding":"superseded","superseded_by":"alpha_new.json"});
        let old = write_json(
            &case
                .root()
                .join("conductor/mutation_campaigns/receipts/alpha_old.json"),
            &old_slim,
        );
        let new_slim = py_to_json(&slim.call1((json_to_py(py, &new_payload),)).unwrap());
        let new = write_json(
            &case
                .root()
                .join("conductor/mutation_campaigns/receipts/alpha_new.json"),
            &new_slim,
        );
        let _patches = no_citations(py);
        let result = plan(py, case.root(), &[]).unwrap();
        assert_eq!(deleted(&result), [old]);
        assert_eq!(
            kept(py, &result, &new).extract::<String>().unwrap(),
            "newest PASS for alpha"
        );
    });
}
