#[path = "support/embedding_fixture.rs"]
mod fixture;
#[allow(dead_code)]
mod support;
use conductor_native::vector_index::Config;
use fixture::*;
use forge::code_index::{
    extract::{self, Options, Refresh},
    QueryRequest, Reader, SearchMode, Store,
};
use serde_json::{json, Value};
use std::fs;

fn options(paths: &[&str], chunk_bytes: usize) -> Options {
    Options {
        paths: paths.iter().map(|p| (*p).to_owned()).collect(),
        chunk_bytes,
        max_source_bytes: 1024 * 1024,
        allow_syntax_errors: false,
    }
}
fn query(text: &str) -> QueryRequest {
    QueryRequest {
        text: text.into(),
        vector: None,
        space: None,
        mode: SearchMode::Exact,
        limit: 10,
        candidates: 100,
        ef: 160,
    }
}
fn sequence(vectors: Vec<Vec<f32>>) -> Vec<Reply> {
    vec![
        reply("/api/tags", inventory(7)),
        reply(
            "/api/embed",
            json!({"model":"local-embed:test","embeddings":vectors,"prompt_eval_count":10}),
        ),
        reply("/api/tags", inventory(7)),
    ]
}
fn settings(endpoint: String, paths: &[&str]) -> Refresh {
    Refresh {
        sources: options(paths, 512),
        embedding: config(endpoint),
        graph: Config::default(),
        deadline_seconds: 10,
    }
}

#[test]
fn plans_cover_exact_utf8_bytes_and_prefer_native_symbol_boundaries() {
    let host = support::Host::new();
    let first = format!("fn first() {{ let value = \"{}\"; }}\n", "π🦀".repeat(20));
    let second = format!("fn second() {{ let value = \"{}\"; }}\n", "文".repeat(80));
    let text = format!("{first}{second}");
    fs::write(host.0.join("main.rs"), &text).unwrap();
    let maximum = first.len() + 12;
    let plan = extract::plan(&host.0, &options(&["main.rs"], maximum)).unwrap();
    let file = &plan.files[0];
    assert!(file.syntax_error.is_none());
    assert_eq!(file.spans[0][1], first.len());
    let mut restored = String::new();
    let mut previous = 0;
    for &[start, end] in &file.spans {
        assert_eq!(start, previous);
        assert!(end - start <= maximum);
        restored.push_str(&text[start..end]);
        previous = end;
    }
    assert_eq!(restored, text);
    assert_eq!(plan.source_bytes, text.len());
    fs::write(host.0.join("broken.rs"), "fn broken( {").unwrap();
    let mut opts = options(&["broken.rs"], 128);
    assert!(extract::plan(&host.0, &opts).is_err());
    opts.allow_syntax_errors = true;
    assert!(extract::plan(&host.0, &opts).unwrap().files[0]
        .syntax_error
        .is_some());
    for paths in [
        vec!["../outside"],
        vec!["main.rs", "main.rs"],
        vec!["./main.rs"],
    ] {
        assert!(extract::plan(&host.0, &options(&paths, 128)).is_err());
    }
    let mut small = options(&["main.rs"], 128);
    small.max_source_bytes = 1;
    assert!(extract::plan(&host.0, &small).is_err());
}

#[test]
fn source_refresh_batches_in_order_and_matches_embedding_recipe() {
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.rs"), "fn alpha() {}\n").unwrap();
    fs::write(source.join("b.rs"), "fn beta() {}\n").unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0], vec![0.0, 1.0]]));
    let cfg = settings(endpoint, &["b.rs", "a.rs"]);
    let expected = cfg.embedding.space().unwrap();
    let report = extract::refresh(&store, &source, cfg).unwrap();
    assert_eq!(report.manifest.rows, 2);
    assert_eq!(report.manifest.space, expected);
    assert_eq!(report.batches, 1);
    assert_eq!(report.input_tokens_total, 10);
    assert!(report.diagnostics.is_empty());
    let calls = worker.join().unwrap();
    let body: Value = serde_json::from_slice(&calls[1].1).unwrap();
    assert_eq!(body["input"], json!(["fn alpha() {}\n", "fn beta() {}\n"]));
    let mut reader = Reader::open(&store).unwrap();
    assert_eq!(
        reader.search(&source, query("alpha")).unwrap().hits[0].path,
        "a.rs"
    );
    let empty =
        extract::refresh(&store, &source, settings("http://127.0.0.1:1".into(), &[])).unwrap();
    assert_eq!(empty.manifest.rows, 0);
    assert_eq!(empty.batches, 0);
}

