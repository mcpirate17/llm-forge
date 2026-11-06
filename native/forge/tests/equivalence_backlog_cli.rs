//! Real native CLI and the Make handoff; authored summaries only, no equivalence guest.
#![cfg(target_os = "linux")]
#[path = "support/forge_binary.rs"]
mod forge_binary;
use serde_json::{json, Value};
use slop_core::gate_types::Summary;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

fn summary(overrides: Value) -> Vec<u8> {
    let mut value = serde_json::to_value(Summary::default()).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .extend(overrides.as_object().unwrap().clone());
    serde_json::to_vec(&value).unwrap()
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(dir.path().join("runs")).unwrap();
    fs::set_permissions(dir.path().join("runs"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(dir.path().join("pyproject.toml"), "[tool.forge.test_record]\nstore='r.db'\n[tool.forge.equivalence_probe]\nledger='ledger.json'\nreport='report.md'\nrun_artifacts='runs'\n").unwrap();
    fs::write(dir.path().join("summary.json"), summary(serde_json::from_str(r#"{"measured":[{"module":"a.py","units":null}],"advisory":[{"module":"a.py","qualname":"f","verdict":"NO_DIFFERENCE_OBSERVED","rule":"drop_clamp","lineno":2,"description":"removed"}]}"#).unwrap())).unwrap();
    fs::write(
        dir.path().join("runs/gate-recorded.json"),
        summary(json!({})),
    )
    .unwrap();
    fs::set_permissions(
        dir.path().join("runs/gate-recorded.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::write(dir.path().join("authored-empty.json"), summary(json!({}))).unwrap();
    dir
}
fn invoke(root: &Path, args: &[&str]) -> Output {
    Command::new(forge_binary::path())
        .current_dir(root)
        .env("PATH", "")
        .args(["equivalence", "backlog", "summary.json"])
        .args(args)
        .output()
        .unwrap()
}
fn value(output: &Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn native_preview_apply_and_explicit_consumption_work_without_path() {
    let dir = fixture();
    let preview = value(&invoke(dir.path(), &["--json"]));
    assert_eq!(preview["status"], "PREVIEW");
    assert_eq!(preview["findings"], 1);
    assert_eq!(preview["summaries"], 2);
    assert!(!dir.path().join("ledger.json").exists());
    assert!(!invoke(dir.path(), &["--consume-recorded"]).status.success());
    let applied = value(&invoke(
        dir.path(),
        &["--apply", "--consume-recorded", "--json"],
    ));
    assert_eq!(applied["status"], "APPLIED");
    assert_eq!(applied["consumed"].as_array().unwrap().len(), 1);
    assert!(dir.path().join("summary.json").exists());
    assert!(!dir.path().join("runs/gate-recorded.json").exists());
    let ledger: Value =
        serde_json::from_slice(&fs::read(dir.path().join("ledger.json")).unwrap()).unwrap();
    assert_eq!(ledger["items"].as_array().unwrap().len(), 1);
    assert!(fs::read_to_string(dir.path().join("report.md"))
        .unwrap()
        .contains("**f**"));
    let repeated = value(&invoke(dir.path(), &["--dry-run", "--json"]));
    assert_eq!(repeated["new"], 0);
    assert_eq!(repeated["carried"], 1);
    fs::write(dir.path().join("summary.json"), "{bad").unwrap();
    assert_eq!(invoke(dir.path(), &["--apply"]).status.code(), Some(4));
}

#[test]
fn make_backlog_routes_only_the_fold_to_native() {
    let dir = fixture();
    let stub = dir.path().join("gate-stub");
    fs::write(
        &stub,
        r#"#!/bin/sh
set -eu
[ "$1" = '-m' ] && [ "$2" = 'conductor.equivalence_gate' ] && [ "$3" = 'gate' ]
printf '%s\n' "$@" > gate-arguments.txt
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--json' ]; then shift; /bin/cp authored-empty.json "$1"; exit 0; fi
  shift
done
exit 2
"#,
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let output = Command::new("make")
        .current_dir(dir.path())
        .args(["--no-print-directory", "-f"])
        .arg(repository.join("conductor.mk"))
        .arg("slop-backlog")
        .arg(format!("PYTHON={}", stub.display()))
        .arg(format!("FORGE={}", forge_binary::path()))
        .arg(format!("CONDUCTOR_HOST_ROOT={}", dir.path().display()))
        .arg("CONDUCTOR_REPORTS_DIR=reports")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let gate_args = fs::read_to_string(dir.path().join("gate-arguments.txt")).unwrap();
    assert!(gate_args.starts_with("-m\nconductor.equivalence_gate\ngate\n"));
    assert!(!gate_args.contains("backlog"));
    assert!(dir.path().join("reports/slop_sweep.json").exists());
    assert!(dir.path().join("ledger.json").exists());
    assert!(dir.path().join("report.md").exists());
    assert!(!dir.path().join("runs/gate-recorded.json").exists());
}
