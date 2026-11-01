//! Native card fixtures; mock HTTP only, never a real model.
#[path = "support/embedding_fixture.rs"]
mod embedding_fixture;
#[allow(dead_code)]
mod support;
use embedding_fixture::{config, inventory, reply, server};
use forge::{
    cards::{self, Settings},
    recall::Query,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};

fn fixture() -> support::Host {
    let host = support::Host::new();
    fs::set_permissions(&host.0, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(host.0.join("pyproject.toml"), "").unwrap();
    fs::create_dir(host.0.join("notes")).unwrap();
    fs::write(host.0.join("notes/KB-A.md"), "# First\n\nalpha full body").unwrap();
    fs::write(host.0.join("notes/kb_z.md"), "# Second\n\nbeta full body").unwrap();
    fs::write(host.0.join("notes/other.md"), "not a card").unwrap();
    host
}
fn mock(
    count: usize,
) -> (
    forge::embedding::Config,
    std::thread::JoinHandle<embedding_fixture::Requests>,
) {
    let (endpoint, worker) = server(vec![
        reply("/api/tags", inventory(7)),
        reply(
            "/api/embed",
            json!({"model":"local-embed:test","embeddings":vec![vec![3.0,4.0];count],"prompt_eval_count":8*count}),
        ),
        reply("/api/tags", inventory(7)),
    ]);
    (config(endpoint), worker)
}
fn settings(host: &support::Host, embedding: forge::embedding::Config) -> Settings {
    Settings {
        host: host.0.clone(),
        notes: Some(host.0.join("notes")),
        output: host.0.join("cards.json"),
        embedding,
        timeout_seconds: 10,
    }
}
#[test]
fn build_and_search_preserve_sorted_full_text_and_exact_native_recipe() {
    let host = fixture();
    let (config, worker) = mock(2);
    let report = cards::build(settings(&host, config.clone())).unwrap();
    assert_eq!(worker.join().unwrap().len(), 3);
    assert_eq!(report.cards, 2);
    assert_eq!(
        report.embedding_fingerprint,
        forge::memory_ingest::fingerprint(&config).unwrap()
    );
    let index = cards::load(&host.0.join("cards.json")).unwrap();
    assert_eq!(index.len(), 2);
    let (search_config, worker) = mock(1);
    let hits = cards::search(&index, &search_config, "test query", 2).unwrap();
    let requests = worker.join().unwrap();
    assert_eq!(
        hits.hits
            .iter()
            .map(|hit| hit.name.as_str())
            .collect::<Vec<_>>(),
        ["KB-A.md", "kb_z.md"]
    );
    assert_eq!(hits.hits[0].text, "# First\n\nalpha full body");
    let body: Value = serde_json::from_slice(&requests[1].1).unwrap();
    assert_eq!(
        body["input"][0],
        format!("{}test query", cards::QUERY_PREFIX)
    );
    let mut mismatch = config;
    mismatch.threads = 2;
    assert!(cards::search(&index, &mismatch, "test query", 1)
        .err()
        .unwrap()
        .to_string()
        .contains("recipe mismatch"));
}
#[test]
fn legacy_indexes_are_offline_only_and_native_loader_rejects_corruption() {
    let host = fixture();
    let path = host.0.join("legacy.json");
    let fp = format!("sha256:{}", "a".repeat(64));
    let legacy = json!({"schema_version":2,"embedding":{
        "fingerprint":fp,"dimension":2,"paid":false,"num_gpu":0,"num_ctx":2048,
        "contract_version":1,"route_id":"local","backend_kind":"ollama",
        "protocol":"ollama-embed","endpoint":"http://127.0.0.1:11434",
        "model":"legacy-model:latest","model_revision":"pinned",
        "normalization":"l2","tokenizer_policy":"model-default",
        "truncation_policy":"reject","max_input_chars":16384,
        "device_policy":"cpu","cost_per_million_tokens":0.0,"max_request_usd":0.0,
        "selection_reason":"quality","quality_receipt_sha256":"a".repeat(64),
        "estimated_cost_usd":0.0,"keep_alive":"0s"},
        "cards":[{"name":"kb_a.md","path":"/note.md","text":"text","vector":[1.0,0.0]}]});
    fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let index = cards::load(&path).unwrap();
    let task = "legacy";
    let query = Query::from_document(
        json!({"schema_version":1,"task_sha256":format!("{:x}",Sha256::digest(task)),
        "embedding_fingerprint":fp,"vector":[1.0,0.0]}),
        task,
    )
    .unwrap();
    assert_eq!(
        cards::score_query(&index, &query, 3).unwrap()[0].name,
        "kb_a.md"
    );
    let (config, worker) = server(vec![]);
    worker.join().unwrap();
    assert!(cards::search(&index, &embedding_fixture::config(config), task, 1).is_err());
    let mut cases = Vec::new();
    let mut wrong = legacy.clone();
    wrong["embedding"]["dimension"] = json!(3);
    cases.push(wrong);
    let mut wrong = legacy.clone();
    wrong["cards"][0]["vector"] = json!([1.0]);
    cases.push(wrong);
    let mut wrong = legacy.clone();
    wrong["cards"][0]["vector"] = json!([0.0, 0.0]);
    cases.push(wrong);
    let mut wrong = legacy.clone();
    wrong["cards"][0]["vector"] = json!(["x", 1.0]);
    cases.push(wrong);
    let mut wrong = legacy.clone();
    wrong["embedding"]["fingerprint"] = json!("sha256:not-a-hash");
    cases.push(wrong);
    for (field, value) in [
        ("artifact_sha256", json!("b".repeat(64))),
        ("runner", json!("cpu")),
        ("threads", json!(1)),
        ("query_prefix_sha256", json!("c".repeat(64))),
    ] {
        let mut partial = legacy.clone();
        partial["embedding"][field] = value;
        cases.push(partial);
    }
    let mut partial = legacy.clone();
    partial["provenance"] = json!({"notes_root":"/notes", "source_set_sha256":"a".repeat(64)});
    cases.push(partial);
    let mut partial = legacy.clone();
    partial["cards"][0]["source_sha256"] = json!("b".repeat(64));
    cases.push(partial);
    for case in cases {
        fs::write(&path, serde_json::to_vec(&case).unwrap()).unwrap();
        assert!(cards::load(&path).is_err());
    }
}

#[test]
fn native_metadata_and_source_content_are_hash_bound() {
    let host = fixture();
    let (config, worker) = mock(2);
    cards::build(settings(&host, config)).unwrap();
    worker.join().unwrap();
    let path = host.0.join("cards.json");
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut partial = original.clone();
    partial["embedding"]
        .as_object_mut()
        .unwrap()
        .remove("native_recipe");
    partial["cards"][0]["text"] = json!("unbound changed text");
    fs::write(&path, serde_json::to_vec(&partial).unwrap()).unwrap();
    assert!(cards::load(&path)
        .err()
        .unwrap()
        .to_string()
        .contains("requires native_recipe"));
    for (field, value) in [
        ("model", json!("other:test")),
        ("threads", json!(2)),
        ("artifact_sha256", json!("b".repeat(64))),
        ("query_prefix_sha256", json!("0".repeat(64))),
    ] {
        let mut changed = original.clone();
        changed["embedding"][field] = value;
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(cards::load(&path).is_err(), "{field}");
    }
    let mut changed = original.clone();
    changed["cards"][0]["vector"] = json!([1000.0, 1.0]);
    fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(cards::load(&path).is_err());
    let mut changed = original;
    changed["cards"][0]["text"] = json!("unbound changed text");
    fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(cards::load(&path).is_err());
}
#[test]
fn source_change_and_symlink_escape_preserve_existing_output() {
    let host = fixture();
    let (config, worker) = mock(2);
    cards::build(settings(&host, config.clone())).unwrap();
    worker.join().unwrap();
    let index = host.0.join("cards.json");
    let before = fs::read(&index).unwrap();
    let changed = host.0.join("notes/KB-A.md");
    let mut embed = reply(
        "/api/embed",
        json!({"model":"local-embed:test","embeddings":[[3.0,4.0],[3.0,4.0]],"prompt_eval_count":16}),
    );
    embed.before_reply = Some(Box::new(move || {
        fs::write(changed, "changed during build").unwrap()
    }));
    let (endpoint, worker) = server(vec![
        reply("/api/tags", inventory(7)),
        embed,
        reply("/api/tags", inventory(7)),
    ]);
    assert!(cards::build(settings(&host, embedding_fixture::config(endpoint))).is_err());
    worker.join().unwrap();
    assert_eq!(fs::read(&index).unwrap(), before);
    fs::remove_file(host.0.join("notes/KB-A.md")).unwrap();
    symlink(host.0.join("notes/other.md"), host.0.join("notes/KB-A.md")).unwrap();
    assert!(cards::build(settings(&host, config)).is_err());
    assert_eq!(fs::read(&index).unwrap(), before);
}

#[test]
fn failed_embedding_and_unsafe_targets_preserve_existing_files() {
    let host = fixture();
    let (config, worker) = mock(2);
    cards::build(settings(&host, config)).unwrap();
    worker.join().unwrap();
    let index = host.0.join("cards.json");
    let before = fs::read(&index).unwrap();
    let mut failure = reply("/api/embed", json!({"error":"refused"}));
    failure.status = 503;
    let (endpoint, worker) = server(vec![reply("/api/tags", inventory(7)), failure]);
    assert!(cards::build(settings(&host, embedding_fixture::config(endpoint))).is_err());
    assert_eq!(worker.join().unwrap().len(), 2);
    assert_eq!(fs::read(&index).unwrap(), before);
    let sentinel = host.0.join("sentinel");
    fs::write(&sentinel, b"untouched").unwrap();
    let alias = host.0.join("alias.json");
    symlink(&sentinel, &alias).unwrap();
    let mut request = settings(
        &host,
        embedding_fixture::config("http://127.0.0.1:1".into()),
    );
    request.output = alias;
    assert!(cards::build(request).is_err());
    assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
    let link = host.0.join("notes-alias");
    symlink(host.0.join("notes"), &link).unwrap();
    let mut request = settings(
        &host,
        embedding_fixture::config("http://127.0.0.1:1".into()),
    );
    request.notes = Some(link);
    assert!(cards::build(request).is_err());
    assert_eq!(fs::read(index).unwrap(), before);
}
#[test]
fn cli_build_and_search_work_without_git_or_python_on_path() {
    let host = fixture();
    let (config, worker) = mock(2);
    fs::write(
        host.0.join("config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let output = host
        .command()
        .env("PATH", "")
        .args([
            "cards",
            "build",
            "--host",
            host.0.to_str().unwrap(),
            "--notes",
            "notes",
            "--output",
            "cards.json",
            "--embedding",
            host.0.join("config.json").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    worker.join().unwrap();
    let (config, worker) = mock(1);
    fs::write(
        host.0.join("config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let output = host
        .command()
        .env("PATH", "")
        .args([
            "cards",
            "search",
            "--index",
            host.0.join("cards.json").to_str().unwrap(),
            "--embedding",
            host.0.join("config.json").to_str().unwrap(),
            "--task",
            "native search",
            "--top-k",
            "1",
        ])
        .output()
        .unwrap();
    worker.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["hits"].as_array().unwrap().len(), 1);
}

#[test]
fn one_bound_query_scores_native_memory_and_whole_cards_together() {
    let host = fixture();
    let (card_config, card_worker) = mock(2);
    let report = cards::build(settings(&host, card_config)).unwrap();
    card_worker.join().unwrap();
    fs::create_dir(host.0.join("mem")).unwrap();
    fs::write(host.0.join("mem/one.md"), "native memory fact").unwrap();
    fs::write(
        host.0.join("catalog.toml"),
        "schema_version=1\n[[source]]\nid='notes'\nkind='index'\nroot='mem'\nchunk='whole'\n",
    )
    .unwrap();
    let (memory_config, memory_worker) = mock(1);
    forge::memory_ingest::ingest(forge::memory_ingest::Settings {
        host: host.0.clone(),
        store: host.0.join("memory-store"),
        catalog: Some(host.0.join("catalog.toml")),
        sources: vec![],
        full: false,
        allow_missing_roots: false,
        timeout_seconds: 10,
        embedding: memory_config,
    })
    .unwrap();
    memory_worker.join().unwrap();
    let pin = forge::memory_store::Store::open(&host.0.join("memory-store"))
        .unwrap()
        .pin()
        .unwrap();
    assert_eq!(pin.manifest.fingerprint, report.embedding_fingerprint);
    let task = "shared task";
    let query = Query::from_document(
        json!({"schema_version":1,"task_sha256":format!("{:x}",Sha256::digest(task)),
        "embedding_fingerprint":report.embedding_fingerprint,"vector":[3.0,4.0]}),
        task,
    )
    .unwrap();
    let index = cards::load(&host.0.join("cards.json")).unwrap();
    assert!(!cards::score_query(&index, &query, 3).unwrap().is_empty());
    assert!(!forge::recall::memory_hits(
        &pin.source,
        Some(&pin.source.with_file_name("index.sidecar")),
        &query,
        3
    )
    .unwrap()
    .is_empty());
}
#[test]
fn card_storage_accepts_valid_indexes_larger_than_tool_argument_limit() {
    let host = fixture();
    let text = "x".repeat(600_000);
    let value = json!({"schema_version":2,"embedding":{"fingerprint":format!("sha256:{}","a".repeat(64)),
        "dimension":2,"paid":false,"num_gpu":0,"num_ctx":2048},"cards":[
        {"name":"one","path":"/one","text":text,"vector":[1.0,0.0]},
        {"name":"two","path":"/two","text":text,"vector":[0.0,1.0]}]});
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(bytes.len() > 1024 * 1024);
    let path = host.0.join("large.json");
    fs::write(&path, bytes).unwrap();
    assert_eq!(cards::load(&path).unwrap().len(), 2);
}
