#[path = "support/embedding_fixture.rs"]
mod fixture;
#[path = "../benches/real_code_index/embedding.rs"]
mod query_embedding;
#[allow(dead_code)]
mod support;
use conductor_native::vector_index::Config;
use fixture::*;
use forge::code_index::{
    extract::{self, BatchPolicy, Options, Refresh},
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
        force_reembed: false,
        batch: BatchPolicy::default(),
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
    assert_eq!(report.manifest.version, 2);
    assert!(report.manifest.refresh_origin.is_some());
    assert_eq!(report.manifest.space, expected);
    assert_eq!(report.batches, 1);
    assert_eq!(report.batch, BatchPolicy::default());
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
    let mut forced = settings(endpoint, &["a.rs"]);
    forced.force_reembed = true;
    assert!(extract::refresh(&store, &source, forced).is_err());
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
    for (chunk_bytes, repeats, max_inputs, max_bytes) in [
        (512, 4000, 32, 65536),
        (16384, 22000, 32, 65536),
        (512, 500, 3, 65536),
        (512, 500, 32, 1024),
        (512, 500, 1, 512),
    ] {
        let host = support::Host::new();
        let source = host.0.join("source");
        fs::create_dir(&source).unwrap();
        let text: String = (0..repeats).map(|i| format!("{i:05} α ")).collect();
        fs::write(source.join("code.txt"), &text).unwrap();
        let opts = options(&["code.txt"], chunk_bytes);
        let plan = extract::plan(&source, &opts).unwrap();
        let mut sizes = Vec::new();
        let (mut rows, mut bytes) = (0, 0);
        for &[start, end] in &plan.files[0].spans {
            if rows == max_inputs || bytes + end - start > max_bytes {
                sizes.push(rows);
                (rows, bytes) = (0, 0);
            }
            rows += 1;
            bytes += end - start;
        }
        if rows > 0 {
            sizes.push(rows);
        }
        let responses = sizes
            .iter()
            .flat_map(|&n| sequence(vec![vec![1.0, 0.0]; n]))
            .collect();
        let (endpoint, worker) = server(responses);
        let mut cfg = settings(endpoint, &["code.txt"]);
        cfg.sources = opts;
        cfg.batch = BatchPolicy {
            max_inputs,
            max_bytes,
        };
        let report =
            extract::refresh(&Store::create(&host.0.join("index")).unwrap(), &source, cfg).unwrap();
        assert_eq!(
            report.batch,
            BatchPolicy {
                max_inputs,
                max_bytes
            }
        );
        assert_eq!(report.manifest.rows, plan.rows);
        assert_eq!(report.batches as usize, sizes.len());
        let mut recovered = String::new();
        for (request, body) in worker.join().unwrap() {
            if !request.starts_with("POST") {
                continue;
            }
            let data: Value = serde_json::from_slice(&body).unwrap();
            let inputs = data["input"].as_array().unwrap();
            assert!(!inputs.is_empty() && inputs.len() <= max_inputs);
            let bytes: usize = inputs.iter().map(|s| s.as_str().unwrap().len()).sum();
            assert!(bytes <= max_bytes);
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
    let mut cfg = settings(endpoint, &["a.rs"]);
    cfg.batch = BatchPolicy {
        max_inputs: 1,
        max_bytes: 512,
    };
    fs::write(&config_path, serde_json::to_vec(&cfg).unwrap()).unwrap();
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
    assert_eq!(data["batch"], json!({"max_inputs": 1, "max_bytes": 512}));
}

#[test]
fn invalid_batch_policy_is_rejected_before_any_http_or_index_publication() {
    use std::net::TcpListener;
    let host = support::Host::new();
    fs::write(host.0.join("code.txt"), "hello source").unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    for (max_inputs, max_bytes) in [(0, 65536), (33, 65536), (1, 0), (1, 511), (1, 65537)] {
        let mut cfg = settings(endpoint.clone(), &["code.txt"]);
        cfg.batch = BatchPolicy {
            max_inputs,
            max_bytes,
        };
        let error = extract::refresh(&store, &host.0, cfg).unwrap_err();
        assert!(
            error.to_string().contains("invalid embedding batch limits"),
            "{error:#}"
        );
        assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
        assert!(store.current().is_err());
    }
}

#[test]
fn refresh_json_preserves_legacy_defaults_and_admits_explicit_smaller_limits() {
    let mut value = serde_json::to_value(settings("http://127.0.0.1:1".into(), &[])).unwrap();
    value.as_object_mut().unwrap().remove("batch");
    let legacy: Refresh = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(legacy.batch, BatchPolicy::default());
    value["batch"] = json!({"max_inputs": 2, "max_bytes": 1024});
    let smaller: Refresh = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        smaller.batch,
        BatchPolicy {
            max_inputs: 2,
            max_bytes: 1024
        }
    );
    smaller.batch.validate(smaller.sources.chunk_bytes).unwrap();
    value["batch"] = json!({"max_inputs": 2});
    let partial: Refresh = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(partial.batch.max_bytes, 65536);
    value["batch"]["unknown_limit"] = json!(3);
    assert!(serde_json::from_value::<Refresh>(value).is_err());
}

#[test]
fn failed_later_batch_preserves_completed_progress_without_publishing_partial_index() {
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.txt"), "first").unwrap();
    fs::write(source.join("b.txt"), "second").unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let mut replies = sequence(vec![vec![1.0, 0.0]]);
    replies.push(reply("/api/tags", inventory(7)));
    let mut failed = reply("/api/embed", json!({"error":"later batch failed"}));
    failed.status = 500;
    replies.push(failed);
    let (endpoint, worker) = server(replies);
    let mut cfg = settings(endpoint, &["b.txt", "a.txt"]);
    cfg.batch.max_inputs = 1;
    let mut observations = Vec::new();
    let error = extract::refresh_with_progress(&store, &source, cfg, |progress| {
        observations.push(progress);
        Ok(())
    })
    .unwrap_err();
    assert!(format!("{error:#}").contains("500"), "{error:#}");
    let calls = worker.join().unwrap();
    let first: Value = serde_json::from_slice(&calls[1].1).unwrap();
    assert_eq!(first["input"], json!(["first"]));
    assert_eq!(observations.len(), 2);
    assert_eq!(
        (
            observations[0].batches,
            observations[0].rows,
            observations[0].input_tokens_total
        ),
        (0, 0, 0)
    );
    assert_eq!(
        (
            observations[1].batches,
            observations[1].rows,
            observations[1].input_tokens_total
        ),
        (1, 1, 10)
    );
    assert_eq!(observations[1].batch.max_inputs, 1);
    assert!(store.current().is_err());
}

#[test]
fn observer_failure_refuses_publication_after_completed_preparation() {
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.txt"), "first").unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    let mut last_rows = 0;
    let result = extract::refresh_with_progress(
        &store,
        &source,
        settings(endpoint, &["a.txt"]),
        |progress| {
            last_rows = progress.rows;
            anyhow::ensure!(progress.batches == 0, "progress journal failed");
            Ok(())
        },
    );
    let error = result.unwrap_err();
    assert!(
        format!("{error:#}").contains("progress journal failed"),
        "{error:#}"
    );
    assert_eq!(last_rows, 1);
    worker.join().unwrap();
    assert!(store.current().is_err());
}

#[test]
fn query_preparation_preserves_all_24_vectors_order_and_sums_batch_tokens() {
    use std::time::{Duration, Instant};
    let texts: Vec<_> = (0..24)
        .map(|i| format!("{i:02}:{}", "q".repeat(198)))
        .collect();
    let inputs: Vec<_> = texts.iter().map(String::as_str).collect();
    for (policy, batch_rows) in [
        (
            BatchPolicy {
                max_inputs: 3,
                max_bytes: 65536,
            },
            3,
        ),
        (
            BatchPolicy {
                max_inputs: 32,
                max_bytes: 512,
            },
            2,
        ),
    ] {
        let mut replies = Vec::new();
        for start in (0..24).step_by(batch_rows) {
            replies.extend(sequence(
                (start..start + batch_rows)
                    .map(|i| vec![1.0, (i + 1) as f32])
                    .collect(),
            ));
        }
        let (endpoint, worker) = server(replies);
        let client = forge::embedding::Client::new(config(endpoint)).unwrap();
        let mut progress = Vec::new();
        let (batch, batches) = query_embedding::queries(
            &client,
            &inputs,
            policy,
            Instant::now() + Duration::from_secs(10),
            |p| {
                progress.push(p);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(batch.vectors.len(), 24);
        assert_eq!(batches, (24 / batch_rows) as u64);
        assert_eq!(batch.input_tokens_total, batches * 10);
        assert_eq!(progress.len(), batches as usize + 1);
        for (i, p) in progress.iter().enumerate() {
            assert_eq!(
                (p.rows, p.batches, p.input_tokens_total),
                ((i * batch_rows) as u64, i as u64, (i * 10) as u64)
            );
            assert_eq!(p.batch, policy);
        }
        for (i, vector) in batch.vectors.iter().enumerate() {
            assert!((vector[1] / vector[0] - (i + 1) as f32).abs() < 0.0001);
        }
        let mut recovered = Vec::new();
        for (request, body) in worker.join().unwrap() {
            if !request.starts_with("POST") {
                continue;
            }
            let body: Value = serde_json::from_slice(&body).unwrap();
            let sent = body["input"].as_array().unwrap();
            assert!(sent.len() <= policy.max_inputs);
            assert!(
                sent.iter()
                    .map(|s| s.as_str().unwrap().len())
                    .sum::<usize>()
                    <= policy.max_bytes
            );
            recovered.extend(sent.iter().map(|s| s.as_str().unwrap().to_owned()));
        }
        assert_eq!(recovered, texts);
    }
}

#[test]
fn oversized_query_is_rejected_before_any_http() {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let client =
        forge::embedding::Client::new(config(format!("http://{}", listener.local_addr().unwrap())))
            .unwrap();
    let large = "x".repeat(513);
    let result = query_embedding::queries(
        &client,
        &["valid", &large],
        BatchPolicy {
            max_inputs: 2,
            max_bytes: 512,
        },
        std::time::Instant::now(),
        |_| Ok(()),
    );
    assert!(result.is_err());
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
}

fn excessive_later_token_count() -> Vec<Reply> {
    let mut responses = sequence(vec![vec![1.0, 0.0]]);
    responses.push(reply("/api/tags", inventory(7)));
    responses.push(reply(
        "/api/embed",
        json!({"model":"local-embed:test",
        "embeddings":[[1.0,0.0]], "prompt_eval_count":u64::MAX}),
    ));
    responses
}

#[test]
fn later_overflow_sized_server_count_refuses_refresh_and_preserves_previous_index() {
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.txt"), "first").unwrap();
    fs::write(source.join("b.txt"), "second").unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    let original = extract::refresh(&store, &source, settings(endpoint, &["a.txt"]))
        .unwrap()
        .manifest
        .generation;
    worker.join().unwrap();
    let (endpoint, worker) = server(excessive_later_token_count());
    let mut cfg = settings(endpoint, &["a.txt", "b.txt"]);
    cfg.batch.max_inputs = 1;
    // Exercise two real provider batches even though a prior row is reusable.
    cfg.force_reembed = true;
    let mut seen = Vec::new();
    let result = extract::refresh_with_progress(&store, &source, cfg, |p| {
        seen.push(p);
        Ok(())
    });
    assert!(format!("{:#}", result.unwrap_err()).contains("embedding token usage"));
    worker.join().unwrap();
    assert_eq!(store.current().unwrap(), original);
    assert_eq!(seen.len(), 2);
    assert_eq!((seen[1].reused_rows, seen[1].new_rows), (0, 1));
    assert_eq!(
        (seen[1].rows, seen[1].batches, seen[1].input_tokens_total),
        (1, 1, 10)
    );
}

#[test]
fn later_overflow_sized_query_count_refuses_result_and_retains_completed_progress() {
    let (endpoint, worker) = server(excessive_later_token_count());
    let client = forge::embedding::Client::new(config(endpoint)).unwrap();
    let mut seen = Vec::new();
    let result = query_embedding::queries(
        &client,
        &["first", "second"],
        BatchPolicy {
            max_inputs: 1,
            max_bytes: 512,
        },
        std::time::Instant::now() + std::time::Duration::from_secs(10),
        |p| {
            seen.push(p);
            Ok(())
        },
    );
    assert!(format!("{:#}", result.unwrap_err()).contains("embedding token usage"));
    worker.join().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        (seen[0].rows, seen[0].batches, seen[0].input_tokens_total),
        (0, 0, 0)
    );
    assert_eq!(
        (seen[1].rows, seen[1].batches, seen[1].input_tokens_total),
        (1, 1, 10)
    );
    assert_eq!(
        seen[1].batch,
        BatchPolicy {
            max_inputs: 1,
            max_bytes: 512
        }
    );
}

#[test]
fn query_observer_error_is_explicit_and_stops_before_the_next_request() {
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    let client = forge::embedding::Client::new(config(endpoint)).unwrap();
    let mut seen = Vec::new();
    let result = query_embedding::queries(
        &client,
        &["first", "second"],
        BatchPolicy {
            max_inputs: 1,
            max_bytes: 512,
        },
        std::time::Instant::now() + std::time::Duration::from_secs(10),
        |p| {
            seen.push(p);
            anyhow::ensure!(p.batches == 0, "query preparation journal failed");
            Ok(())
        },
    );
    assert!(format!("{:#}", result.unwrap_err()).contains("query preparation journal failed"));
    assert_eq!(worker.join().unwrap().len(), 3);
    assert_eq!(seen.len(), 2);
    assert_eq!(
        (seen[1].rows, seen[1].batches, seen[1].input_tokens_total),
        (1, 1, 10)
    );
}

#[test]
fn unchanged_refresh_reuses_exact_vectors_without_network_or_token_fabrication() {
    use conductor_native::vector_index::Snapshot;
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.rs"), "fn alpha() {}\n").unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![3.0, 4.0]]));
    let first = extract::refresh(&store, &source, settings(endpoint, &["a.rs"])).unwrap();
    worker.join().unwrap();
    let old = Snapshot::open(
        &store
            .root()
            .join(&first.manifest.generation)
            .join("vectors.bin"),
    )
    .unwrap();
    let before: Vec<_> = old.row(0).unwrap().iter().map(|v| v.to_bits()).collect();
    drop(old);
    let sentinel = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    sentinel.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", sentinel.local_addr().unwrap());
    let next = extract::refresh(&store, &source, settings(endpoint, &["a.rs"])).unwrap();
    assert_eq!(
        sentinel.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(
        (
            next.reused_rows,
            next.new_rows,
            next.batches,
            next.input_tokens_total
        ),
        (1, 0, 0, 0)
    );
    let snapshot = Snapshot::open(
        &store
            .root()
            .join(&next.manifest.generation)
            .join("vectors.bin"),
    )
    .unwrap();
    assert_eq!(
        snapshot
            .row(0)
            .unwrap()
            .iter()
            .map(|v| v.to_bits())
            .collect::<Vec<_>>(),
        before
    );
}
#[test]
fn changed_deleted_and_new_files_refresh_only_missing_rows() {
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    for (path, data) in [
        ("a.rs", "fn a() {}\n"),
        ("b.rs", "fn b() {}\n"),
        ("c.rs", "fn c() {}\n"),
    ] {
        fs::write(source.join(path), data).unwrap();
    }
    let store = Store::create(&host.0.join("index")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]; 3]));
    extract::refresh(
        &store,
        &source,
        settings(endpoint, &["a.rs", "b.rs", "c.rs"]),
    )
    .unwrap();
    worker.join().unwrap();
    fs::write(source.join("b.rs"), "fn changed() {}\n").unwrap();
    fs::remove_file(source.join("c.rs")).unwrap();
    fs::write(source.join("d.rs"), "fn added() {}\n").unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![0.0, 1.0]; 2]));
    let report = extract::refresh(
        &store,
        &source,
        settings(endpoint, &["a.rs", "b.rs", "d.rs"]),
    )
    .unwrap();
    let calls = worker.join().unwrap();
    let body: Value = serde_json::from_slice(&calls[1].1).unwrap();
    assert_eq!(
        body["input"],
        json!(["fn changed() {}\n", "fn added() {}\n"])
    );
    assert_eq!(
        (report.reused_rows, report.new_rows, report.manifest.rows),
        (1, 2, 3)
    );
    assert_eq!(report.input_tokens_total, 10);
    let mut reader = Reader::open(&store).unwrap();
    assert!(reader.search(&source, query("c")).unwrap().hits.is_empty());
}
#[test]
fn changed_space_and_force_reembed_do_not_reuse() {
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.rs"), "fn a() {}\n").unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    extract::refresh(&store, &source, settings(endpoint, &["a.rs"])).unwrap();
    worker.join().unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![0.0, 1.0]]));
    let mut changed = settings(endpoint, &["a.rs"]);
    changed.embedding.threads = 2;
    let report = extract::refresh(&store, &source, changed).unwrap();
    worker.join().unwrap();
    assert_eq!((report.reused_rows, report.new_rows), (0, 1));
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    let mut forced = settings(endpoint, &["a.rs"]);
    forced.embedding.threads = 2;
    forced.force_reembed = true;
    let report = extract::refresh(&store, &source, forced).unwrap();
    worker.join().unwrap();
    assert_eq!(
        (report.reused_rows, report.new_rows, report.batches),
        (0, 1, 1)
    );
}
#[test]
fn corrupted_prior_generation_fails_closed_without_reembedding() {
    use std::os::unix::fs::PermissionsExt;
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.rs"), "fn a() {}\n").unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    let prior = extract::refresh(&store, &source, settings(endpoint, &["a.rs"]))
        .unwrap()
        .manifest
        .generation;
    worker.join().unwrap();
    let vector = store.root().join(&prior).join("vectors.bin");
    fs::set_permissions(&vector, fs::Permissions::from_mode(0o600)).unwrap();
    let mut bytes = fs::read(&vector).unwrap();
    bytes[0] ^= 1;
    fs::write(&vector, bytes).unwrap();
    fs::set_permissions(&vector, fs::Permissions::from_mode(0o400)).unwrap();
    assert!(extract::refresh(
        &store,
        &source,
        settings("http://127.0.0.1:1".into(), &["a.rs"])
    )
    .is_err());
    assert_eq!(store.current().unwrap(), prior);
}
#[test]
fn partial_reuse_then_provider_failure_keeps_prior_generation() {
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.rs"), "fn a() {}\n").unwrap();
    fs::write(source.join("b.rs"), "fn b() {}\n").unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]; 2]));
    let prior = extract::refresh(&store, &source, settings(endpoint, &["a.rs", "b.rs"]))
        .unwrap()
        .manifest
        .generation;
    worker.join().unwrap();
    fs::write(source.join("b.rs"), "fn changed() {}\n").unwrap();
    let (endpoint, worker) = server(vec![reply("/api/tags", inventory(8))]);
    assert!(extract::refresh(&store, &source, settings(endpoint, &["a.rs", "b.rs"])).is_err());
    worker.join().unwrap();
    assert_eq!(store.current().unwrap(), prior);
}
#[test]
fn imported_generation_without_native_refresh_origin_is_reembedded() {
    use forge::code_index::{build, BuildHeader, EmbeddingRow};
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    let text = "fn a() {}\n";
    fs::write(source.join("a.rs"), text).unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let model = config("http://127.0.0.1:1".into());
    let plan = extract::plan(&source, &options(&["a.rs"], 512)).unwrap();
    let prior = build(
        &store,
        &source,
        BuildHeader {
            version: 1,
            rows: 1,
            space: model.space().unwrap(),
            config: Config::default(),
        },
        [Ok(EmbeddingRow {
            path: "a.rs".into(),
            source_sha256: plan.files[0].source_sha256.clone(),
            start_byte: 0,
            end_byte: text.len(),
            vector: vec![1.0, 0.0],
        })],
    )
    .unwrap();
    assert_eq!(prior.version, 1);
    assert!(prior.refresh_origin.is_none());
    let (endpoint, worker) = server(sequence(vec![vec![0.0, 1.0]]));
    let report = extract::refresh(&store, &source, settings(endpoint, &["a.rs"])).unwrap();
    worker.join().unwrap();
    assert_eq!((report.reused_rows, report.new_rows), (0, 1));
    assert!(report.manifest.refresh_origin.is_some());
}
#[test]
fn corrupted_refresh_origin_is_rejected_even_with_force() {
    use std::os::unix::fs::PermissionsExt;
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.rs"), "fn a() {}\n").unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    let prior = extract::refresh(&store, &source, settings(endpoint, &["a.rs"]))
        .unwrap()
        .manifest
        .generation;
    worker.join().unwrap();
    let manifest = store.root().join(&prior).join("manifest.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    value["refresh_origin"]["generation"] = json!("mismatched-generation");
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&manifest, serde_json::to_vec(&value).unwrap()).unwrap();
    fs::set_permissions(&manifest, fs::Permissions::from_mode(0o400)).unwrap();
    let mut forced = settings("http://127.0.0.1:1".into(), &["a.rs"]);
    forced.force_reembed = true;
    assert!(extract::refresh(&store, &source, forced).is_err());
    assert_eq!(store.current().unwrap(), prior);
}
#[test]
fn changing_span_or_path_requires_new_embeddings() {
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.txt"), "x".repeat(200)).unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    extract::refresh(&store, &source, settings(endpoint, &["a.txt"])).unwrap();
    worker.join().unwrap();
    let mut changed = settings("http://127.0.0.1:1".into(), &["a.txt"]);
    changed.sources.chunk_bytes = 128;
    let spans = extract::plan(&source, &changed.sources).unwrap().rows;
    assert!(spans > 1);
    let (endpoint, worker) = server(sequence(vec![vec![0.0, 1.0]; spans]));
    changed.embedding.endpoint = endpoint;
    let result = extract::refresh(&store, &source, changed).unwrap();
    worker.join().unwrap();
    assert_eq!((result.reused_rows, result.new_rows), (0, spans as u64));
    fs::rename(source.join("a.txt"), source.join("b.txt")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]; spans]));
    let mut renamed = settings(endpoint, &["b.txt"]);
    renamed.sources.chunk_bytes = 128;
    let result = extract::refresh(&store, &source, renamed).unwrap();
    worker.join().unwrap();
    assert_eq!((result.reused_rows, result.new_rows), (0, spans as u64));
}
#[test]
fn empty_planned_file_drift_aborts_before_publication() {
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.rs"), "fn a() {}\n").unwrap();
    fs::write(source.join("empty.txt"), "").unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    let prior = extract::refresh(&store, &source, settings(endpoint, &["a.rs", "empty.txt"]))
        .unwrap()
        .manifest
        .generation;
    worker.join().unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    let mut forced = settings(endpoint, &["a.rs", "empty.txt"]);
    forced.force_reembed = true;
    let mut changed = false;
    let result = extract::refresh_with_progress(&store, &source, forced, |progress| {
        if progress.new_rows > 0 && !changed {
            fs::write(source.join("empty.txt"), "newly nonempty")?;
            changed = true;
        }
        Ok(())
    });
    assert!(result.is_err());
    worker.join().unwrap();
    assert_eq!(store.current().unwrap(), prior);
}
#[test]
fn cli_force_reembed_sends_new_request_for_unchanged_row() {
    let host = support::Host::new();
    let source = host.0.join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.rs"), "fn a() {}\n").unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    extract::refresh(&store, &source, settings(endpoint, &["a.rs"])).unwrap();
    worker.join().unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![0.0, 1.0]]));
    let cfg = host.0.join("refresh.json");
    fs::write(
        &cfg,
        serde_json::to_vec(&settings(endpoint, &["a.rs"])).unwrap(),
    )
    .unwrap();
    let output = std::process::Command::new(support::forge_binary::path())
        .args(["code-index", "refresh", "--index"])
        .arg(store.root())
        .arg("--source-root")
        .arg(&source)
        .arg("--config")
        .arg(&cfg)
        .arg("--force-reembed")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["reused_rows"], 0);
    assert_eq!(report["new_rows"], 1);
    assert_eq!(worker.join().unwrap().len(), 3);
}
#[test]
fn named_source_root_replacement_during_refresh_refuses_publication() {
    let host = support::Host::new();
    let source = host.0.join("source");
    let displaced = host.0.join("source-original");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("a.rs"), "fn a() {}\n").unwrap();
    fs::write(source.join("empty.txt"), "").unwrap();
    let store = Store::create(&host.0.join("index")).unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    let prior = extract::refresh(&store, &source, settings(endpoint, &["a.rs", "empty.txt"]))
        .unwrap()
        .manifest
        .generation;
    worker.join().unwrap();
    let (endpoint, worker) = server(sequence(vec![vec![1.0, 0.0]]));
    let mut forced = settings(endpoint, &["a.rs", "empty.txt"]);
    forced.force_reembed = true;
    let mut swapped = false;
    let result = extract::refresh_with_progress(&store, &source, forced, |progress| {
        if progress.new_rows > 0 && !swapped {
            fs::rename(&source, &displaced)?;
            fs::create_dir(&source)?;
            fs::write(source.join("a.rs"), "fn a() {}\n")?;
            fs::write(source.join("empty.txt"), "replacement content")?;
            swapped = true;
        }
        Ok(())
    });
    worker.join().unwrap();
    assert!(swapped);
    assert!(result.is_err());
    assert_eq!(store.current().unwrap(), prior);
    assert_eq!(fs::read(displaced.join("empty.txt")).unwrap(), b"");
}
