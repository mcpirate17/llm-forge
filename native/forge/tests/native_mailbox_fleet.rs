//! Fleet inspection is native and cannot acknowledge or rewrite coordination state.
#[allow(dead_code)]
mod support;
use conductor_native::a2a_store::Store;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::fs;
use std::net::TcpListener;
use std::process::Output;

fn fixture() -> support::Host {
    let host = support::Host::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    host.ok(&["mailbox", "init", "--name", "receiver", "--port", &port]);
    drop(listener);
    fs::write(
        host.0.join(".current_work.md"),
        "## Active Coordination\n## Current task, heading-owner\n",
    )
    .unwrap();
    let data = json!({"kind":"coordination-v2","thread_id":"test-thread",
        "status":"open","requires_response":true,"summary":"public bounded summary"});
    Store::initialize(&host.0.join(".agents/a2a"), "receiver")
        .unwrap()
        .record_inbound(
            "message-one",
            "sender",
            "receiver",
            "secret body must remain private",
            Some(&data.to_string()),
            "2026-10-31T00:00:00.000+00:00",
        )
        .unwrap();
    host
}

fn run(host: &support::Host, args: &[&str]) -> Output {
    host.command()
        .env("PATH", "")
        .env("CONDUCTOR_PYTHON", "/forbidden")
        .args(["mailbox", "fleet", "--as-name", "receiver"])
        .args(args)
        .output()
        .unwrap()
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("secret body"));
    serde_json::from_str(&text).unwrap()
}

#[test]
fn fleet_joins_live_inputs_without_touching_inbox_liveness_or_cached_state() {
    let host = fixture();
    let root = host.0.join(".agents/a2a");
    let registry = fs::read(root.join("agents.json")).unwrap();
    let liveness = fs::read(root.join("liveness.json")).ok();
    let report = success(run(&host, &["--json"]));
    assert_eq!(report["authority"], "bounded-fleet-observation");
    assert_eq!(
        report["seats"]["sender"]["last_heard"]["said"],
        "public bounded summary"
    );
    assert_eq!(
        report["seats"]["heading-owner"]["headings"][0],
        "Current task, heading-owner"
    );
    assert_eq!(report["peer_observations"]["receiver"]["status"], "down");
    assert_eq!(report["inbox"]["shown"], 1);
    assert_eq!(report["inbox"]["omitted"], 0);
    assert_eq!(fs::read(root.join("agents.json")).unwrap(), registry);
    assert_eq!(fs::read(root.join("liveness.json")).ok(), liveness);
    assert!(!host.0.join("conductor/active_state.json").exists());
    let db = Connection::open(root.join("receiver/store.sqlite")).unwrap();
    let read: Option<String> = db
        .query_row(
            "SELECT read_at FROM messages WHERE message_id='message-one'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(read.is_none());
}

#[test]
fn process_observations_are_explicit_and_need_no_external_command() {
    let host = fixture();
    let report = success(run(&host, &["--json", "--processes"]));
    assert_eq!(report["processes_requested"], true);
    assert!(report["worktree_processes"].is_object());
    let text = run(&host, &[]);
    assert!(text.status.success());
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("FLEET STATUS") && text.contains("no messages acknowledged"));
}

#[test]
fn malformed_required_inputs_are_errors_and_missing_stores_are_never_created() {
    let empty = support::Host::new();
    let missing = run(&empty, &["--json"]);
    assert!(!missing.status.success() && missing.stdout.is_empty());
    assert!(!empty.0.join(".agents/a2a").exists());
    let host = fixture();
    fs::create_dir_all(host.0.join(".git/governance")).unwrap();
    fs::write(
        host.0.join(".git/governance/ownership-claims.json"),
        "invalid",
    )
    .unwrap();
    let corrupt = run(&host, &["--json"]);
    assert!(!corrupt.status.success() && corrupt.stdout.is_empty());
    assert!(String::from_utf8_lossy(&corrupt.stderr).contains("claim"));
}
