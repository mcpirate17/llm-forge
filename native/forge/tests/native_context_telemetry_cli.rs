#[allow(dead_code)]
mod support;
use conductor_native::context_telemetry_aggregate::{summarize, summarize_bytes};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

fn command(host: &support::Host, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
    command
        .env_clear()
        .env("PATH", "")
        .env("HOME", &host.0)
        .env("LEDGER_ROOT", host.0.join("ledger"))
        .env("CONTEXT_TELEMETRY_PATH", host.0.join("events.jsonl"))
        .current_dir(&host.0)
        .args(["context", "telemetry"])
        .args(args);
    command
}

fn run(host: &support::Host, args: &[&str], input: &[u8]) -> Output {
    forge::process_probe::run_with_input(
        &mut command(host, args),
        input,
        Duration::from_secs(5),
        1024 * 1024,
    )
    .unwrap()
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn records(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn recording_works_with_empty_path_and_retains_only_event_metadata() {
    let host = support::Host::new();
    let payload = json!({"hook_event_name":"PostToolUse","tool_name":"Read",
        "tool_input":{"path":"private"},"tool_response":"private body 🦀","session_id":"fixture"});
    for args in [vec![], vec!["record"]] {
        let output = run(&host, &args, payload.to_string().as_bytes());
        success(&output);
        assert!(output.stdout.is_empty());
    }
    let rows = records(&host.0.join("events.jsonl"));
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["event"], "PostToolUse");
    assert_eq!(rows[0]["tool"], "Read");
    assert_eq!(rows[0]["session_id"], "fixture");
    assert!(rows[0]["output_bytes"].as_u64().unwrap() > 0);
    assert!(!fs::read_to_string(host.0.join("events.jsonl"))
        .unwrap()
        .contains("private"));
}

#[test]
fn hook_tee_preserves_bytes_labels_hash_and_quiet_output() {
    let host = support::Host::new();
    let bytes = " {\n\"hookSpecificOutput\":{\"hookEventName\":\"SessionStart\",\"additionalContext\":\"秘密🦀\"}}\n".as_bytes();
    let output = run(
        &host,
        &[
            "hook-context",
            "--hook",
            "session-start",
            "--category",
            "instructions",
        ],
        bytes,
    );
    success(&output);
    assert_eq!(output.stdout, bytes);
    let rows = records(&host.0.join("events.jsonl"));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["tool"], "session-start");
    assert_eq!(rows[0]["category"], "instructions");
    assert_eq!(rows[0]["hook_event"], "SessionStart");
    assert_eq!(rows[0]["output_bytes"], 3);
    assert_eq!(rows[0]["content_hash"].as_str().unwrap().len(), 16);
    assert!(!rows[0].to_string().contains("秘密"));
    let quiet = b"{}\n";
    let output = run(&host, &["hook-context", "--hook", "quiet"], quiet);
    success(&output);
    assert_eq!(output.stdout, quiet);
    assert_eq!(records(&host.0.join("events.jsonl")).len(), 1);
}

#[test]
fn malformed_tee_remains_byte_exact_but_recording_errors_are_explicit() {
    let host = support::Host::new();
    for bytes in [b"not JSON\n".as_slice(), b"\xff\n".as_slice()] {
        let output = run(&host, &["hook-context", "--hook", "fixture"], bytes);
        assert!(!output.status.success());
        assert_eq!(output.stdout, bytes);
        assert!(!output.stderr.is_empty());
    }
    let output = run(&host, &["record"], b"{bad}");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!host.0.join("events.jsonl").exists());
    for bytes in [b"[]".as_slice(), b"null".as_slice(), b"42".as_slice()] {
        let output = run(&host, &["record"], bytes);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("must be a JSON object"));
    }
}

#[test]
fn failed_sink_exits_nonzero_and_hook_tee_still_preserves_bytes() {
    let host = support::Host::new();
    let sink = host.0.join("sink-directory");
    fs::create_dir(&sink).unwrap();
    let bytes = b" {\"hookSpecificOutput\":{\"additionalContext\":\"retained context\"}}\n";
    for args in [vec!["record"], vec!["hook-context", "--hook", "fixture"]] {
        let mut cmd = command(&host, &args);
        cmd.env("CONTEXT_TELEMETRY_PATH", &sink);
        let output =
            forge::process_probe::run_with_input(&mut cmd, bytes, Duration::from_secs(5), 4096)
                .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("write telemetry"));
        if args[0] == "hook-context" {
            assert_eq!(output.stdout, bytes);
        } else {
            assert!(output.stdout.is_empty());
        }
    }
}

#[test]
fn summary_uses_bounded_buffers_with_native_aggregation_and_default_sink() {
    let host = support::Host::new();
    let log = host.0.join("events.jsonl");
    let bytes = b"{\"event\":\"PostToolUse\",\"tool\":\"Read\",\"output_bytes\":12}\ninvalid\n[]\n\xff\n{\"event\":\"HookContext\",\"tool\":\"start\",\"output_bytes\":8}\n";
    fs::write(&log, bytes).unwrap();
    let expected = summarize(&[log.to_str().unwrap().into()], 10).unwrap();
    assert_eq!(
        summarize_bytes(&[(log.to_str().unwrap(), bytes)], 10),
        expected
    );
    let output = run(&host, &["summary", "--bound-bytes", "10", "--json"], b"");
    success(&output);
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        expected
    );
    assert_eq!(expected["events"], 2);
    assert_eq!(expected["output_bytes"], 20);
    assert_eq!(expected["rows"][0]["over_bound_bytes"], 2);
    let output = run(&host, &["summary", log.to_str().unwrap()], b"");
    success(&output);
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("PostToolUse"));
}

#[test]
fn oversized_stdin_labels_files_and_nonregular_summary_inputs_fail_boundedly() {
    let host = support::Host::new();
    let input = host.0.join("oversized-input");
    fs::write(&input, vec![b' '; 1024 * 1024 + 1]).unwrap();
    let mut cmd = command(&host, &["hook-context", "--hook", "fixture"]);
    cmd.stdin(Stdio::from(fs::File::open(&input).unwrap()));
    // A regular file supplies finite oversized stdin; process_probe's optional
    // input adapter intentionally replaces a preconfigured stdin descriptor.
    let output = cmd.output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("exceeds 1 MiB"));
    let output = run(&host, &["hook-context", "--hook", &"h".repeat(101)], b"{}");
    assert!(!output.status.success());
    let log = host.0.join("events.jsonl");
    fs::write(&log, b"{}\n").unwrap();
    let mut args = vec!["summary"];
    args.extend(std::iter::repeat_n(log.to_str().unwrap(), 33));
    let output = run(&host, &args, b"");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("exceeds 32 files"));
    let huge = host.0.join("oversized-log");
    fs::File::create(&huge)
        .unwrap()
        .set_len(10 * 1024 * 1024 + 1)
        .unwrap();
    let output = run(&host, &["summary", huge.to_str().unwrap()], b"");
    assert!(!output.status.success());
    fs::File::options()
        .write(true)
        .open(&huge)
        .unwrap()
        .set_len(10 * 1024 * 1024)
        .unwrap();
    let mut args = vec!["summary"];
    args.extend(std::iter::repeat_n(huge.to_str().unwrap(), 7));
    let output = run(&host, &args, b"");
    assert!(!output.status.success());
    let link = host.0.join("log-link");
    std::os::unix::fs::symlink(&log, &link).unwrap();
    for path in [&link, &host.0] {
        let output = run(&host, &["summary", path.to_str().unwrap()], b"");
        assert!(!output.status.success());
    }
}
