#[path = "support/embedding_fixture.rs"]
mod fixture;
use fixture::*;
use forge::embedding::Client;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
#[test]
fn pinned_native_client_normalizes_and_binds_numerical_recipe() {
    let (endpoint, worker) = server(vec![
        reply("/api/tags", inventory(7)),
        reply("/api/embed", output()),
        reply("/api/tags", inventory(7)),
    ]);
    let settings = config(endpoint);
    let client = Client::new(settings.clone()).unwrap();
    let embedded = client.embed("fn search() {}").unwrap();
    assert_eq!(embedded.vector, [0.6, 0.8]);
    assert_eq!(embedded.input_tokens, 8);
    assert_eq!(embedded.space, settings.space().unwrap());
    let calls = worker.join().unwrap();
    assert_eq!(calls.len(), 3);
    let body: Value = serde_json::from_slice(&calls[1].1).unwrap();
    assert_eq!(body["truncate"], false);
    assert_eq!(body["input"], json!(["fn search() {}"]));
    assert_eq!(body["options"]["num_gpu"], 0);
    assert_eq!(body["options"]["num_thread"], 1);
    let mut changed = settings.clone();
    changed.threads = 2;
    assert_ne!(settings.space().unwrap(), changed.space().unwrap());
    changed = settings.clone();
    changed.endpoint = "http://192.168.1.2:11434".into();
    assert_eq!(settings.space().unwrap(), changed.space().unwrap());
}

#[test]
fn model_changes_remote_backends_and_invalid_inference_fail_explicitly() {
    let mut remote = inventory(7);
    remote["models"][0]["remote_host"] = json!("https://cloud.invalid");
    for tags in [inventory(8), remote, json!({"models":[]})] {
        let (endpoint, worker) = server(vec![reply("/api/tags", tags)]);
        assert!(Client::new(config(endpoint))
            .unwrap()
            .embed("text")
            .is_err());
        assert_eq!(worker.join().unwrap().len(), 1);
    }
    let (endpoint, worker) = server(vec![
        reply("/api/tags", inventory(7)),
        reply("/api/embed", output()),
        reply("/api/tags", inventory(8)),
    ]);
    assert!(Client::new(config(endpoint))
        .unwrap()
        .embed("text")
        .unwrap_err()
        .to_string()
        .contains("artifact changed"));
    worker.join().unwrap();
    for data in [
        json!({"model":"wrong","embeddings":[[1,0]],"prompt_eval_count":2}),
        json!({"model":"local-embed:test","embeddings":[[1]],"prompt_eval_count":2}),
        json!({"model":"local-embed:test","embeddings":[[0,0]],"prompt_eval_count":2}),
        json!({"model":"local-embed:test","embeddings":[[1,0]],"prompt_eval_count":99999}),
    ] {
        let (endpoint, worker) = server(vec![
            reply("/api/tags", inventory(7)),
            reply("/api/embed", data),
        ]);
        assert!(Client::new(config(endpoint))
            .unwrap()
            .embed("text")
            .is_err());
        worker.join().unwrap();
    }
}

#[test]
fn network_and_body_bounds_fail_without_following_redirects() {
    for endpoint in [
        "https://127.0.0.1:443",
        "http://localhost:11434",
        "http://8.8.8.8:80",
        "http://127.0.0.1:0",
        "http://user@127.0.0.1:11434",
        "http://127.0.0.1:11434/path",
        "http://127.0.0.1:11434/",
    ] {
        assert!(
            Client::new(config(endpoint.into())).is_err(),
            "accepted endpoint {endpoint}"
        );
    }
    for (status, body) in [
        (302, json!({})),
        (500, json!({"error":"fixture failure"})),
        (200, json!({"large":"x".repeat(1024 * 1024)})),
    ] {
        let mut response = reply("/api/tags", body);
        response.status = status;
        let (endpoint, worker) = server(vec![response]);
        assert!(Client::new(config(endpoint))
            .unwrap()
            .embed("text")
            .is_err());
        worker.join().unwrap();
    }
    let mut response = reply("/api/tags", json!({}));
    response.wait_for_close = true;
    let (endpoint, worker) = server(vec![response]);
    let mut settings = config(endpoint);
    settings.timeout_seconds = 1;
    let started = Instant::now();
    assert!(Client::new(settings).unwrap().embed("text").is_err());
    assert!(started.elapsed() < Duration::from_secs(3));
    worker.join().unwrap();
}

#[test]
fn batch_order_bounds_and_caller_deadlines_are_enforced() {
    let outputs =
        json!({"model":"local-embed:test","embeddings":[[3,4],[-1,0]],"prompt_eval_count":15});
    let (endpoint, worker) = server(vec![
        reply("/api/tags", inventory(7)),
        reply("/api/embed", outputs),
        reply("/api/tags", inventory(7)),
    ]);
    let client = Client::new(config(endpoint)).unwrap();
    let result = client
        .embed_batch(
            &["first", "second"],
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap();
    assert_eq!(result.vectors, [vec![0.6, 0.8], vec![-1.0, 0.0]]);
    assert_eq!(result.input_tokens_total, 15);
    let calls = worker.join().unwrap();
    let body: Value = serde_json::from_slice(&calls[1].1).unwrap();
    assert_eq!(body["input"], json!(["first", "second"]));
    assert!(client.embed_batch(&[], Instant::now()).is_err());
    assert!(client.embed_batch(&[""], Instant::now()).is_err());
    assert!(client.embed_batch(&["text"; 33], Instant::now()).is_err());
    assert!(client
        .embed_batch(&[&"x".repeat(16385)], Instant::now())
        .is_err());
    assert!(client
        .embed_batch(&["valid"], Instant::now() - Duration::from_secs(1))
        .unwrap_err()
        .to_string()
        .contains("deadline expired"));
}
