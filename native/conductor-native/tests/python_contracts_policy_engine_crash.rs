#![cfg(feature = "python-compat-tests")]
//! Equivalence help remains available to the slop gate report.
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;
use std::{ffi::OsString, fs, path::Path, process::Command};
use support::Case;

#[test]
fn incomplete_probe_help_names_a_command_the_gate_accepts() {
    let _case = Case::new();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../slop-core/src/gate_report.rs");
    let content = fs::read_to_string(source).unwrap();
    assert!(content.contains("python -m conductor.equivalence_gate gate --module <module>"));
    let python = std::env::var_os("PYO3_PYTHON")
        .or_else(|| std::env::var_os("PYTHON"))
        .unwrap_or_else(|| OsString::from("python3"));
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
    let mut search_path = vec![source_root];
    if let Some(inherited) = std::env::var_os("PYTHONPATH") {
        search_path.extend(std::env::split_paths(&inherited));
    }
    let output = Command::new(python)
        .args(["-m", "conductor.equivalence_gate", "gate", "--help"])
        .env("PYTHONPATH", std::env::join_paths(search_path).unwrap())
        .output()
        .expect("run equivalence_gate help");
    assert!(
        output.status.success(),
        "equivalence_gate help failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("--module"));
}
