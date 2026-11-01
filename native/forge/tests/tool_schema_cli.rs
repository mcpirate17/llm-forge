#[path = "support/forge_binary.rs"]
mod forge_binary;
use forge::tool_schema::MAX_INPUT_BYTES;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new(contents: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "forge-schema-{}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, contents).unwrap();
        Self(path)
    }

    fn command(&self, action: &str, flag: &str) -> Command {
        let mut command = Command::new(forge_binary::path());
        command
            .env("PATH", "")
            .args(["schema", action, flag])
            .arg(&self.0);
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_file(&self.0).unwrap();
    }
}

fn run(mut command: Command, input: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn cli_validates_and_repairs_without_any_interpreter_on_path() {
    let file = Fixture::new(r#"{"type":"integer","minimum":1,"maximum":9007199254740993}"#);
    let valid = run(file.command("validate", "--schema"), b"9007199254740993");
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );
    assert!(String::from_utf8(valid.stdout)
        .unwrap()
        .contains("9007199254740993"));
    let invalid = run(file.command("validate", "--schema"), b"9007199254740994");
    assert_eq!(invalid.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&invalid.stdout).unwrap()["error"]["code"],
        "number_range"
    );
    let mut repair = file.command("validate", "--schema");
    repair.arg("--repair-fences");
    let repaired = run(repair, b"```json\n10\n```");
    assert!(repaired.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&repaired.stdout).unwrap()["repaired"],
        true
    );
}

#[test]
fn server_reuses_catalog_and_recovers_after_invalid_utf8_and_oversized_records() {
    let file = Fixture::new(
        r#"{"tools":[{"name":"noop","inputSchema":{"type":"object","additionalProperties":false}}]}"#,
    );
    let mut input = Vec::new();
    input.extend_from_slice(b"{\"name\":\"noop\",\"arguments\":{}}\n");
    input.extend_from_slice(b"{\"name\":\"unknown\",\"arguments\":{}}\n");
    input.extend_from_slice(&[0xff, b'\n']);
    input.extend(std::iter::repeat_n(b'x', MAX_INPUT_BYTES + 500));
    input.extend_from_slice(b"\n{\"arguments\":{},\"name\":\"noop\"}");
    let output = run(file.command("serve", "--catalog"), &input);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0]["valid"], true);
    assert_eq!(rows[1]["error"]["code"], "unknown_tool");
    assert_eq!(rows[2]["error"]["code"], "utf8");
    assert_eq!(rows[3]["error"]["code"], "input_limit");
    assert_eq!(rows[4]["valid"], true);
}

#[test]
fn cli_generates_grammar_and_fails_closed_on_invalid_registration() {
    let file = Fixture::new(r#"{"type":"string","enum":["read","write"]}"#);
    let grammar = file.command("grammar", "--schema").output().unwrap();
    assert!(grammar.status.success());
    assert!(String::from_utf8(grammar.stdout)
        .unwrap()
        .contains("root ::= ws n-0"));
    let catalog = Fixture::new(
        r#"{"tools":[{"name":"noop","inputSchema":{"type":"object","additionalProperties":false}}]}"#,
    );
    let calls = catalog.command("grammar", "--catalog").output().unwrap();
    assert!(calls.status.success());
    assert!(String::from_utf8(calls.stdout)
        .unwrap()
        .contains("root ::= ws tool-call"));
    let mut conflicting = catalog.command("grammar", "--catalog");
    conflicting.arg("--schema").arg(&file.0);
    assert_eq!(conflicting.output().unwrap().status.code(), Some(2));
    let file = Fixture::new(r#"{"type":"string","format":"email"}"#);
    let error = file.command("grammar", "--schema").output().unwrap();
    assert_eq!(error.status.code(), Some(2));
    assert!(error.stdout.is_empty());
    assert!(String::from_utf8(error.stderr)
        .unwrap()
        .contains("unsupported keyword"));
}
