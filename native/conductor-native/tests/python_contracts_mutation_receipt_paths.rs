#![cfg(feature = "python-compat-tests")]
//! Receipt path selection at the host Python boundary.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::fs;
use std::path::Path;
use support::{assert_error, module, path, text, Case};

fn campaign<'py>(py: Python<'py>, id: &str) -> Bound<'py, pyo3::types::PyAny> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("campaign_id", id).unwrap();
    module(py, "types")
        .getattr("SimpleNamespace")
        .unwrap()
        .call((), Some(&kwargs))
        .unwrap()
}

fn default_path(root: &Path, id: &str) -> std::path::PathBuf {
    Python::attach(|py| {
        module(py, "conductor.mutation_receipt_build")
            .getattr("_default_receipt_path")
            .unwrap()
            .call1((campaign(py, id), path(py, root)))
            .unwrap()
            .extract()
            .unwrap()
    })
}

#[test]
fn default_receipt_path_uses_configured_or_legacy_root_and_creates_directories() {
    let case = Case::new();
    let legacy = default_path(case.root(), "fixture_campaign");
    assert_eq!(
        legacy.parent().unwrap(),
        case.root().join("research/reports/mutation_testing")
    );
    assert!(legacy.parent().unwrap().is_dir());
    assert!(legacy
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("fixture_campaign_"));
    assert_eq!(legacy.extension().unwrap(), "json");

    case.write(
        "pyproject.toml",
        "[tool.conductor]\nmutation_receipt_root = \"campaigns/receipts\"\n",
    );
    let configured = default_path(case.root(), "llm_forge_campaign");
    assert_eq!(
        configured.parent().unwrap(),
        case.root().join("campaigns/receipts")
    );
    assert!(configured.parent().unwrap().is_dir());
    assert!(configured
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("llm_forge_campaign_"));

    case.write(
        "pyproject.toml",
        "[tool.conductor]\nmutation_receipt_root = \"a/b/c/receipts\"\n",
    );
    let nested = case.root().join("a/b/c/receipts");
    assert!(!nested.exists());
    assert_eq!(
        default_path(case.root(), "fixture_campaign")
            .parent()
            .unwrap(),
        nested
    );
    assert!(nested.is_dir());
}

#[test]
fn receipt_directory_creation_failure_is_a_named_campaign_error() {
    let case = Case::new();
    case.write(
        "pyproject.toml",
        "[tool.conductor]\nmutation_receipt_root = \"blocked\"\n",
    );
    case.write("blocked", "not a directory\n");
    Python::attach(|py| {
        let build = module(py, "conductor.mutation_receipt_build");
        let error = module(py, "conductor.mutation_scope")
            .getattr("CampaignError")
            .unwrap();
        assert_error(
            py,
            build
                .getattr("_default_receipt_path")
                .unwrap()
                .call1((campaign(py, "fixture_campaign"), path(py, case.root())))
                .unwrap_err(),
            &error,
            "cannot create mutation receipt directory",
        );
    });
}

#[test]
fn resolved_receipt_path_uses_default_or_explicit_target() {
    let case = Case::new();
    case.write(
        "pyproject.toml",
        "[tool.conductor]\nmutation_receipt_root = \"campaigns/receipts\"\n",
    );
    Python::attach(|py| {
        let resolve = module(py, "conductor.mutation_receipt_build")
            .getattr("_resolve_receipt_path")
            .unwrap();
        let default = resolve
            .call1((
                campaign(py, "resolved_campaign"),
                py.None(),
                path(py, case.root()),
            ))
            .unwrap();
        let output: std::path::PathBuf = default.get_item(0).unwrap().extract().unwrap();
        assert_eq!(
            output.parent().unwrap(),
            case.root().join("campaigns/receipts")
        );
        assert!(text(&default.get_item(1).unwrap())
            .starts_with("campaigns/receipts/resolved_campaign_"));
        let explicit = case.root().join("somewhere/receipt.json");
        let resolved = resolve
            .call1((
                campaign(py, "fixture_campaign"),
                path(py, &explicit),
                path(py, case.root()),
            ))
            .unwrap();
        assert_eq!(
            resolved
                .get_item(0)
                .unwrap()
                .extract::<std::path::PathBuf>()
                .unwrap(),
            explicit
        );
        assert_eq!(
            text(&resolved.get_item(1).unwrap()),
            "somewhere/receipt.json"
        );
    });
    assert!(!case.root().join("somewhere/receipt.json").exists());
    assert!(fs::read_dir(case.root().join("campaigns/receipts"))
        .unwrap()
        .next()
        .is_none());
}
