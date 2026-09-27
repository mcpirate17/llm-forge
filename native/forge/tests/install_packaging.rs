//! Installation contract for the three shipped native runtimes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("Forge crate is below native/")
        .to_path_buf()
}

fn manifest(path: &Path) -> toml::Value {
    std::fs::read_to_string(path)
        .expect("read package manifest")
        .parse()
        .expect("parse package manifest")
}

fn make_dry_run(root: &Path, target: &str) -> String {
    let output = Command::new("make")
        .args(["--dry-run", target])
        .current_dir(root)
        .output()
        .expect("render Makefile target");
    assert!(
        output.status.success(),
        "make --dry-run {target}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 Makefile plan")
        .replace("\\\n", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn developer_install_precompiles_every_native_test_configuration() {
    let root = source_root();
    let install = make_dry_run(&root, "install");
    assert!(install.contains("RUSTUP_TOOLCHAIN=1.98.0"));
    assert!(install.contains("OMP_NUM_THREADS=1 OPENBLAS_NUM_THREADS=1"));
    assert!(install.contains("export PATH=\"$venv/bin:$PATH\""));
    for command in [
        "cargo +1.98.0 test --manifest-path native/conductor-native/Cargo.toml --locked --jobs 2 --all-targets --features python-compat-tests --no-run",
        "cargo +1.98.0 test --manifest-path native/conductor-native/Cargo.toml --locked --jobs 2 --all-targets --no-run",
        "PYO3_NO_PYTHON=1 cargo +1.98.0 test --manifest-path native/conductor-native/Cargo.toml --locked --jobs 2 --all-targets --no-default-features --features source-analysis --no-run",
        "cargo +1.98.0 test --manifest-path native/slop-core/Cargo.toml --locked --jobs 2 --all-targets --no-run",
        "PYO3_NO_PYTHON=1 cargo +1.98.0 test --manifest-path native/forge/Cargo.toml --locked --jobs 2 --all-targets --no-run",
        "cargo +1.98.0 build --manifest-path native/conductor-native/Cargo.toml --locked --jobs 2 --bins --features python-compat-tests",
        "rustc +1.98.0 --edition=2021 --crate-name=forge_task_test_worker",
        "cargo +1.98.0 build --manifest-path native/forge/tests/fixtures/stub_dispatch/Cargo.toml --locked --jobs 2",
    ] {
        assert!(install.contains(command), "install omits {command}");
    }
}

#[test]
fn developer_test_runs_every_native_configuration() {
    let root = source_root();
    let tests = make_dry_run(&root, "test");
    assert!(tests.contains("CUDA_VISIBLE_DEVICES= CARGO_BUILD_JOBS=2 RUST_TEST_THREADS=2"));
    assert!(tests.contains("OMP_NUM_THREADS=1 OPENBLAS_NUM_THREADS=1"));
    assert!(tests.contains("export PATH=\"$venv/bin:$PATH\""));
    for command in [
        "cargo +1.98.0 test --offline --locked --jobs 2 --manifest-path native/conductor-native/Cargo.toml --features python-compat-tests --test 'python_contracts_*' -- --test-threads=1",
        "cargo +1.98.0 test --offline --locked --jobs 2 --manifest-path native/conductor-native/Cargo.toml;",
        "PYO3_NO_PYTHON=1 cargo +1.98.0 test --offline --locked --jobs 2 --manifest-path native/conductor-native/Cargo.toml --no-default-features --features source-analysis -- --test-threads=1",
        "PYO3_NO_PYTHON=1 cargo +1.98.0 test --offline --locked --jobs 2 --manifest-path native/forge/Cargo.toml;",
        "cargo +1.98.0 test --offline --locked --jobs 2 --manifest-path native/slop-core/Cargo.toml",
    ] {
        assert!(tests.contains(command), "test omits {command}");
    }
}

#[test]
fn local_install_declares_cli_and_both_extension_wheels() {
    let root = source_root();
    let host = manifest(&root.join("pyproject.toml"));
    let dependencies = host["project"]["dependencies"]
        .as_array()
        .expect("root dependencies");
    for name in ["forge-cli", "conductor-native", "slop-core"] {
        assert!(dependencies
            .iter()
            .any(|value| value.as_str() == Some(name)));
    }
    let sources = &host["tool"]["uv"]["sources"];
    assert_eq!(sources["forge-cli"]["path"].as_str(), Some("native/forge"));
    assert_eq!(
        sources["conductor-native"]["path"].as_str(),
        Some("native/conductor-native")
    );
    assert_eq!(
        sources["slop-core"]["path"].as_str(),
        Some("native/slop-core")
    );
    let cli = manifest(&root.join("native/forge/pyproject.toml"));
    assert_eq!(cli["project"]["name"].as_str(), Some("forge-cli"));
    assert_eq!(cli["tool"]["maturin"]["bindings"].as_str(), Some("bin"));
    assert!(cli["tool"]["maturin"].get("module-name").is_none());
    assert!(cli["project"].get("version").is_none());
    assert!(cli["project"]["dynamic"]
        .as_array()
        .expect("dynamic version")
        .iter()
        .any(|value| value.as_str() == Some("version")));
    for crate_name in ["conductor-native", "slop-core"] {
        let crate_root = root.join("native").join(crate_name);
        let python = manifest(&crate_root.join("pyproject.toml"));
        let cargo = manifest(&crate_root.join("Cargo.toml"));
        assert_eq!(
            python["project"]["version"].as_str(),
            cargo["package"]["version"].as_str(),
            "{crate_name} wheel and crate versions differ"
        );
        assert!(python["tool"]["maturin"]["module-name"].is_str());
    }
}

fn runtime_command(executable: &Path, scripts: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .env("PATH", scripts)
        .env("CARGO", "/nonexistent/cargo")
        .env("RUSTC", "/nonexistent/rustc")
        .env_remove("PYTHONPATH")
        .env_remove("PYTHONHOME");
    command
}

fn legacy_guard(executable: &Path, scripts: &Path) -> std::process::Output {
    let mut child = runtime_command(executable, scripts)
        .args(["legacy-hook", "guard-check"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run installed Forge legacy hook");
    child
        .stdin
        .take()
        .expect("guard stdin")
        .write_all(br#"{"command":"git status"}"#)
        .expect("send guard request");
    child.wait_with_output().expect("reap installed Forge")
}

#[test]
#[ignore = "requires a clean uv-synced venv in FORGE_INSTALL_SMOKE_VENV"]
fn installed_native_runtimes_run_without_cargo_on_path() {
    let venv = PathBuf::from(
        std::env::var_os("FORGE_INSTALL_SMOKE_VENV")
            .expect("FORGE_INSTALL_SMOKE_VENV must name the clean installed venv"),
    );
    let venv = venv.canonicalize().expect("canonical installed venv");
    let scripts = venv.join("bin");
    assert!(
        !scripts.join("cargo").exists(),
        "runtime PATH contains Cargo"
    );
    assert!(
        !scripts.join("rustc").exists(),
        "runtime PATH contains rustc"
    );
    let forge = scripts.join("forge");
    assert!(
        forge.is_file(),
        "uv did not install Forge CLI: {}",
        forge.display()
    );
    let version = runtime_command(&forge, &scripts)
        .arg("--version")
        .output()
        .expect("run installed Forge");
    assert!(
        version.status.success(),
        "{}",
        String::from_utf8_lossy(&version.stderr)
    );
    let version_text = String::from_utf8(version.stdout).expect("UTF-8 Forge version");
    assert!(version_text.starts_with(&format!("forge {} ", env!("CARGO_PKG_VERSION"))));
    let guard = legacy_guard(&forge, &scripts);
    assert!(
        guard.status.success(),
        "{}",
        String::from_utf8_lossy(&guard.stderr)
    );
    assert_eq!(guard.stdout, b"null\n");

    let python = scripts.join("python");
    let probe = runtime_command(&python, &scripts)
        .args([
            "-c",
            "import json, conductor_native, slop_core; print(json.dumps([conductor_native.__file__, slop_core.__file__]))",
        ])
        .output()
        .expect("import installed native extensions");
    assert!(
        probe.status.success(),
        "{}",
        String::from_utf8_lossy(&probe.stderr)
    );
    let origins: Value = serde_json::from_slice(&probe.stdout).expect("native origin JSON");
    let origins = origins.as_array().expect("two native origins");
    assert_eq!(origins.len(), 2);
    for origin in origins {
        let path = PathBuf::from(origin.as_str().expect("native module path"));
        assert!(
            path.starts_with(&venv),
            "native module escaped venv: {}",
            path.display()
        );
        assert!(
            path.is_file(),
            "native module is absent: {}",
            path.display()
        );
    }
}
