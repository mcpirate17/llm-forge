#[path = "support/tokenizer_fixture.rs"]
mod fixture;
#[path = "support/forge_binary.rs"]
mod forge_binary;
use conductor_native::syntax::Language;
use fixture::Fixture;
use forge::context_store::{Store, DEFAULT_QUOTA};
use forge::context_window::{compact, Entry, Kind, Request};
use forge::tokenizer::Counter;
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn entry(id: &str, kind: Kind, content: impl Into<String>) -> Entry {
    Entry {
        id: id.into(),
        kind,
        content: content.into(),
        protected: false,
        exchange: None,
        language: None,
    }
}
fn request(entries: Vec<Entry>, tokens: usize) -> Request {
    Request {
        entries,
        max_tokens: tokens,
        max_bytes: 16384,
        reserved_tokens: 10,
        keep_recent: 1,
    }
}
fn setup() -> (Fixture, Counter, Store) {
    let fixture = Fixture::new();
    let counter = Counter::load(&fixture.config).unwrap();
    let store = Store::create(&fixture.root.join("recovery"), DEFAULT_QUOTA).unwrap();
    (fixture, counter, store)
}
fn recover(store: &Store, hash: &str) -> serde_json::Value {
    serde_json::from_slice(&store.get(hash).unwrap()).unwrap()
}

#[test]
fn fitting_text_is_exact_and_does_not_create_recovery_artifacts() {
    let (_fixture, counter, store) = setup();
    let req = request(
        vec![
            entry("a", Kind::Instructions, "hello"),
            entry("b", Kind::Text, " world!"),
        ],
        100,
    );
    let output = compact(&counter, &store, &req).unwrap();
    assert_eq!(output.text, "hello\n\n world!");
    assert_eq!(output.output_tokens, counter.count(&output.text).unwrap());
    assert!(!output.protected_overflow);
    assert!(output.recovery_sha256.is_none());
    assert!(output.reductions.is_empty());
    assert_eq!(output.input_bytes, output.output_bytes);
}

#[test]
fn sliding_context_preserves_policy_recent_groups_and_complete_tool_exchanges() {
    let (_fixture, counter, store) = setup();
    let mut call = entry("call", Kind::ToolCall, "old call ".repeat(50));
    let mut result = entry("result", Kind::ToolResult, "old result ".repeat(50));
    call.exchange = Some("exchange-1".into());
    result.exchange = call.exchange.clone();
    let req = request(
        vec![
            entry("policy", Kind::Policy, "RETAIN EXACT POLICY 🦀"),
            entry("old", Kind::Text, "old text ".repeat(100)),
            call,
            result,
            entry("latest", Kind::Text, "latest request"),
        ],
        250,
    );
    let out = compact(&counter, &store, &req).unwrap();
    assert!(!out.protected_overflow, "{}", out.text);
    assert!(out.output_tokens + out.reserved_tokens <= req.max_tokens);
    assert!(out.text.starts_with("RETAIN EXACT POLICY 🦀"));
    assert!(out.text.contains("latest request"));
    assert_eq!(out.omitted_ids, ["old", "call", "result"]);
    assert_eq!(out.output_tokens, counter.count(&out.text).unwrap());
    let original = recover(&store, out.recovery_sha256.as_ref().unwrap());
    assert_eq!(original["request"], serde_json::to_value(&req).unwrap());
    assert_eq!(
        compact(&counter, &store, &req).unwrap().recovery_sha256,
        out.recovery_sha256
    );
}

#[test]
fn ast_folding_preserves_signatures_and_explicit_protection_overflows_without_loss() {
    let (_fixture, counter, store) = setup();
    let source = format!(
        "pub fn answer() -> usize {{\n{}42\n}}",
        "let _ = \"東京🦀\";\n".repeat(100)
    );
    let mut code = entry("code", Kind::Code, source.clone());
    code.language = Some(Language::Rust);
    let mut req = request(vec![code], 350);
    let folded = compact(&counter, &store, &req).unwrap();
    assert!(!folded.protected_overflow);
    assert!(folded.text.contains("pub fn answer() -> usize"));
    assert!(folded.text.contains("body omitted"));
    assert!(!folded.text.contains("東京"));
    assert_eq!(
        recover(&store, folded.recovery_sha256.as_ref().unwrap())["request"]["entries"][0]
            ["content"],
        source
    );
    req.entries[0].protected = true;
    let exact = compact(&counter, &store, &req).unwrap();
    assert!(exact.protected_overflow);
    assert!(exact.text.starts_with(&source));
    assert!(exact.reductions.is_empty());
    req.entries[0].protected = false;
    req.entries[0].content = "fn broken( {".repeat(100);
    let invalid = compact(&counter, &store, &req).unwrap();
    assert!(invalid.protected_overflow);
    assert!(invalid.reductions[0].error.is_some());
    assert!(invalid.text.starts_with(&req.entries[0].content));
}

