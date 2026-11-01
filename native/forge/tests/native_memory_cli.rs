#[allow(dead_code)]
mod support;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::process::Output;

fn fixture() -> support::Host {
    let host = support::Host::new();
    fs::set_permissions(&host.0, fs::Permissions::from_mode(0o700)).unwrap();
    let fingerprint = format!("sha256:{}", "a".repeat(64));
    fs::write(host.0.join("query.json"),json!({"schema_version":1,"task_sha256":format!("{:x}",Sha256::digest(b"native")),"embedding_fingerprint":fingerprint,"vector":[1.0,0.0]}).to_string()).unwrap();
    let rows:Vec<_>=(0..8).map(|i|json!({"schema_version":3,"embedding":{"fingerprint":fingerprint,"dimension":2,"paid":true},
        "source_sha256":format!("{i:064x}"),"source":"notes","path":format!("row-{i}.md"),"title":format!("row {i}"),"text":"context text", "vector":[1.0-i as f64/8.0,i as f64/8.0]}).to_string()).collect();
    fs::write(host.0.join("memory.jsonl"), rows.join("\n")).unwrap();
    host
}
fn cli(host: &support::Host, args: &[&str]) -> Output {
    host.command()
        .env("PATH", "")
        .env("CONDUCTOR_PYTHON", "/forbidden")
        .arg("memory")
        .args(args)
        .output()
        .unwrap()
}
fn good(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    serde_json::from_slice(&output.stdout).unwrap()
}
fn bad(output: Output, reason: &str) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(reason),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn build(host: &support::Host) -> Value {
    good(cli(host, &["build", "--source", "memory.jsonl"]))
}
fn query(host: &support::Host, extra: &[&str]) -> Output {
    let mut args = vec![
        "query",
        "--source",
        "memory.jsonl",
        "--task",
        "native",
        "--query",
        "query.json",
    ];
    args.extend(extra);
    cli(host, &args)
}

#[test]
fn native_build_mapped_query_and_full_verify_share_the_same_snapshot() {
    let host = fixture();
    let built = build(&host);
    assert_eq!(built["metadata"]["schema_version"], 2);
    assert_eq!(built["metadata"]["rows"], 8);
    let result = good(query(&host, &["--top-k", "3"]));
    assert_eq!(result["mode"], "mapped-f32");
    assert_eq!(result["hits"].as_array().unwrap().len(), 3);
    assert_eq!(result["hits"][0]["path"], "row-0.md");
    assert_eq!(
        good(cli(&host, &["verify", "--source", "memory.jsonl"]))["verified"],
        true
    );
    let path = host.0.join("memory.jsonl.sidecar");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o400
    );
    let before = fs::read(&path).unwrap();
    good(query(&host, &[]));
    assert_eq!(before, fs::read(path).unwrap());
}

#[test]
fn missing_snapshots_fail_without_creating_files_or_scanning_the_source() {
    let host = fixture();
    let before = fs::read_dir(&host.0).unwrap().count();
    bad(query(&host, &[]), "explicitly run");
    assert_eq!(fs::read_dir(&host.0).unwrap().count(), before);
    bad(query(&host, &["--scan"]), "unexpected argument");
    assert_eq!(fs::read_dir(&host.0).unwrap().count(), before);
}

#[test]
fn changed_sources_fail_without_rebuilding_or_overwriting_the_snapshot() {
    let host = fixture();
    build(&host);
    let path = host.0.join("memory.jsonl.sidecar");
    let before = fs::read(&path).unwrap();
    let source = host.0.join("memory.jsonl");
    let bytes = fs::read_to_string(&source)
        .unwrap()
        .replace("context text", "changed text");
    fs::write(source, bytes).unwrap();
    bad(query(&host, &[]), "stale");
    bad(cli(&host, &["verify", "--source", "memory.jsonl"]), "stale");
    assert_eq!(before, fs::read(&path).unwrap());
    build(&host);
    assert_eq!(good(query(&host, &[]))["hits"][0]["text"], "changed text");
}

#[test]
fn explicit_outputs_are_supported_but_source_aliases_and_symlinks_are_rejected() {
    let host = fixture();
    let source = fs::read(host.0.join("memory.jsonl")).unwrap();
    bad(
        cli(
            &host,
            &[
                "build",
                "--source",
                "memory.jsonl",
                "--output",
                "memory.jsonl",
            ],
        ),
        "aliases source",
    );
    let target = host.0.join("untouched");
    fs::write(&target, b"keep").unwrap();
    symlink(&target, host.0.join("link")).unwrap();
    bad(
        cli(
            &host,
            &["build", "--source", "memory.jsonl", "--output", "link"],
        ),
        "unsafe snapshot",
    );
    assert_eq!(fs::read(target).unwrap(), b"keep");
    good(cli(
        &host,
        &[
            "build",
            "--source",
            "memory.jsonl",
            "--output",
            "chosen.snapshot",
        ],
    ));
    assert_eq!(
        good(query(&host, &["--sidecar", "chosen.snapshot"]))["mode"],
        "mapped-f32"
    );
    assert_eq!(source, fs::read(host.0.join("memory.jsonl")).unwrap());
}

#[test]
fn malformed_requests_fail_before_printing_or_creating_outputs() {
    let host = fixture();
    build(&host);
    for count in ["0", "65", "18446744073709551615"] {
        bad(query(&host, &["--top-k", count]), "1..64");
    }
    bad(
        cli(
            &host,
            &[
                "query",
                "--source",
                "memory.jsonl",
                "--query",
                "query.json",
                "--task",
                "wrong",
            ],
        ),
        "bind this task",
    );
    let output = query(&host, &["--scan", "--sidecar", "chosen"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let path = host.0.join("memory.jsonl.sidecar");
    let before = fs::read(&path).unwrap();
    fs::write(host.0.join("memory.jsonl"), b"not-json").unwrap();
    bad(
        cli(&host, &["build", "--source", "memory.jsonl"]),
        "expected",
    );
    assert_eq!(before, fs::read(path).unwrap());
}
