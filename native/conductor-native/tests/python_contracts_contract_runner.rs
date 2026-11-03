#![cfg(feature = "python-compat-tests")]
//! Retained native runtime planning contracts, independent of candidate-review execution.

#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use pyo3::prelude::*;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use support::{module, text, Case};

// Concrete candidate programs for these runtime-planning fixtures. Actual confined
// execution and source provenance are covered by Forge's contract Stage controls.
const PROVENANCE_TEST: &str = r#"
#[test]
fn candidate_imports_and_binary_are_used() {
    use std::process::Command;
    let python = std::env::var("PYO3_PYTHON").unwrap();
    let script = "import conductor, conductor._native, conductor.candidate_probe, conductor_native; print(conductor.__file__); print(conductor.candidate_probe.MARKER); print(conductor._native.marker()); print(conductor_native.__file__)";
    let imported = Command::new(python).args(["-c", script]).output().unwrap();
    assert!(imported.status.success(), "{}", String::from_utf8_lossy(&imported.stderr));
    let lines = String::from_utf8(imported.stdout).unwrap();
    let lines: Vec<_> = lines.lines().collect();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
    assert_eq!(lines[0], root.join("src/conductor/__init__.py").to_str().unwrap());
    assert_eq!(lines[1], "CANDIDATE_PYTHON");
    assert_eq!(lines[2], "CANDIDATE_EXTENSION");
    assert!(lines[3].ends_with("/contract-extension/conductor_native.so"));
    let forge = Command::new(std::env::var("FORGE_BIN").unwrap()).output().unwrap();
    assert!(forge.status.success());
    assert_eq!(String::from_utf8(forge.stdout).unwrap().trim(), "CANDIDATE_FORGE");
}
"#;

const SLOP_PROVENANCE_TEST: &str = r#"
#[test]
fn candidate_slop_extension_is_used() {
    use std::process::Command;
    let python = std::env::var("PYO3_PYTHON").unwrap();
    let script = "import slop_core; print(slop_core.marker()); print(slop_core.__file__)";
    let imported = Command::new(python).args(["-c", script]).output().unwrap();
    assert!(imported.status.success(), "{}", String::from_utf8_lossy(&imported.stderr));
    let lines = String::from_utf8(imported.stdout).unwrap();
    let lines: Vec<_> = lines.lines().collect();
    assert_eq!(lines[0], "CANDIDATE_SLOP");
    assert!(lines[1].ends_with("/contract-extension/slop_core.so"));
}
"#;