#[test]
fn compiler_errors_become_required_and_diff_projections_keep_every_changed_line() {
    let (_fixture, counter, store) = setup();
    let log=format!("{}\x1b[31merror[E0308]: wrong type\x1b[0m\n --> src/main.rs:5:9\n 5 | foo()\n   | ^^^^^ expected u32\n{}", "Compiling dep\n".repeat(100), "progress\n".repeat(100));
    let req = request(
        vec![
            entry("log", Kind::Log, log),
            entry("latest", Kind::Text, "fix the error"),
        ],
        600,
    );
    let out = compact(&counter, &store, &req).unwrap();
    assert!(!out.protected_overflow, "{}", out.text);
    assert!(out.text.contains("error[E0308]"));
    assert!(out.text.contains("src/main.rs:5:9"));
    assert!(!out.omitted_ids.contains(&"log".into()));
    assert_eq!(out.reductions[0].diagnostic_errors, 1);
    let patch = format!(
        "--- a/file\n+++ b/file\n@@ -1,202 +1,202 @@\n{}-old\n+new\n",
        " unchanged line\n".repeat(200)
    );
    let out = compact(
        &counter,
        &store,
        &request(vec![entry("diff", Kind::Diff, patch)], 350),
    )
    .unwrap();
    assert!(!out.protected_overflow);
    assert!(out.text.contains("-old\n+new\n"));
    assert!(out.text.contains("exact applicable patch in recovery"));
}

#[test]
fn malformed_groups_and_budget_overflows_fail_before_any_spill() {
    let (_fixture, counter, store) = setup();
    let valid = request(vec![entry("one", Kind::Text, "one")], 100);
    let mut cases = vec![];
    let mut r = valid.clone();
    r.entries.push(r.entries[0].clone());
    cases.push(r);
    let mut r = valid.clone();
    r.max_tokens = 0;
    cases.push(r);
    let mut r = valid.clone();
    r.reserved_tokens = usize::MAX;
    cases.push(r);
    let mut r = valid.clone();
    r.entries[0].kind = Kind::ToolResult;
    cases.push(r);
    let mut r = valid.clone();
    r.entries[0].kind = Kind::ToolResult;
    r.entries[0].exchange = Some("bad".into());
    cases.push(r);
    let mut r = valid.clone();
    r.entries[0].kind = Kind::Code;
    cases.push(r);
    let mut r = valid.clone();
    r.keep_recent = 2;
    cases.push(r);
    let mut r = valid.clone();
    r.entries[0].exchange = Some("group".into());
    r.entries.push(entry("middle", Kind::Text, "middle"));
    let mut tail = entry("tail", Kind::Text, "tail");
    tail.exchange = Some("group".into());
    r.entries.push(tail);
    cases.push(r);
    for r in cases {
        assert!(compact(&counter, &store, &r).is_err(), "{r:?}");
    }
}

#[test]
fn quota_corruption_permissions_and_concurrent_readers_preserve_recovery_integrity() {
    let (fixture, counter, store) = setup();
    let req = request(vec![entry("text", Kind::Text, "x".repeat(500))], 200);
    let small = Store::open(&fixture.root.join("recovery"), 1).unwrap();
    assert!(compact(&counter, &small, &req).is_err());
    let out = compact(&counter, &store, &req).unwrap();
    let hash = out.recovery_sha256.unwrap();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let store = &store;
            let hash = &hash;
            scope.spawn(move || {
                for _ in 0..20 {
                    assert_eq!(recover(store, hash)["version"], 1);
                }
            });
        }
    });
    assert!(store.get("../escape").is_err());
    let path = fixture.root.join("recovery").join(format!("{hash}.json"));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(store.get(&hash).is_err());
    fs::write(&path, "corrupted").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    assert!(store.get(&hash).is_err());
    assert!(compact(&counter, &store, &req).is_err());
}

#[test]
fn native_cli_compacts_recovers_and_reports_protected_overflow_with_empty_path() {
    let (fixture, _, _) = setup();
    let config = fixture.root.join("config.json");
    let input = fixture.root.join("request.json");
    let recovery = fixture.root.join("recovery");
    fs::write(&config, serde_json::to_vec(&fixture.config).unwrap()).unwrap();
    let req = request(vec![entry("policy", Kind::Policy, "z".repeat(500))], 200);
    fs::write(&input, serde_json::to_vec(&req).unwrap()).unwrap();
    let out = std::process::Command::new(forge_binary::path())
        .env("PATH", "")
        .args(["context", "compact", "--tokenizer"])
        .arg(config)
        .arg("--request")
        .arg(input)
        .arg("--spill-dir")
        .arg(&recovery)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["protected_overflow"], true);
    let out = std::process::Command::new(forge_binary::path())
        .env("PATH", "")
        .args(["context", "recover", "--spill-dir"])
        .arg(recovery)
        .arg("--sha256")
        .arg(value["recovery_sha256"].as_str().unwrap())
        .output()
        .unwrap();
    assert!(out.status.success());
    let recovered: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(recovered["request"], serde_json::to_value(req).unwrap());
}

#[test]
fn varied_unicode_histories_respect_final_token_and_byte_budgets() {
    let (_fixture, counter, store) = setup();
    for seed in 0..64 {
        let mut entries = vec![entry(
            "policy",
            Kind::Instructions,
            "KEEP EXACT INSTRUCTIONS",
        )];
        for i in 0..8 {
            entries.push(entry(
                &format!("old-{i}"),
                Kind::Text,
                ["hello world! ", "東京🦀 ", "aaaaa ", "line\n"][i % 4]
                    .repeat((seed * 13 + i * 7) % 40),
            ));
        }
        entries.push(entry("latest", Kind::Text, "latest request"));
        let mut req = request(entries, 256 + seed * 7);
        req.max_bytes = 256 + seed * 3;
        let output = compact(&counter, &store, &req).unwrap();
        assert!(!output.protected_overflow, "seed {seed}: {}", output.text);
        assert_eq!(output.output_tokens, fixture::reference(&output.text).len());
        assert!(output.output_tokens + req.reserved_tokens <= req.max_tokens);
        assert!(output.text.len() <= req.max_bytes);
        assert!(output.text.starts_with("KEEP EXACT INSTRUCTIONS"));
        assert!(output.text.contains("latest request"));
    }
}
