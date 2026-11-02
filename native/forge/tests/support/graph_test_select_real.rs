//! Opt-in real compiler/PyO guest identity test; no ambient toolchain selection.
#[path = "../../src/candidate_cargo_runtime_cache.rs"]
mod cache;
use super::{commit, forge_binary, graph, write};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
const GUEST_TEST: &str = r#"
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyModule};
use std::{fs, path::PathBuf, process::Command};
#[test]
fn newly_built_source_and_binary_identities() {
    let marker = "__MARKER__";
    let runtime = PathBuf::from(std::env::var_os("TMPDIR").unwrap());
    let native = runtime.join("contract-extension/conductor_native.so");
    let forge = runtime.join("contract-forge");
    assert_eq!(PathBuf::from(std::env::var_os("FORGE_BIN").unwrap()), forge);
    let output = Command::new(&forge).output().unwrap();
    assert!(output.status.success());
    let output = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.lines().next(), Some(marker));
    assert_eq!(output.lines().nth(1), forge.to_str());
    let (native_hash, forge_hash) = Python::attach(|py| {
        let wrapper = PyModule::import(py, "conductor").unwrap();
        assert_eq!(wrapper.getattr("SOURCE_MARKER").unwrap().extract::<String>().unwrap(), marker);
        let extension = PyModule::import(py, "conductor_native").unwrap();
        let imported = PathBuf::from(extension.getattr("__file__").unwrap().extract::<String>().unwrap());
        assert_eq!(imported, native);
        let hash = |path: &std::path::Path| -> String {
            PyModule::import(py, "hashlib").unwrap().getattr("sha256").unwrap()
                .call1((PyBytes::new(py, &fs::read(path).unwrap()),)).unwrap()
                .call_method0("hexdigest").unwrap().extract().unwrap()
        };
        (hash(&native), hash(&forge))
    });
    fs::write(std::env::var_os("FORGE_GRAPH_REAL_OBSERVATION").unwrap(),
        format!("{marker}\n{native_hash}\n{forge_hash}\n{}\n{}\n", native.display(), forge.display())).unwrap();
    assert!(std::env::var_os("FORGE_GRAPH_REAL_FAIL").is_none(), "requested real guest failure after identity verification");
}
"#;

const PYTEST_TEST: &str = r#"
import hashlib
import os
from pathlib import Path
import subprocess

def test_same_fresh_native_and_forge_as_contract():
    import conductor
    import conductor_native
    marker = "__MARKER__"
    runtime = Path(os.environ["TMPDIR"])
    native = runtime / "contract-extension/conductor_native.so"
    forge = runtime / "contract-forge"
    assert conductor.SOURCE_MARKER == marker
    assert Path(conductor_native.__file__) == native
    assert Path(os.environ["FORGE_BIN"]) == forge
    completed = subprocess.run([str(forge)], check=True, capture_output=True, text=True)
    assert completed.stdout.splitlines() == [marker, str(forge)]
    lines = [marker, hashlib.sha256(native.read_bytes()).hexdigest(),
             hashlib.sha256(forge.read_bytes()).hexdigest(), str(native), str(forge)]
    observed = Path(os.environ["FORGE_GRAPH_REAL_OBSERVATION"])
    assert observed.read_text().splitlines() == lines
    observed.with_suffix(".pytest").write_text("\n".join(lines) + "\n")
    assert "FORGE_GRAPH_REAL_PYTEST_FAIL" not in os.environ
"#;

fn create(root: &Path, marker: &str) {
    write(root, "src/conductor/probe.py", "VALUE = 1\n");
    write(
        root,
        "tests/test_guest_identity.py",
        &PYTEST_TEST.replace("__MARKER__", marker),
    );
    write(
        root,
        "src/conductor/__init__.py",
        "from conductor_native import SOURCE_MARKER\n",
    );
    write(
        root,
        "native/conductor-native/Cargo.toml",
        r#"[package]
name='conductor-native'
version='0.1.0'
edition='2021'
[lib]
name='conductor_native'
crate-type=['cdylib','rlib']
[features]
python=['dep:pyo3']
python-compat-tests=['python','pyo3/auto-initialize']
[dependencies]
pyo3={version='=0.29.2',optional=true}
"#,
    );
    write(
        root,
        "native/conductor-native/src/lib.rs",
        &format!(
            r#"
#[cfg(feature="python")]
use pyo3::prelude::*;
#[cfg(feature="python")]
#[pymodule]
fn conductor_native(module: &Bound<'_, pyo3::types::PyModule>) -> PyResult<()> {{
    module.add("SOURCE_MARKER", "{marker}")?;
    Ok(())
}}
"#
        ),
    );
    write(
        root,
        "native/conductor-native/tests/python_contracts_graph_guest.rs",
        &GUEST_TEST.replace("__MARKER__", marker),
    );
    write(
        root,
        "native/conductor-native/src/python_contract_targets.tsv",
        "src/conductor/probe.py\tpython_contracts_graph_guest\n",
    );
    write(
        root,
        "native/forge/Cargo.toml",
        "[package]\nname='forge'\nversion='0.1.0'\nedition='2021'\n",
    );
    write(root, "native/forge/src/main.rs", &format!("fn main() {{ println!(\"{marker}\"); println!(\"{{}}\", std::env::current_exe().unwrap().display()); }}\n"));
}

