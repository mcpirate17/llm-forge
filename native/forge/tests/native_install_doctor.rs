#[allow(dead_code)]
mod support;
use serde_json::{json, Value};
use std::fs;
use std::process::Output;

fn call(host: &support::Host, arguments: &[&str]) -> Output {
    host.command()
        .args(arguments)
        .env("PATH", "")
        .env("HOME", host.0.join("home"))
        .env("XDG_STATE_HOME", host.0.join("state"))
        .env("CONDUCTOR_PYTHON", "/forbidden-python")
        .env("CONDUCTOR_SNAPSHOT_PYTHON", "/forbidden-python")
        .env_remove("FORGE_NATIVE_HOOKS")
        .output()
        .unwrap()
}

#[test]
fn full_install_and_doctor_work_without_an_interpreter_or_path() {
    let host = support::Host::new();
    let root = host.0.to_str().unwrap();
    let install = ["hooks", "install", "--host", root];
    let output = call(&host, &install);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let settings = host.0.join(".claude/settings.json");
    let first = fs::read(&settings).unwrap();
    assert!(!String::from_utf8_lossy(&first).contains("PYTHON"));
    assert!(call(&host, &install).status.success());
    assert_eq!(fs::read(&settings).unwrap(), first);
    let output = call(&host, &["doctor", "--host", root, "--json"]);
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], true);
    let checks = report["checks"].as_array().unwrap();
    assert_eq!(checks.len(), 5);
    assert!(checks.iter().all(|c| c["status"] == "PASS"));
    assert!(checks.iter().all(|c| c["name"] != "python"));
}

#[test]
fn reinstall_removes_obsolete_bindings_and_preserves_unrelated_entries() {
    let host = support::Host::new();
    let root = host.0.to_str().unwrap();
    fs::create_dir_all(host.0.join(".claude")).unwrap();
    let binary = support::forge_binary::path();
    let external =
        json!({"matcher":".*","hooks":[{"type":"command","command":"operator-owned-hook"}]});
    let old = json!({"hooks":{"SessionStart":[external.clone(),{"matcher":".*","hooks":[{"type":"command","command":format!("env CONDUCTOR_PYTHON=/old/python FORGE_MODE=warn {binary} hook SessionStart")}]}]}});
    fs::write(host.0.join(".claude/settings.json"), old.to_string()).unwrap();
    let output = call(&host, &["hooks", "install", "--host", root]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = fs::read_to_string(host.0.join(".claude/settings.json")).unwrap();
    assert!(!text.contains("CONDUCTOR_PYTHON"));
    let settings: Value = serde_json::from_str(&text).unwrap();
    assert!(settings["hooks"]["SessionStart"]
        .as_array()
        .unwrap()
        .contains(&external));
    let before = text;
    assert!(!call(
        &host,
        &[
            "hooks",
            "install",
            "--host",
            root,
            "--python",
            "/old/python"
        ]
    )
    .status
    .success());
    assert_eq!(
        fs::read_to_string(host.0.join(".claude/settings.json")).unwrap(),
        before
    );
}
