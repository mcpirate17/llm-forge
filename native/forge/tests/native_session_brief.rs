//! All CLI fixtures run with an empty PATH and invalid interpreter selection.
#[allow(dead_code)]
mod support;
use conductor_native::a2a_store::Store;
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::process::Output;
use std::time::{SystemTime, UNIX_EPOCH};

fn cli(host: &support::Host, task: &str, args: &[&str]) -> Output {
    host.command()
        .env("PATH", "")
        .env("CONDUCTOR_PYTHON", "/forbidden")
        .env_remove("CONDUCTOR_NOTES_DB")
        .env_remove("A2A_AGENT_NAME")
        .args(["session", "brief", "--task", task])
        .args(args)
        .output()
        .unwrap()
}
fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    String::from_utf8(output.stdout).unwrap()
}
fn refused(output: Output, reason: &str) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains(reason), "{error}");
    assert!(!error.contains("panicked"), "{error}");
}
fn utc(seconds: i64) -> String {
    let mut date = std::mem::MaybeUninit::<libc::tm>::uninit();
    assert!(!unsafe { libc::gmtime_r(&seconds, date.as_mut_ptr()) }.is_null());
    let date = unsafe { date.assume_init() };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
        date.tm_year + 1900,
        date.tm_mon + 1,
        date.tm_mday,
        date.tm_hour,
        date.tm_min,
        date.tm_sec
    )
}
fn claim(path: &str, owner: &str, delta: i64) -> Value {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut fields = json!({"owner":owner,"paths":[path],"justification":"fixture work",
        "created_at":utc(now - 60 + delta),"expires_at":utc(now + 3600 + delta)});
    fields.sort_all_objects();
    let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(&fields).unwrap()));
    fields["claim_id"] = json!(format!("claim-{}", &hash[..20]));
    fields
}
fn claims(host: &support::Host, claims: Vec<Value>) -> std::path::PathBuf {
    let dir = host.0.join(".git/governance");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("ownership-claims.json");
    fs::write(
        &path,
        json!({"schema_version":1,"claims":claims}).to_string(),
    )
    .unwrap();
    path
}
fn notes(host: &support::Host) -> std::path::PathBuf {
    fs::write(
        host.0.join("forge.toml"),
        "schema_version=1\n[conductor]\nnotes_db='custom.db'\n",
    )
    .unwrap();
    let path = host.0.join("custom.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE VIRTUAL TABLE notes_fts USING fts5(path UNINDEXED, source UNINDEXED, title, body, mtime UNINDEXED);
        INSERT INTO notes_fts VALUES('tasks/one.md','tasks','Native task','compiler native tools',0);
        INSERT INTO notes_fts VALUES('notes/private.md','notes','Non-task note','compiler native tools',0);").unwrap();
    path
}
fn semantic(host: &support::Host, task: &str) -> Value {
    fs::set_permissions(&host.0, fs::Permissions::from_mode(0o700)).unwrap();
    let metadata = json!({"fingerprint":format!("sha256:{}", "a".repeat(64)),"dimension":2,"paid":false,"num_gpu":0,"num_ctx":2048});
    let query = json!({"schema_version":1,"task_sha256":format!("{:x}", Sha256::digest(task.trim().as_bytes())),
        "embedding_fingerprint":metadata["fingerprint"],"vector":[2.0,0.0]});
    fs::write(host.0.join("query.json"), query.to_string()).unwrap();
    let memory: Vec<_> = (0..5).map(|i| json!({"schema_version":3,"embedding":metadata,"source_sha256":"b".repeat(64),
        "source":"notes","path":format!("memory/{i}.md"),"title":format!("memory-{i}"),
        "text":"---\nhidden: metadata\n---\n Semantic   memory 🦀", "vector":[1.0 - i as f64 / 10.0,0.0]}).to_string()).collect();
    fs::write(host.0.join("memory.jsonl"), memory.join("\n")).unwrap();
    conductor_native::memory_index_sidecar::build_sidecar(
        &host.0.join("memory.jsonl"),
        &host.0.join("memory.jsonl.sidecar"),
    )
    .unwrap();
    fs::write(host.0.join("cards.json"), json!({"schema_version":2,"embedding":metadata,"cards":[
        {"name":"KB-NATIVE","path":"cards/native.md","text":"---\nhidden: card\n---\nNative   guidance", "vector":[1.0,0.0]}]}).to_string()).unwrap();
    query
}

#[test]
fn missing_optional_sources_are_visible_and_create_no_state() {
    let host = support::Host::new();
    let text = success(cli(&host, "Native work", &[]));
    for needle in [
        "TASK: Native work",
        "MANDATES: none",
        "no paths requested",
        "no identity requested",
        "notes index missing",
        "SEMANTIC: not requested",
    ] {
        assert!(text.contains(needle), "{text}");
    }
    assert_eq!(fs::read_dir(&host.0).unwrap().count(), 1);
}

#[test]
fn task_frontmatter_is_preserved_and_semantic_artifacts_are_strict() {
    let host = support::Host::new();
    let task = "---\nKeep this task instruction\n---\nContinue work";
    let text = success(cli(&host, task, &[]));
    assert!(text.starts_with(&format!("TASK: {task}\n")), "{text}");
    semantic(&host, "native");
    let args = ["--query", "query.json", "--cards-index", "cards.json"];
    let query = fs::read_to_string(host.0.join("query.json")).unwrap();
    fs::write(
        host.0.join("query.json"),
        query.replacen('{', "{\"schema_version\":1,", 1),
    )
    .unwrap();
    refused(cli(&host, "native", &args), "duplicate");
    fs::write(host.0.join("query.json"), query).unwrap();
    let mut cards: Value =
        serde_json::from_slice(&fs::read(host.0.join("cards.json")).unwrap()).unwrap();
    cards["embedding"]["fingerprint"] = json!(format!("sha256:{}", "d".repeat(64)));
    fs::write(host.0.join("cards.json"), cards.to_string()).unwrap();
    refused(cli(&host, "native", &args), "fingerprint mismatch");
    cards["embedding"]["fingerprint"] = json!(format!("sha256:{}", "a".repeat(64)));
    cards["cards"][0]["vector"] = json!([1.0]);
    fs::write(host.0.join("cards.json"), cards.to_string()).unwrap();
    refused(cli(&host, "native", &args), "invalid card vector");
}

#[test]
fn briefs_keep_live_overlapping_claims_and_four_headings_without_reading_large_tail() {
    let host = support::Host::new();
    fs::write(host.0.join("forge.toml"), "schema_version=1\n[conductor.session]\npreamble=[]\nstanding_mandates=['KB-NATIVE: native tools']\n").unwrap();
    fs::write(
        host.0.join(".current_work.md"),
        format!(
            "## Active Coordination\n## One\n## Two\n## Three\n## Four\n{}",
            "x".repeat(2 * 1024 * 1024)
        ),
    )
    .unwrap();
    let path = claims(
        &host,
        vec![
            claim("src/core", "overlap-owner", 0),
            claim("src/core2", "other-owner", 0),
            claim("src/core", "expired-owner", -7200),
        ],
    );
    let before = fs::read(&path).unwrap();
    let text = success(cli(&host, "Native", &["--paths", "src/core/lib.rs"]));
    for needle in [
        "MANDATES: KB-NATIVE",
        "1 active overlaps",
        "overlap-owner",
        "HEADING: Four",
    ] {
        assert!(text.contains(needle), "{text}");
    }
    assert!(!text.contains("other-owner") && !text.contains("expired-owner"));
    assert_eq!(before, fs::read(path).unwrap());
    assert!(!host.0.join("conductor/active_state.json").exists());
}

#[test]
fn heading_scan_rejects_invalid_files_and_oversized_prefixes() {
    let host = support::Host::new();
    let path = host.0.join(".current_work.md");
    fs::write(&path, "x".repeat(8193)).unwrap();
    refused(cli(&host, "task", &[]), "line exceeds");
    fs::write(&path, "ignored\n".repeat(150000)).unwrap();
    refused(cli(&host, "task", &[]), "1 MiB");
    fs::remove_file(&path).unwrap();
    symlink("missing", &path).unwrap();
    refused(cli(&host, "task", &[]), "Too many levels");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    refused(cli(&host, "task", &[]), "regular file");
}

#[test]
fn corrupt_claims_do_not_become_no_claims_and_unicode_timestamps_never_panic() {
    let host = support::Host::new();
    let path = claims(&host, vec![]);
    for text in [
        "{\"schema_version\":1,\"schema_version\":1,\"claims\":[]}".into(),
        " ".repeat(1024 * 1024 + 1),
    ] {
        fs::write(&path, text).unwrap();
        refused(cli(&host, "task", &[]), "unreadable");
    }
    let mut row = claim("src/core", "owner", 0);
    row["created_at"] = json!("🦀🦀🦀🦀🦀🦀");
    claims(&host, vec![row]);
    refused(cli(&host, "task", &[]), "timezone");
    fs::remove_file(&path).unwrap();
    symlink("missing", &path).unwrap();
    refused(cli(&host, "task", &[]), "unreadable");
}

#[test]
fn configured_notes_use_task_filter_and_read_only_search() {
    let host = support::Host::new();
    let path = notes(&host);
    let before = fs::read(&path).unwrap();
    let text = success(cli(&host, "compiler", &[]));
    assert!(
        text.contains("Native task") && !text.contains("Non-task note"),
        "{text}"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let search = host
        .command()
        .env_remove("CONDUCTOR_NOTES_DB")
        .args(["notes", "search", "--host", ".", "compiler", "--json"])
        .output()
        .unwrap();
    let results: Value = serde_json::from_str(&success(search)).unwrap();
    assert_eq!(results.as_array().unwrap().len(), 2);
    assert!(!host.0.join("research/notes.db").exists());
}

#[test]
fn unsafe_or_corrupt_notes_are_errors_with_no_fallback() {
    let host = support::Host::new();
    let path = notes(&host);
    fs::write(&path, b"corrupt").unwrap();
    refused(cli(&host, "compiler", &[]), "database");
    fs::remove_file(&path).unwrap();
    symlink("missing", &path).unwrap();
    refused(cli(&host, "compiler", &[]), "regular file");
}

#[test]
fn inbox_uses_bounded_snapshot_without_read_or_presentation_acknowledgment() {
    let host = support::Host::new();
    let root = host.0.join(".agents/a2a");
    let mut store = Store::initialize(&root, "agent").unwrap();
    for i in 0..12 {
        store
            .record_inbound(
                &format!("id-{i}"),
                "sender",
                "agent",
                &"body ".repeat(10000),
                None,
                "2026-10-31T00:00:00+00:00",
            )
            .unwrap();
    }
    let text = success(cli(&host, "task", &["--identity", "agent"]));
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("INBOX (preview only): "))
        .unwrap();
    let envelope: Value = serde_json::from_str(line).unwrap();
    assert_eq!(envelope["total"], 12);
    assert!(envelope["shown"].as_u64().unwrap() <= 8);
    assert!(line.chars().count() <= 1200);
    assert!(text.chars().count() <= 3501);
    let conn = Connection::open(root.join("agent/store.sqlite")).unwrap();
    let unread: i64 = conn
        .query_row(
            "SELECT count(*) FROM messages WHERE read_at IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unread, 12);
    assert_eq!(success(cli(&host, "task", &["--identity", "agent"])), text);
}

#[test]
fn shared_task_bound_vector_retrieves_memory_and_cards_without_network() {
    let host = support::Host::new();
    semantic(&host, "native");
    let text = success(cli(
        &host,
        "native",
        &[
            "--query",
            "query.json",
            "--memory-index",
            "memory.jsonl",
            "--cards-index",
            "cards.json",
        ],
    ));
    assert!(
        text.contains("memory-0") && text.contains("memory-2") && !text.contains("memory-3"),
        "{text}"
    );
    assert!(
        text.contains("KB-NATIVE")
            && text.contains("Semantic memory 🦀")
            && !text.contains("hidden:"),
        "{text}"
    );
    assert_eq!(fs::read_dir(&host.0).unwrap().count(), 6);
}

#[test]
fn semantic_query_refuses_wrong_task_space_dimension_and_invalid_values() {
    let host = support::Host::new();
    let mut query = semantic(&host, "native");
    let args = [
        "--query",
        "query.json",
        "--memory-index",
        "memory.jsonl",
        "--cards-index",
        "cards.json",
    ];
    refused(cli(&host, "different", &args), "bind this task");
    for (field, value, error) in [
        ("vector", json!([0.0, 0.0]), "nonzero"),
        ("vector", json!([1.0]), "dim mismatch"),
        (
            "embedding_fingerprint",
            json!(format!("sha256:{}", "c".repeat(64))),
            "fingerprint",
        ),
    ] {
        let original = query[field].clone();
        query[field] = value;
        fs::write(host.0.join("query.json"), query.to_string()).unwrap();
        refused(cli(&host, "native", &args), error);
        query[field] = original;
    }
    fs::write(host.0.join("query.json"), query.to_string()).unwrap();
    fs::remove_file(host.0.join("memory.jsonl")).unwrap();
    symlink("missing", host.0.join("memory.jsonl")).unwrap();
    refused(cli(&host, "native", &args), "Too many levels");
}

#[test]
fn invalid_cli_inputs_and_essential_overflow_produce_no_partial_brief() {
    let host = support::Host::new();
    for args in [
        vec!["--max-chars", "100"],
        vec!["--paths", "../outside"],
        vec!["--paths", "src//a"],
        vec!["--identity", "../outside"],
        vec!["--query", "missing"],
    ] {
        let output = cli(&host, "task", &args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    refused(cli(&host, "  ", &[]), "task must");
    claims(
        &host,
        (0..15)
            .map(|i| claim("src", &format!("owner-{i}"), 0))
            .collect(),
    );
    refused(
        cli(
            &host,
            "task",
            &["--paths", "src/file", "--max-chars", "1024"],
        ),
        "exceed brief budget",
    );
}

#[test]
fn optional_context_is_omitted_atomically_with_exact_unicode_character_accounting() {
    let host = support::Host::new();
    semantic(&host, "native");
    let payload: Value = serde_json::from_str(&success(cli(
        &host,
        "native",
        &[
            "--query",
            "query.json",
            "--memory-index",
            "memory.jsonl",
            "--cards-index",
            "cards.json",
            "--max-chars",
            "1024",
            "--json",
        ],
    )))
    .unwrap();
    let text = payload["text"].as_str().unwrap();
    assert_eq!(payload["characters"], text.chars().count());
    assert!(text.chars().count() <= 1024);
    assert_eq!(payload["authority"], "non-authoritative-session-brief");
    // Larger Unicode payload forces omission without splitting an item.
    let mut index: Value =
        serde_json::from_slice(&fs::read(host.0.join("cards.json")).unwrap()).unwrap();
    index["cards"][0]["text"] = json!("🦀".repeat(2000));
    fs::write(host.0.join("cards.json"), index.to_string()).unwrap();
    let text = success(cli(
        &host,
        "native",
        &[
            "--query",
            "query.json",
            "--memory-index",
            "memory.jsonl",
            "--cards-index",
            "cards.json",
            "--max-chars",
            "1024",
        ],
    ));
    assert!(text.chars().count() <= 1025);
    assert!(text.contains("OMITTED:") || text.contains(&"🦀".repeat(239)));
}

#[test]
fn default_handoff_context_reuses_native_brief_and_explicit_context_stays_exact() {
    let host = support::Host::new();
    notes(&host);
    success(
        host.command()
            .env("PATH", "")
            .env_remove("CONDUCTOR_NOTES_DB")
            .args([
                "handoff",
                "stage",
                "--identity",
                "agent",
                "--task",
                "compiler",
            ])
            .output()
            .unwrap(),
    );
    let path = host.0.join(".agents/handoff/agent/pending.json");
    let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(value["context"].as_str().unwrap().contains("Native task"));
    assert!(!host.0.join("conductor/active_state.json").exists());
    fs::write(host.0.join("custom.db"), b"corrupt").unwrap();
    success(
        host.command()
            .env("PATH", "")
            .args([
                "handoff",
                "stage",
                "--identity",
                "agent",
                "--task",
                "compiler",
                "--context",
                "Exact explicit context",
            ])
            .output()
            .unwrap(),
    );
    let value: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(value["context"], "Exact explicit context");
}

#[test]
fn invalid_git_metadata_cannot_hide_claims_or_read_unbounded_files() {
    let host = support::Host::new();
    fs::remove_dir_all(host.0.join(".git")).unwrap();
    let marker = host.0.join(".git");
    for bytes in [
        b"not-a-gitdir".to_vec(),
        vec![b'x'; 4097],
        b"gitdir: missing".to_vec(),
    ] {
        fs::write(&marker, bytes).unwrap();
        refused(
            cli(&host, "native", &["--paths", "src"]),
            "claim repository metadata",
        );
    }
    fs::remove_file(&marker).unwrap();
    let target = host.0.join("target");
    fs::create_dir(&target).unwrap();
    symlink(&target, &marker).unwrap();
    refused(cli(&host, "native", &["--paths", "src"]), "Git marker");
    fs::remove_file(&marker).unwrap();
    fs::create_dir_all(target.join("worktree")).unwrap();
    fs::write(&marker, "gitdir: target/worktree").unwrap();
    fs::write(target.join("worktree/commondir"), vec![b'x'; 4097]).unwrap();
    refused(
        cli(&host, "native", &["--paths", "src"]),
        "claim repository metadata",
    );
}
