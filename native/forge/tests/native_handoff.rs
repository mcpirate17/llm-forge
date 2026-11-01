#[allow(dead_code)]
mod support;
use serde_json::{json, Value};
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::process::Output;

fn cli(host: &support::Host, args: &[&str]) -> Output {
    host.command()
        .args(["handoff", "--host"])
        .arg(&host.0)
        .args(args)
        .env("PATH", "")
        .env_remove("A2A_AGENT_NAME")
        .output()
        .unwrap()
}
fn success(host: &support::Host, args: &[&str]) -> Value {
    let result = cli(host, args);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}
fn stage(host: &support::Host, task: &str, context: &str) -> Value {
    success(
        host,
        &[
            "stage",
            "--identity",
            "test-1",
            "--task",
            task,
            "--context",
            context,
        ],
    )
}
fn pending(host: &support::Host) -> std::path::PathBuf {
    host.0.join(".agents/handoff/test-1/pending.json")
}
fn read(host: &support::Host) -> Value {
    serde_json::from_slice(&fs::read(pending(host)).unwrap()).unwrap()
}

#[test]
fn staged_context_is_activated_once_and_subsequent_stages_keep_lineage_without_python() {
    let host = support::Host::new();
    let first = stage(&host, "Continue implementation", "Preserve 🦀 context");
    let before = read(&host);
    assert_eq!(before["status"], "prepared");
    assert_eq!(before["parent_handoff_id"], Value::Null);
    assert_eq!(
        fs::metadata(pending(&host)).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let hook = success(&host, &["hook", "--identity", "test-1"]);
    let context = hook["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("Preserve 🦀 context"));
    assert!(context.contains("non-authoritative"));
    let active = read(&host);
    assert_eq!(active["status"], "active");
    assert!(active["activated_at"].is_string());
    let after = fs::read(pending(&host)).unwrap();
    let again = success(&host, &["hook", "--identity", "test-1"]);
    assert!(again["hookSpecificOutput"]
        .get("additionalContext")
        .is_none());
    assert_eq!(fs::read(pending(&host)).unwrap(), after);
    let next = stage(&host, "Next step", "New context");
    assert_ne!(first["handoff_id"], next["handoff_id"]);
    assert_eq!(read(&host)["parent_handoff_id"], first["handoff_id"]);
    assert!(!host.0.join("conductor/active_state.json").exists());
}

#[test]
fn stale_and_corrupt_handoffs_fail_loud_without_mutating_the_staged_file() {
    let host = support::Host::new();
    stage(&host, "Original", "Important context");
    let original = fs::read(pending(&host)).unwrap();
    fs::write(
        host.0.join("pyproject.toml"),
        "[tool.conductor.session]\npreamble=[]\nstanding_mandates=['NEW: mandate']\n",
    )
    .unwrap();
    let hook = success(&host, &["hook", "--identity", "test-1"]);
    assert!(hook["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("stale"));
    assert_eq!(fs::read(pending(&host)).unwrap(), original);
    fs::remove_file(host.0.join("pyproject.toml")).unwrap();
    let mut corrupted = read(&host);
    corrupted["context"] = json!("tampered");
    fs::write(pending(&host), serde_json::to_vec(&corrupted).unwrap()).unwrap();
    let bytes = fs::read(pending(&host)).unwrap();
    let hook = success(&host, &["hook", "--identity", "test-1"]);
    assert!(hook["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("integrity"));
    assert_eq!(fs::read(pending(&host)).unwrap(), bytes);
    assert!(!cli(
        &host,
        &[
            "stage",
            "--identity",
            "test-1",
            "--task",
            "Replace corruption"
        ]
    )
    .status
    .success());
}

#[test]
fn malformed_metadata_duplicate_keys_and_oversized_inputs_are_rejected() {
    let host = support::Host::new();
    for args in [
        vec!["stage", "--identity", "../escape", "--task", "Invalid"],
        vec![
            "stage",
            "--identity",
            "test-1",
            "--task",
            "Invalid",
            "--paths",
            "../outside",
        ],
        vec![
            "prepare",
            "--output",
            "out.json",
            "--task",
            "Invalid",
            "--runtime-json",
            r#"{"a":1,"a":2}"#,
        ],
        vec![
            "prepare",
            "--output",
            "out.json",
            "--task",
            "Invalid",
            "--runtime-json",
            r#"{"a":[]}"#,
        ],
    ] {
        assert!(!cli(&host, &args).status.success(), "{args:?}");
    }
    assert!(!host.0.join("out.json").exists());
    let context = host.0.join("large.txt");
    fs::write(&context, vec![b'x'; 1024 * 1024 + 1]).unwrap();
    assert!(!cli(
        &host,
        &[
            "prepare",
            "--output",
            "out.json",
            "--task",
            "Large",
            "--context-file",
            context.to_str().unwrap()
        ]
    )
    .status
    .success());
    stage(&host, "Duplicate", "Context");
    let bytes = fs::read_to_string(pending(&host)).unwrap();
    let duplicate = bytes.replacen("\"context\":", "\"context\":\"ignored\",\"context\":", 1);
    fs::write(pending(&host), &duplicate).unwrap();
    let hook = success(&host, &["hook", "--identity", "test-1"]);
    assert!(hook["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("duplicate"));
    assert_eq!(fs::read_to_string(pending(&host)).unwrap(), duplicate);
}

#[test]
fn locked_symlinked_and_shared_files_are_never_overwritten() {
    let host = support::Host::new();
    stage(&host, "Task", "Retain");
    let original = fs::read(pending(&host)).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(pending(&host).with_extension("json.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert!(!cli(
        &host,
        &["stage", "--identity", "test-1", "--task", "Racing"]
    )
    .status
    .success());
    assert_eq!(fs::read(pending(&host)).unwrap(), original);
    drop(lock);
    let target = host.0.join("target.json");
    fs::rename(pending(&host), &target).unwrap();
    symlink(&target, pending(&host)).unwrap();
    let output = cli(
        &host,
        &["activate", "--input", pending(&host).to_str().unwrap()],
    );
    assert!(!output.status.success());
    assert_eq!(fs::read(&target).unwrap(), original);
    fs::remove_file(pending(&host)).unwrap();
    fs::hard_link(&target, pending(&host)).unwrap();
    assert!(!cli(
        &host,
        &["stage", "--identity", "test-1", "--task", "Hard link"]
    )
    .status
    .success());
    fs::remove_file(pending(&host)).unwrap();
    fs::rename(&target, pending(&host)).unwrap();
    fs::set_permissions(pending(&host), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(!cli(
        &host,
        &["activate", "--input", pending(&host).to_str().unwrap()]
    )
    .status
    .success());
}

#[test]
fn prepare_activate_identity_resolution_and_unicode_clipping_are_native() {
    let host = support::Host::new();
    let context = host.0.join("context.txt");
    fs::write(&context, format!("api_key=secret\n{}", "🦀".repeat(4000))).unwrap();
    success(
        &host,
        &[
            "prepare",
            "--task",
            "A task",
            "--output",
            "prepared.json",
            "--context-file",
            context.to_str().unwrap(),
            "--runtime-json",
            r#"{"runner":"native","ok":true,"count":3}"#,
        ],
    );
    let envelope: Value =
        serde_json::from_slice(&fs::read(host.0.join("prepared.json")).unwrap()).unwrap();
    assert_eq!(envelope["context"].as_str().unwrap().chars().count(), 3500);
    assert!(!envelope["context"].as_str().unwrap().contains("secret"));
    let active = cli(&host, &["activate", "--input", "prepared.json"]);
    assert!(
        active.status.success(),
        "{}",
        String::from_utf8_lossy(&active.stderr)
    );
    assert!(String::from_utf8_lossy(&active.stdout).contains("runner=native"));
    fs::create_dir_all(host.0.join(".agents/a2a")).unwrap();
    fs::write(host.0.join(".agents/a2a/default_identity"), "file-agent\n").unwrap();
    let staged = success(&host, &["stage", "--task", "Default context"]);
    assert!(staged["staged"]
        .as_str()
        .unwrap()
        .contains("file-agent/pending.json"));
    let absent = success(&host, &["hook", "--identity", "not-staged"]);
    assert!(absent["hookSpecificOutput"]
        .get("additionalContext")
        .is_none());
}

#[test]
fn resource_ids_are_bounded_path_safe_and_distinct() {
    let mut ids = std::collections::HashSet::new();
    for _ in 0..1024 {
        assert!(ids.insert(forge::resource_id("handoff").unwrap()));
    }
    for invalid in ["", "../escape", "a/b", "é", &"a".repeat(33)] {
        assert!(forge::resource_id(invalid).is_err());
    }
    assert!(ids
        .iter()
        .all(|id| id.len() < 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')));
}
