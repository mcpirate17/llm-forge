#[path = "../src/source_fingerprint.rs"]
mod source_fingerprint;
#[allow(dead_code)]
mod support;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::process::{Output, Stdio};

fn host() -> support::Host {
    let host = support::Host::new();
    fs::write(host.0.join("pyproject.toml"),
        "[tool.conductor.session]\npreamble=['Bounded native fixture policy']\nstanding_mandates=['KB-TEST: keep fixture']\n").unwrap();
    host
}
fn hook(host: &support::Host, input: &str) -> Output {
    let mut child = host
        .command()
        .args(["hook", "SessionStart"])
        .env("PATH", "")
        .env("CLAUDE_PROJECT_DIR", &host.0)
        .env("HOME", host.0.join("home"))
        .env("XDG_STATE_HOME", host.0.join("state"))
        .env("CRG_GATE_STATE_DIR", host.0.join("gate"))
        .env("CRG_DATA_DIR", host.0.join("graph"))
        .env("CONTEXT_TELEMETRY_PATH", host.0.join("telemetry.jsonl"))
        .env("CONDUCTOR_PYTHON", "/forbidden-python")
        .env_remove("A2A_AGENT_NAME")
        .env_remove("PROJECT_HOOK_DIR")
        .env_remove("FORGE_HOOK_STANDALONE")
        .env_remove("FORGE_NATIVE_HOOKS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let write = child.stdin.take().unwrap().write_all(input.as_bytes());
    if let Err(error) = write {
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }
    child.wait_with_output().unwrap()
}
fn context(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let payload: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        payload["hookSpecificOutput"]["hookEventName"],
        "SessionStart"
    );
    payload["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .into()
}
fn extension(host: &support::Host, mode: &str) -> std::path::PathBuf {
    let path = host.0.join(".claude/hooks/project");
    fs::create_dir_all(&path).unwrap();
    fs::copy(support::task_worker(), path.join("session-start")).unwrap();
    fs::write(path.join("mode"), mode).unwrap();
    path
}

#[test]
fn startup_composes_policy_handoff_graph_gate_and_additive_native_extension() {
    let host = host();
    fs::remove_file(host.0.join("pyproject.toml")).unwrap();
    fs::write(host.0.join("forge.toml"),
        "schema_version=1\n[conductor.session]\npreamble=['Bounded native fixture policy']\nstanding_mandates=['KB-TEST: keep fixture']\n").unwrap();
    let path = extension(&host, "ok");
    let result = host
        .command()
        .env("PATH", "")
        .env_remove("A2A_AGENT_NAME")
        .args([
            "handoff",
            "stage",
            "--identity",
            "claude",
            "--task",
            "Continue fixture",
            "--context",
            "retained handoff 🦀",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let input = r#"{"session_id":"native-session","source":"clear"}"#;
    let text = context(hook(&host, input));
    assert!(text.contains("Bounded native fixture policy"));
    assert!(text.contains("MANDATES: KB-TEST"));
    assert_eq!(text.matches("EXPOSED:").count(), 1);
    assert!(text.contains("retained handoff 🦀"));
    assert!(text.contains("Project extension context 🦀"));
    assert_eq!(
        fs::read_to_string(path.join("received.json")).unwrap(),
        input
    );
    assert!(fs::read_dir(host.0.join("gate")).unwrap().any(|e| e
        .unwrap()
        .path()
        .extension()
        .is_some_and(|e| e == "pending")));
    assert!(!context(hook(&host, input)).contains("retained handoff 🦀"));
    let records = fs::read_to_string(host.0.join("telemetry.jsonl")).unwrap();
    assert!(records
        .lines()
        .any(|line| serde_json::from_str::<Value>(line).unwrap()["event"] == "HookContext"));
    assert!(records.contains("session-start"));
}

#[test]
fn extension_failures_output_floods_and_timeouts_preserve_generic_context() {
    let host = host();
    let path = extension(&host, "fail");
    for mode in ["fail", "flood", "sleep"] {
        fs::write(path.join("mode"), mode).unwrap();
        let began = std::time::Instant::now();
        let text = context(hook(&host, "{}"));
        assert!(text.contains("Bounded native fixture policy"));
        assert!(text.contains("project extension failed"), "{text}");
        assert!(!text.contains(&"x".repeat(200)));
        assert!(began.elapsed() < std::time::Duration::from_secs(5));
    }
}

#[test]
fn missing_registration_is_visible_without_suppressing_preamble() {
    let host = host();
    fs::create_dir_all(host.0.join(".agents/a2a")).unwrap();
    fs::write(host.0.join(".agents/a2a/default_identity"), "fixture\n").unwrap();
    let text = context(hook(&host, "{}"));
    assert!(text.contains("mailbox startup failed"));
    assert!(text.contains("Bounded native fixture policy"));
}

#[test]
fn invalid_identity_cannot_activate_the_default_identity_handoff() {
    let host = host();
    let staged = host
        .command()
        .env_remove("A2A_AGENT_NAME")
        .args([
            "handoff",
            "stage",
            "--identity",
            "claude",
            "--task",
            "Retain fixture",
            "--context",
            "unconsumed handoff",
        ])
        .output()
        .unwrap();
    assert!(staged.status.success());
    fs::create_dir_all(host.0.join(".agents/a2a")).unwrap();
    fs::write(host.0.join(".agents/a2a/default_identity"), "../invalid\n").unwrap();
    let text = context(hook(&host, "{}"));
    assert!(text.contains("identity failed"));
    assert!(text.contains("Bounded native fixture policy"));
    assert!(!text.contains("unconsumed handoff"));
    let envelope: Value = serde_json::from_slice(
        &fs::read(host.0.join(".agents/handoff/claude/pending.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(envelope["status"], "prepared");
}

#[test]
fn malformed_duplicate_and_oversized_hook_inputs_cause_no_startup_writes() {
    let host = host();
    for input in [
        "[1]".into(),
        "{\"session_id\":1,\"session_id\":2}".into(),
        format!("{{\"data\":\"{}\"}}", "x".repeat(1024 * 1024)),
    ] {
        let output = hook(&host, &input);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!host.0.join("conductor").exists());
        assert!(!host.0.join("gate").exists());
    }
}

fn native_tree(host: &support::Host) {
    for crate_name in ["forge", "conductor-native", "slop-core"] {
        let base = host.0.join("native").join(crate_name);
        fs::create_dir_all(base.join("src")).unwrap();
        fs::write(
            base.join("Cargo.toml"),
            format!("[package]\nname='{crate_name}'\n"),
        )
        .unwrap();
        fs::write(base.join("Cargo.lock"), "version = 4\n").unwrap();
        fs::write(base.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    }
    for path in [
        "ledger/routing_policy.toml",
        "native/forge/tests/fixtures/ledger/calibration.json",
    ] {
        fs::create_dir_all(host.0.join(path).parent().unwrap()).unwrap();
        fs::write(host.0.join(path), "fixture").unwrap();
    }
}

#[test]
fn source_fingerprint_detects_same_length_edits_paths_and_missing_inputs() {
    let host = host();
    native_tree(&host);
    let hash = || {
        source_fingerprint::fingerprint(&host.0, &source_fingerprint::files(&host.0).unwrap())
            .unwrap()
    };
    let first = hash();
    assert_eq!(first, hash());
    fs::write(
        host.0.join("native/forge/src/lib.rs"),
        "pub fn changed() {}\n",
    )
    .unwrap();
    assert_ne!(first, hash());
    let second = hash();
    fs::rename(
        host.0.join("native/forge/src/lib.rs"),
        host.0.join("native/forge/src/other.rs"),
    )
    .unwrap();
    assert_ne!(second, hash());
    assert!(context(hook(&host, &json!({}).to_string())).contains("Native Forge binary differs"));
    fs::remove_file(host.0.join("native/forge/Cargo.lock")).unwrap();
    assert!(source_fingerprint::files(&host.0).is_err());
}
