#![cfg(target_os = "linux")]
#[path = "support/forge_binary.rs"]
mod forge_binary;

use forge::candidate_receipt;
use quick_xml::{events::Event, Reader, XmlVersion};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::symlink, path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(forge::resource_id("candidate-report-cli").unwrap());
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn command(&self) -> Command {
        let mut command = Command::new(forge_binary::path());
        command
            .args(["candidate", "report"])
            .current_dir(&self.0)
            .env("PATH", "");
        command
    }
    fn write(&self, payload: &Value) {
        let receipt = candidate_receipt::seal(&serde_json::to_vec(payload).unwrap()).unwrap();
        fs::write(
            self.0.join("receipt.json"),
            serde_json::to_vec(receipt.payload()).unwrap(),
        )
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn payload() -> Value {
    json!({"schema_version":1,"surface":"manual","profile":"fast","decision":"fail",
        "candidate":{"tree_oid":"a".repeat(40)},"policy":{"block_at":"medium"},
        "engine":{},"graph":{},"bypass":{},"baselines":[],"binding":"b".repeat(64),
        "timings":{"duration_ms":1234},"cache":{"hits":0,"misses":1},
        "findings":[{"check_id":"scan","rule_id":"bad","severity":"medium",
            "message":"雪<&>\"'","path":"src/a b.rs","line":2,"column":3}],
        "checks":[{"check_id":"scan<&>\"'\n雪","status":"failed","duration_ms":1234,
            "findings":[],"stdout_tail":"out<&>\"'\n雪","stderr_tail":""}]})
}

#[test]
fn exports_failed_receipts_without_interpreters_or_approval() {
    let root = Fixture::new();
    root.write(&payload());
    let output = root.command().arg("receipt.json").output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let human = String::from_utf8(output.stdout).unwrap();
    assert!(human.contains("recorded decision=fail"));
    assert!(human.contains("execution=unverified"));
    assert!(human.contains("blocking=1"));
    let output = root
        .command()
        .args([
            "receipt.json",
            "--format",
            "json",
            "--output",
            "export.json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let before =
        candidate_receipt::parse_and_verify(&fs::read(root.0.join("receipt.json")).unwrap())
            .unwrap();
    let after = candidate_receipt::parse_and_verify(&fs::read(root.0.join("export.json")).unwrap())
        .unwrap();
    assert_eq!(before.digest(), after.digest());
    assert_eq!(before.payload(), after.payload());
}

#[test]
fn sarif_and_junit_are_parseable_and_preserve_recorded_status() {
    let root = Fixture::new();
    root.write(&payload());
    let output = root
        .command()
        .args(["receipt.json", "--format", "sarif"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let sarif: Value = serde_json::from_slice(&output.stdout).unwrap();
    let run = &sarif["runs"][0];
    assert_eq!(run["properties"]["executionVerified"], false);
    assert_eq!(
        run["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
        "src/a%20b.rs"
    );
    let output = root
        .command()
        .args(["receipt.json", "--format", "junit"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let xml = String::from_utf8(output.stdout).unwrap();
    let mut reader = Reader::from_str(&xml);
    let mut tests = 0;
    let mut failures = 0;
    loop {
        match reader.read_event().unwrap() {
            Event::Start(tag) if tag.name().as_ref() == b"testcase" => {
                tests += 1;
                let attr = tag
                    .attributes()
                    .map(Result::unwrap)
                    .find(|a| a.key.as_ref() == b"name")
                    .unwrap();
                let name = attr
                    .decoded_and_normalized_value(XmlVersion::Explicit1_0, reader.decoder())
                    .unwrap();
                assert_eq!(
                    name,
                    if tests == 1 {
                        "scan<&>\"'\n雪"
                    } else {
                        "recorded-review-verdict"
                    }
                );
            }
            Event::Start(tag) if tag.name().as_ref() == b"failure" => failures += 1,
            Event::Eof => break,
            _ => (),
        }
    }
    assert_eq!((tests, failures), (2, 2));
}

#[test]
fn invalid_receipts_and_unsafe_destinations_never_clobber_outputs() {
    let root = Fixture::new();
    root.write(&payload());
    fs::write(root.0.join("sentinel"), b"keep").unwrap();
    symlink(root.0.join("sentinel"), root.0.join("link")).unwrap();
    let bad_target = root
        .command()
        .args(["receipt.json", "--output", "link"])
        .output()
        .unwrap();
    assert!(!bad_target.status.success());
    assert_eq!(fs::read(root.0.join("sentinel")).unwrap(), b"keep");
    let mut tampered: Value =
        serde_json::from_slice(&fs::read(root.0.join("receipt.json")).unwrap()).unwrap();
    tampered["decision"] = json!("pass");
    fs::write(
        root.0.join("receipt.json"),
        serde_json::to_vec(&tampered).unwrap(),
    )
    .unwrap();
    let output = root
        .command()
        .args(["receipt.json", "--output", "sentinel"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(fs::read(root.0.join("sentinel")).unwrap(), b"keep");
    assert!(fs::read_dir(&root.0).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains("pending")));
}

#[test]
fn xml_invalid_text_fails_before_publication_and_pass_record_is_not_a_gate() {
    let root = Fixture::new();
    let mut data = payload();
    data["decision"] = json!("pass");
    data["checks"][0]["stdout_tail"] = json!("bad\u{1b}[2J");
    root.write(&data);
    fs::write(root.0.join("output"), b"keep").unwrap();
    let output = root
        .command()
        .args(["receipt.json", "--format", "junit", "--output", "output"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(root.0.join("output")).unwrap(), b"keep");
    let output = root.command().arg("receipt.json").output().unwrap();
    assert!(output.status.success());
    let human = String::from_utf8(output.stdout).unwrap();
    assert!(human.contains("recorded decision=pass"));
    assert!(human.contains("execution=unverified"));
    assert!(human.contains("blocking=1"));
}