#[test]
fn concurrent_source_change_and_provider_failure_cannot_publish_partial_refresh() {
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.rs"), "fn old() {}\n").unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    let original = extract::refresh(&store, &source, settings(endpoint, &["a.rs"]))
        .unwrap()
        .manifest
        .generation;
    worker.join().unwrap();
    let mut responses = sequence(vec![vec![1.0, 0.0]]);
    let changed = source.join("a.rs");
    responses[1].before_reply = Some(Box::new(move || {
        fs::write(changed, "fn changed() {}\n").unwrap()
    }));
    let (endpoint, worker) = server(responses);
    assert!(extract::refresh(&store, &source, settings(endpoint, &["a.rs"])).is_err());
    worker.join().unwrap();
    assert_eq!(store.current().unwrap(), original);
    let (endpoint, worker) = server(vec![reply("/api/tags", inventory(8))]);
    assert!(extract::refresh(&store, &source, settings(endpoint, &["a.rs"])).is_err());
    worker.join().unwrap();
    assert_eq!(store.current().unwrap(), original);
}

#[test]
fn construction_checkpoints_abort_without_publishing_vectors() {
    use conductor_native::vector_index::{Builder, Space};
    let mut checkpoints = 0;
    let result = Builder::new_checked(
        Space {
            dimension: 2,
            model_sha256: [1; 32],
        },
        vec![1.0, 0.0, 0.0, 1.0],
        Config::default(),
        || {
            checkpoints += 1;
            anyhow::ensure!(checkpoints < 3, "expired construction budget");
            Ok(())
        },
    );
    assert!(result.is_err());
    assert_eq!(checkpoints, 3);
}

#[test]
fn refresh_respects_both_batch_count_and_input_byte_limits_without_losing_spans() {
    for (chunk_bytes, repeats) in [(512, 4000), (16384, 22000)] {
        let host = support::Host::new();
        let source = host.0.join("source");
        fs::create_dir(&source).unwrap();
        let text = "alpha ".repeat(repeats);
        fs::write(source.join("code.txt"), &text).unwrap();
        let opts = options(&["code.txt"], chunk_bytes);
        let plan = extract::plan(&source, &opts).unwrap();
        let batch_rows = 32.min(65536 / chunk_bytes);
        let mut responses = Vec::new();
        for start in (0..plan.rows).step_by(batch_rows) {
            responses.extend(sequence(vec![
                vec![1.0, 0.0];
                batch_rows.min(plan.rows - start)
            ]));
        }
        let (endpoint, worker) = server(responses);
        let mut cfg = settings(endpoint, &["code.txt"]);
        cfg.sources = opts;
        let report =
            extract::refresh(&Store::create(&host.0.join("index")).unwrap(), &source, cfg).unwrap();
        assert_eq!(report.manifest.rows, plan.rows);
        assert_eq!(report.batches as usize, plan.rows.div_ceil(batch_rows));
        let mut recovered = String::new();
        for (request, body) in worker.join().unwrap() {
            if !request.starts_with("POST") {
                continue;
            }
            let data: Value = serde_json::from_slice(&body).unwrap();
            let inputs = data["input"].as_array().unwrap();
            assert!(inputs.len() <= 32);
            let bytes: usize = inputs.iter().map(|s| s.as_str().unwrap().len()).sum();
            assert!(bytes <= 65536);
            for input in inputs {
                recovered.push_str(input.as_str().unwrap());
            }
        }
        assert_eq!(recovered, text);
    }
}

#[test]
fn cli_planning_is_read_only_and_refresh_uses_only_the_configured_native_client() {
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.rs"), "fn alpha() {}\n").unwrap();
    let index = host.0.join("index");
    let options_path = host.0.join("options.json");
    fs::write(
        &options_path,
        serde_json::to_vec(&options(&["a.rs"], 512)).unwrap(),
    )
    .unwrap();
    let mut command = std::process::Command::new(support::forge_binary::path());
    command
        .env_clear()
        .env("PATH", "")
        .args(["code-index", "plan", "--source-root"])
        .arg(&source)
        .arg("--options")
        .arg(options_path);
    let result =
        forge::process_probe::run(&mut command, std::time::Duration::from_secs(5), 65536).unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let data: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(data["rows"], 1);
    assert_eq!(data["model_requests"], 0);
    assert!(!index.exists());
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    let config_path = host.0.join("refresh.json");
    fs::write(
        &config_path,
        serde_json::to_vec(&settings(endpoint, &["a.rs"])).unwrap(),
    )
    .unwrap();
    let mut command = std::process::Command::new(support::forge_binary::path());
    command
        .env_clear()
        .env("PATH", "")
        .args(["code-index", "refresh", "--source-root"])
        .arg(&source)
        .arg("--index")
        .arg(&index)
        .arg("--config")
        .arg(config_path);
    let result =
        forge::process_probe::run(&mut command, std::time::Duration::from_secs(5), 65536).unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    worker.join().unwrap();
    let data: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(data["manifest"]["rows"], 1);
    assert_eq!(data["batches"], 1);
}