fn locks(root: &Path, base: &Path, bin: &Path, seed: &Path) {
    let home = base.join("lock-cache");
    fs::create_dir(&home).unwrap();
    cache::copy_seed(seed, &home).unwrap();
    for manifest in [
        "native/conductor-native/Cargo.toml",
        "native/forge/Cargo.toml",
    ] {
        let mut command = Command::new(bin.join("cargo"));
        command
            .current_dir(base)
            .args(["generate-lockfile", "--offline", "--manifest-path"])
            .arg(root.join(manifest))
            .env("CARGO_HOME", &home)
            .env("CARGO_NET_OFFLINE", "true")
            .env("RUSTC", bin.join("rustc"))
            .env_remove("RUSTC_WRAPPER")
            .env_remove("RUSTC_WORKSPACE_WRAPPER");
        let output =
            forge::process_probe::run(&mut command, Duration::from_secs(60), 1024 * 1024).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn cli(
    root: &Path,
    runtime: &Path,
    observation: &Path,
    bin: &Path,
    seed: &Path,
    python: &Path,
    fail: &str,
) -> std::process::Output {
    let mut command = Command::new(forge_binary::path());
    command
        .args(["graph-test-select", "--repo"])
        .arg(root)
        .args(["--run", "--runtime-dir"])
        .arg(runtime)
        .arg("--python")
        .arg(python)
        .args(["--timeout-seconds", "240", "src/conductor/probe.py"])
        .env("FORGE_CARGO_BIN", bin)
        .env("FORGE_CARGO_HOME_SEED", seed)
        .env("FORGE_CARGO_EXTRA_PATH", "/usr/bin")
        // This minimal fixture intentionally lacks unrelated installed-plugin APIs.
        .env("PYTEST_DISABLE_PLUGIN_AUTOLOAD", "1")
        .env("FORGE_GRAPH_REAL_OBSERVATION", observation);
    command
        .env_remove("FORGE_GRAPH_REAL_FAIL")
        .env_remove("FORGE_GRAPH_REAL_PYTEST_FAIL");
    if fail == "contract" {
        command.env("FORGE_GRAPH_REAL_FAIL", "1");
    }
    if fail == "pytest" {
        command.env("FORGE_GRAPH_REAL_PYTEST_FAIL", "1");
    }
    forge::process_probe::run(&mut command, Duration::from_secs(300), 1024 * 1024).unwrap()
}

fn assert_identity(output: &std::process::Output, observation: &Path, marker: &str) {
    let text = fs::read_to_string(observation).unwrap();
    let identities: Vec<_> = text.lines().collect();
    assert_eq!(identities[0], marker);
    let evidence: Value = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|value| value.get("graph_test_contract_evidence").cloned())
        .expect("native artifact evidence");
    for (name, digest) in [
        ("conductor_native", identities[1]),
        ("forge", identities[2]),
    ] {
        let artifact = evidence["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["name"] == name)
            .unwrap();
        assert_eq!(artifact["sha256"], digest);
        assert!(artifact["bytes"].as_u64().unwrap() > 0);
    }
    assert_eq!(
        fs::read_to_string(observation.with_extension("pytest")).unwrap(),
        text
    );
}

#[test]
#[ignore = "real native builds: requires FORGE_TEST_CARGO_BIN, FORGE_TEST_CARGO_HOME_SEED and FORGE_TEST_GRAPH_PYTHON; optional FORGE_CONTRACT_PYTHON_LIB_DIR"]
fn real_cli_provisions_guest_artifacts_checks_identity_and_preserves_failure() {
    let bin = PathBuf::from(
        std::env::var_os("FORGE_TEST_CARGO_BIN").expect("explicit native toolchain required"),
    )
    .canonicalize()
    .unwrap();
    let seed = PathBuf::from(
        std::env::var_os("FORGE_TEST_CARGO_HOME_SEED").expect("offline pyo3 cache seed required"),
    )
    .canonicalize()
    .unwrap();
    let python = PathBuf::from(
        std::env::var_os("FORGE_TEST_GRAPH_PYTHON").expect("explicit Python guest required"),
    );
    let owned = tempfile::tempdir().unwrap();
    let base = owned.path().canonicalize().unwrap();
    let root = base.join("repo");
    let runtime = base.join("runtime");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let marker = format!("graph-native-source-{}", std::process::id());
    create(&root, &marker);
    locks(&root, &base, &bin, &seed);
    commit(&root);
    graph(&root);
    let observation = base.join("guest-identities");
    let output = cli(&root, &runtime, &observation, &bin, &seed, &python, "");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_identity(&output, &observation, &marker);
    assert_eq!(fs::read_dir(&runtime).unwrap().count(), 0);
    assert!(!root.join("native/conductor-native/target").exists());
    let pytest_failed = cli(
        &root,
        &runtime,
        &observation,
        &bin,
        &seed,
        &python,
        "pytest",
    );
    assert_eq!(
        pytest_failed.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&pytest_failed.stderr)
    );
    assert_identity(&pytest_failed, &observation, &marker);
    let retained: Vec<_> = fs::read_dir(&runtime)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(retained.len(), 1);
    for entry in retained {
        fs::remove_dir_all(entry).unwrap();
    }
    let failed = cli(
        &root,
        &runtime,
        &observation,
        &bin,
        &seed,
        &python,
        "contract",
    );
    assert_eq!(
        failed.status.code(),
        Some(101),
        "{}",
        String::from_utf8_lossy(&failed.stderr)
    );
    assert_eq!(fs::read_dir(&runtime).unwrap().count(), 1);
}