fn generate_lock(root: &Path, manifest: &str) {
    let output = Command::new("cargo")
        .args([
            "generate-lockfile",
            "--offline",
            "--manifest-path",
            manifest,
        ])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "fixture lockfile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn runtime_fixture(case: &Case) -> PathBuf {
    let root = case.mkdir("candidate_ws");
    case.write(
        "candidate_ws/native/conductor-native/Cargo.toml",
        "[package]\nname='conductor-native'\nversion='0.1.0'\nedition='2021'\n[lib]\nname='conductor_native'\ncrate-type=['cdylib']\n[features]\npython=[]\npython-compat-tests=[]\n[dependencies]\npyo3={version='0.29',features=['extension-module']}\n",
    );
    case.write(
        "candidate_ws/native/conductor-native/src/lib.rs",
        "use pyo3::prelude::*;\nuse pyo3::types::PyModule;\n#[pyfunction] fn marker() -> &'static str { \"CANDIDATE_EXTENSION\" }\n#[pymodule] fn conductor_native(module: &Bound<'_, PyModule>) -> PyResult<()> { module.add_function(wrap_pyfunction!(marker, module)?)?; Ok(()) }\n",
    );
    case.write(
        "candidate_ws/native/conductor-native/tests/python_contracts_probe.rs",
        PROVENANCE_TEST,
    );
    case.write(
        "candidate_ws/native/conductor-native/src/python_contract_targets.tsv",
        "src/conductor/candidate_probe.py\tpython_contracts_probe\n",
    );
    case.write(
        "candidate_ws/native/forge/Cargo.toml",
        "[package]\nname='forge'\nversion='0.1.0'\nedition='2021'\n[[bin]]\nname='forge'\npath='src/main.rs'\n",
    );
    case.write(
        "candidate_ws/native/forge/src/main.rs",
        "fn main() { println!(\"CANDIDATE_FORGE\"); }\n",
    );
    case.write(
        "candidate_ws/src/conductor/__init__.py",
        "MARKER='CANDIDATE_PACKAGE'\n",
    );
    case.write(
        "candidate_ws/src/conductor/candidate_probe.py",
        "MARKER='CANDIDATE_PYTHON'\n",
    );
    case.write(
        "candidate_ws/src/conductor/_native.py",
        "from conductor_native import marker\n",
    );
    generate_lock(&root, "native/conductor-native/Cargo.toml");
    generate_lock(&root, "native/forge/Cargo.toml");
    root
}
fn slop_runtime_fixture(case: &Case) -> PathBuf {
    let root = runtime_fixture(case);
    case.write(
        "candidate_ws/native/slop-core/Cargo.toml",
        "[package]\nname='slop-core'\nversion='0.1.0'\nedition='2021'\n[lib]\nname='slop_core'\ncrate-type=['cdylib']\n[features]\nextension-module=['pyo3/extension-module']\n[dependencies]\npyo3={version='0.29'}\n",
    );
    case.write(
        "candidate_ws/native/slop-core/src/lib.rs",
        "use pyo3::prelude::*;\nuse pyo3::types::PyModule;\n#[pyfunction] fn marker() -> &'static str { \"CANDIDATE_SLOP\" }\n#[pymodule] fn slop_core(module: &Bound<'_, PyModule>) -> PyResult<()> { module.add_function(wrap_pyfunction!(marker, module)?)?; Ok(()) }\n",
    );
    case.write(
        "candidate_ws/native/conductor-native/tests/python_contracts_native_ablations.rs",
        SLOP_PROVENANCE_TEST,
    );
    case.write(
        "candidate_ws/native/conductor-native/src/python_contract_targets.tsv",
        "src/conductor/candidate_probe.py\tpython_contracts_native_ablations\n",
    );
    generate_lock(&root, "native/slop-core/Cargo.toml");
    root
}
fn native_runtime_plan(py: Python<'_>, root: &Path, targets: &[&str]) -> PyResult<Value> {
    let request = json!({
        "snapshot": root,
        "runtime_dir": root.join("runtime"),
        "python_executable": text(&module(py, "sys").getattr("executable").unwrap()),
        "targets": targets,
    });
    let response: String = module(py, "conductor._native")
        .getattr("candidate_verification_native")?
        .call1(("contract_runtime_plan", request.to_string()))?
        .extract()?;
    Ok(serde_json::from_str(&response).unwrap())
}
#[test]
fn slop_build_is_selected_only_for_observed_consumers() {
    let case = Case::new();
    let root = slop_runtime_fixture(&case);
    Python::attach(|py| {
        for (target, expected) in [
            ("python_contracts_probe", false),
            ("python_contracts_native_ablations", true),
            ("python_contracts_probe_budget", true),
            ("python_contracts_equivalence_gate", true),
            ("python_contracts_policy_engine_crash", true),
            ("python_contracts_import_ablation", true),
            ("python_contracts_reuse_inventory", true),
            ("python_contracts_reuse_roi", true),
            ("python_contracts_reuse_consolidation", true),
            ("python_contracts_reuse_sweep", true),
            ("python_contracts_reuse_file_families", true),
            ("python_contracts_reuse_audit_inventory", true),
            ("python_contracts_reuse_file_family_lsh", true),
            ("python_contracts_reuse_consolidation_collect", true),
            ("python_contracts_reuse_detector_scan", true),
            ("python_contracts_reuse_file_family_profiles", true),
        ] {
            let plan = native_runtime_plan(py, &root, &[target]).unwrap();
            let builds = plan["build_commands"].as_array().unwrap();
            assert_eq!(builds.len(), if expected { 3 } else { 2 });
            let slop = builds.iter().find(|step| {
                step["argv"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|arg| arg == "native/slop-core/Cargo.toml")
            });
            assert_eq!(slop.is_some(), expected);
            if let Some(step) = slop {
                assert_eq!(
                    step["artifact"],
                    json!(root.join("runtime/contract-cargo-target/debug/libslop_core.so"))
                );
                assert_eq!(
                    step["destination"],
                    json!(root.join("runtime/contract-extension/slop_core.so"))
                );
                let argv = step["argv"].as_array().unwrap();
                for arg in [
                    "--offline",
                    "--locked",
                    "--jobs",
                    "2",
                    "--features",
                    "extension-module",
                    "--lib",
                ] {
                    assert!(argv.iter().any(|value| value == arg));
                }
            }
        }
    });
}
#[test]
fn selected_slop_requires_a_regular_candidate_manifest() {
    let case = Case::new();
    let root = slop_runtime_fixture(&case);
    let manifest = root.join("native/slop-core/Cargo.toml");
    std::fs::remove_file(&manifest).unwrap();
    Python::attach(|py| {
        let error =
            native_runtime_plan(py, &root, &["python_contracts_native_ablations"]).unwrap_err();
        assert!(error.to_string().contains("candidate slop_core"));
        assert!(native_runtime_plan(py, &root, &["python_contracts_probe"]).is_ok());
    });
    let outside = case.write("outside-slop.toml", "[package]\nname='outside'\n");
    std::os::unix::fs::symlink(outside, &manifest).unwrap();
    Python::attach(|py| {
        let error =
            native_runtime_plan(py, &root, &["python_contracts_native_ablations"]).unwrap_err();
        assert!(error.to_string().contains("candidate slop_core"));
    });
}
#[test]
fn native_policy_reports_unmapped_source_in_mixed_contract_change() {
    let _case = Case::new();
    Python::attach(|py| {
        let native = module(py, "conductor._native");
        let request = json!({
            "sources": ["src/conductor/gate_rollout.py", "src/conductor/unmapped.py"],
            "changed_tests": [],
            "graph_tests": [],
            "native_tests": {},
            "contract_sources": ["src/conductor/gate_rollout.py"],
            "contract_targets": ["python_contracts_gate_rollout"],
            "contract_test_paths": ["native/conductor-native/tests/python_contracts_gate_rollout.rs"],
            "graph": {},
            "graph_error": null,
            "high_risk": false,
            "property_evidence": false
        });
        let raw = native
            .getattr("candidate_verification_native")
            .unwrap()
            .call1(("selection_decide", request.to_string()))
            .unwrap();
        let response: Value = serde_json::from_str(&text(&raw)).unwrap();
        assert_eq!(response["findings"][0]["rule_id"], "no-targeted-tests");
        assert_eq!(
            response["findings"][0]["evidence"]["source_paths"],
            json!(["src/conductor/unmapped.py"])
        );
        assert_eq!(response["graph"]["contract_test_files"], 1);
    });
}
