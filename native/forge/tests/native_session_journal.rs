#[allow(dead_code)]
mod support;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::{Output, Stdio};

fn call(host: &support::Host, args: &[&str], payload: &Value) -> Output {
    let mut child = host
        .command()
        .args(args)
        .env("PATH", "")
        .env("CLAUDE_PROJECT_DIR", &host.0)
        .env("FORGE_JOURNAL_DIR", host.0.join("journal"))
        .env("OBSIDIAN_VAULT_ROOT", host.0.join("vault"))
        .env("HOME", host.0.join("home"))
        .env("FORGE_LEDGER_DISABLE", "1")
        .env_remove("FORGE_HOOK_STANDALONE")
        .env_remove("FORGE_NATIVE_HOOKS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(payload).unwrap().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}
fn good(output: Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn edit(host: &support::Host, sid: &str, name: &str) {
    let path = host.0.join(name);
    fs::write(&path, "fixture").unwrap();
    good(call(
        host,
        &["hook", "PostToolUse"],
        &json!({"session_id":sid,"tool_name":"Edit","tool_input":{"file_path":path}}),
    ));
}
fn journal(host: &support::Host, sid: &str, reason: &str) -> Output {
    call(
        host,
        &["session", "journal", "--host", host.0.to_str().unwrap()],
        &json!({"session_id":sid,"reason":reason}),
    )
}
fn daily(host: &support::Host) -> PathBuf {
    fs::read_dir(host.0.join("vault/claude"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "md"))
        .unwrap()
}

#[test]
fn native_post_edit_records_are_consumed_only_after_durable_daily_publication() {
    let host = support::Host::new();
    git2::Repository::init(&host.0).unwrap();
    edit(&host, "session-1", "one.txt");
    edit(&host, "session-1", "two.txt");
    edit(&host, "session-1", "one.txt");
    assert_eq!(
        fs::read_to_string(host.0.join("journal/session-1.tsv"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    good(journal(&host, "session-1", "finished"));
    let text = fs::read_to_string(daily(&host)).unwrap();
    assert!(text.contains("### Files edited (2)"));
    assert!(text.contains("one.txt"));
    assert!(text.contains("two.txt"));
    assert!(text.contains("End reason: `finished`"));
    assert!(text.contains("git status at session end"));
    assert!(!host.0.join("journal/session-1.tsv").exists());
}

#[test]
fn malformed_accumulators_and_unsafe_destinations_preserve_original_records() {
    let host = support::Host::new();
    edit(&host, "bad", "one.txt");
    let accum = host.0.join("journal/bad.tsv");
    fs::write(&accum, "malformed record\n").unwrap();
    assert!(!journal(&host, "bad", "finished").status.success());
    assert_eq!(fs::read_to_string(&accum).unwrap(), "malformed record\n");
    good(journal(&host, "initial", "initial"));
    let day = daily(&host);
    fs::remove_file(&day).unwrap();
    let outside = host.0.join("outside");
    fs::write(&outside, "retain").unwrap();
    symlink(&outside, &day).unwrap();
    edit(&host, "safe", "two.txt");
    assert!(!journal(&host, "safe", "finished").status.success());
    assert!(host.0.join("journal/safe.tsv").is_file());
    assert_eq!(fs::read_to_string(&outside).unwrap(), "retain");
}

#[test]
fn invalid_session_ids_and_hardlinked_inputs_are_rejected() {
    let host = support::Host::new();
    assert!(!journal(&host, "../escape", "finished").status.success());
    edit(&host, "linked", "one.txt");
    let source = host.0.join("journal/linked.tsv");
    fs::hard_link(&source, host.0.join("retained.tsv")).unwrap();
    assert!(!journal(&host, "linked", "finished").status.success());
    assert!(source.is_file());
}

#[test]
fn repeated_reason_only_events_are_idempotent_and_control_text_is_escaped() {
    let host = support::Host::new();
    good(journal(&host, "reason-only", "stopped\n```injected"));
    let first = fs::read(daily(&host)).unwrap();
    good(journal(&host, "reason-only", "stopped\n```injected"));
    assert_eq!(fs::read(daily(&host)).unwrap(), first);
    let text = String::from_utf8(first).unwrap();
    assert!(!text.contains("\n```injected"));
    assert_eq!(text.matches("forge-session:").count(), 1);
}

#[test]
fn path_separators_do_not_forge_additional_accumulator_records() {
    let host = support::Host::new();
    let output = call(
        &host,
        &["hook", "PostToolUse"],
        &json!({"session_id":"safe","tool_name":"Edit","tool_input":{"file_path":host.0.join("a\ninjected.txt")}}),
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported separators"));
    assert!(!host.0.join("journal/safe.tsv").exists());
}

#[test]
fn completion_hooks_are_native_and_failed_publication_keeps_records() {
    let host = support::Host::new();
    edit(&host, "ending", "changed.txt");
    let output = call(
        &host,
        &["hook", "SessionEnd"],
        &json!({"session_id":"ending"}),
    );
    assert!(output.stdout.is_empty());
    good(output);
    assert!(fs::read_to_string(daily(&host))
        .unwrap()
        .contains("changed.txt"));
    assert!(!host.0.join("journal/ending.tsv").exists());
    edit(&host, "retained", "next.txt");
    fs::write(host.0.join("journal/retained.tsv"), "invalid\n").unwrap();
    let output = call(
        &host,
        &["hook", "SessionEnd"],
        &json!({"session_id":"retained"}),
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("session-end journal failed"));
    good(output);
    assert_eq!(
        fs::read_to_string(host.0.join("journal/retained.tsv")).unwrap(),
        "invalid\n"
    );
    let output = call(&host, &["hook", "SubagentStop"], &json!({}));
    assert!(output.stdout.is_empty());
    good(output);
}

#[test]
fn bounded_daily_publication_preserves_existing_day_and_accumulator_on_overflow() {
    let host = support::Host::new();
    good(journal(&host, "initial", "initial"));
    let day = daily(&host);
    let original = "x".repeat(8 * 1024 * 1024);
    fs::write(&day, &original).unwrap();
    edit(&host, "ending", "changed.txt");
    assert!(!journal(&host, "ending", "finished").status.success());
    assert_eq!(fs::read_to_string(&day).unwrap(), original);
    assert!(host.0.join("journal/ending.tsv").is_file());
    assert!(
        !fs::read_dir(host.0.join("vault/claude")).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("journal-pending"))
    );
}
