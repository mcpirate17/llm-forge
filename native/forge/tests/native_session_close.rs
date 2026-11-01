//! Session-close fixture tests; no shared state or real model calls.
#[path = "support/embedding_fixture.rs"]
mod embedding_fixture;
#[allow(dead_code)]
mod support;
use forge::ownership_lifecycle::{self as lifecycle, Create};
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Output;

fn fixture() -> support::Host {
    let host = support::Host::new();
    fs::write(host.0.join("pyproject.toml"), "").unwrap();
    fs::write(
        host.0.join(".current_work.md"),
        "# Active Coordination\n\n## Existing\n\nbody\n",
    )
    .unwrap();
    host
}
fn claim(host: &support::Host, owner: &str, path: &str) -> String {
    lifecycle::create(
        &host.0,
        Create {
            owner,
            paths: &[path.into()],
            justification: "test",
            expected_minutes: 45.0,
            max_minutes: 60.0,
        },
    )
    .unwrap()
    .claim_id
}
fn run(host: &support::Host, args: &[&str]) -> Output {
    host.command()
        .env("PATH", "")
        .args([
            "session",
            "close",
            "--host",
            host.0.to_str().unwrap(),
            "--owner",
            "lane-a",
            "--json",
        ])
        .args(args)
        .output()
        .unwrap()
}
fn value(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn absent_memory_inputs_and_invalid_claims_fail_before_status_or_release() {
    let host = fixture();
    let id = claim(&host, "lane-a", "src/a.rs");
    let before = fs::read(host.0.join(".current_work.md")).unwrap();
    let missing = run(
        &host,
        &["--title", "finish", "--body", "done", "--claim-id", &id],
    );
    assert!(!missing.status.success());
    assert_eq!(fs::read(host.0.join(".current_work.md")).unwrap(), before);
    assert_eq!(lifecycle::list(&host.0).unwrap().0.len(), 1);
    let invalid = run(
        &host,
        &[
            "--no-memory-index",
            "--title",
            "finish",
            "--body",
            "done",
            "--claim-id",
            &id,
            "--claim-id",
            "claim-missing",
        ],
    );
    assert!(!invalid.status.success());
    assert_eq!(fs::read(host.0.join(".current_work.md")).unwrap(), before);
    assert_eq!(lifecycle::list(&host.0).unwrap().0.len(), 1);
}

#[test]
fn multi_claim_release_validates_every_owner_and_publishes_once() {
    let host = fixture();
    let a = claim(&host, "lane-a", "src/a.rs");
    let b = claim(&host, "lane-b", "src/b.rs");
    assert!(lifecycle::release_many(&host.0, &[a.clone(), b.clone()], "lane-a", false).is_err());
    assert_eq!(lifecycle::list(&host.0).unwrap().0.len(), 2);
    let out = run(
        &host,
        &[
            "--no-memory-index",
            "--title",
            "finished",
            "--body",
            "bounded work complete",
            "--claim-id",
            &a,
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result = value(&out);
    assert_eq!(result["status"], "complete");
    assert_eq!(result["claims_released"], json!([a]));
    assert_eq!(result["memory_status"], "skipped");
    assert_eq!(lifecycle::list(&host.0).unwrap().0[0].claim_id, b);
}

#[test]
fn appended_status_with_failed_refresh_never_releases_claims() {
    let host = fixture();
    let id = claim(&host, "lane-a", "src/a.rs");
    fs::write(host.0.join("conductor"), b"not a directory").unwrap();
    let out = run(
        &host,
        &[
            "--no-memory-index",
            "--title",
            "finish",
            "--body",
            "done",
            "--claim-id",
            &id,
        ],
    );
    assert_eq!(out.status.code(), Some(3));
    let result = value(&out);
    assert_eq!(result["status_publication"], "published");
    assert_eq!(result["claim_release"], "not_requested");
    assert_eq!(lifecycle::list(&host.0).unwrap().0.len(), 1);
    assert!(fs::read_to_string(host.0.join(".current_work.md"))
        .unwrap()
        .contains("finish"));
}

#[test]
fn failed_mock_embedding_reports_partial_after_completed_release() {
    let host = fixture();
    let id = claim(&host, "lane-a", "src/a.rs");
    fs::create_dir_all(host.0.join("notes")).unwrap();
    fs::write(host.0.join("notes/a.md"), "# note\n\nbody\n").unwrap();
    fs::write(
        host.0.join("catalog.toml"),
        "schema_version=1\n[[source]]\nid='notes'\nkind='index'\nroot='notes'\nchunk='heading'\n",
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut client = loop {
            match listener.accept() {
                Ok((client, _)) => break client,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                Err(error) => panic!("mock embedding accept failed: {error}"),
            }
        };
        let mut request = [0; 4096];
        let _ = client.read(&mut request);
        client.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
    });
    fs::write(host.0.join("embedding.json"),json!({"endpoint":endpoint,"model":"test:latest","runner":"",
        "artifact_sha256":"a".repeat(64),"dimension":2,"context_tokens":2048,"threads":1,"gpu_layers":0,"timeout_seconds":2}).to_string()).unwrap();
    let store = host.0.join("memory-store");
    let out = run(
        &host,
        &[
            "--claim-id",
            &id,
            "--memory-embedding",
            "embedding.json",
            "--memory-store",
            store.to_str().unwrap(),
            "--memory-catalog",
            "catalog.toml",
        ],
    );
    worker.join().unwrap();
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result = value(&out);
    assert_eq!(result["claim_release"], "published");
    assert_eq!(result["claims_released"], json!([id]));
    assert_eq!(result["memory_status"], "failed");
    assert!(lifecycle::list(&host.0).unwrap().0.is_empty());
}

fn root_fixture(first: &str, second: &str) -> (support::Host, String) {
    let host = fixture();
    fs::create_dir(host.0.join(first)).unwrap();
    fs::write(host.0.join(first).join("note.md"), "a prior memory fact").unwrap();
    fs::write(host.0.join("catalog.toml"), format!("schema_version=1\n[[source]]\nid='notes'\nkind='index'\nroots=['{first}','{second}']\nchunk='whole'\n")).unwrap();
    let (endpoint, worker) = embedding_fixture::server(vec![
        embedding_fixture::reply("/api/tags", embedding_fixture::inventory(7)),
        embedding_fixture::reply("/api/embed", embedding_fixture::output()),
        embedding_fixture::reply("/api/tags", embedding_fixture::inventory(7)),
    ]);
    let config = embedding_fixture::config(endpoint);
    fs::write(
        host.0.join("embedding.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    forge::memory_ingest::ingest(forge::memory_ingest::Settings {
        host: host.0.clone(),
        store: host.0.join("memory-store"),
        catalog: Some(host.0.join("catalog.toml")),
        sources: vec![],
        full: false,
        allow_missing_roots: false,
        timeout_seconds: 10,
        embedding: config,
    })
    .unwrap();
    assert_eq!(worker.join().unwrap().len(), 3);
    let id = claim(&host, "lane-a", "src/a.rs");
    (host, id)
}

#[test]
fn missing_unused_sibling_root_does_not_block_session_close() {
    // `foo` must not match the prior row under `foobar` by string prefix.
    let (host, id) = root_fixture("foobar", "foo");
    let store = host.0.join("memory-store");
    let out = run(
        &host,
        &[
            "--claim-id",
            &id,
            "--memory-embedding",
            "embedding.json",
            "--memory-store",
            store.to_str().unwrap(),
            "--memory-catalog",
            "catalog.toml",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let result = value(&out);
    assert_eq!(result["claim_release"], "published");
    assert_eq!(result["memory_status"], "ok");
}

#[test]
fn missing_root_with_prior_rows_fails_before_status_and_claim_changes() {
    let (host, id) = root_fixture("actual", "other");
    fs::remove_dir_all(host.0.join("actual")).unwrap();
    fs::create_dir(host.0.join("other")).unwrap();
    fs::write(host.0.join("other/note.md"), "new fact").unwrap();
    let before = fs::read(host.0.join(".current_work.md")).unwrap();
    let store = host.0.join("memory-store");
    let out = run(
        &host,
        &[
            "--title",
            "finish",
            "--body",
            "done",
            "--claim-id",
            &id,
            "--memory-embedding",
            "embedding.json",
            "--memory-store",
            store.to_str().unwrap(),
            "--memory-catalog",
            "catalog.toml",
        ],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("refusing implicit removal"));
    assert_eq!(fs::read(host.0.join(".current_work.md")).unwrap(), before);
    assert_eq!(lifecycle::list(&host.0).unwrap().0.len(), 1);
}

#[test]
fn dangling_memory_pointer_fails_before_status_and_claim_changes() {
    let (host, id) = root_fixture("actual", "optional");
    let store = host.0.join("memory-store");
    fs::remove_file(store.join("current")).unwrap();
    std::os::unix::fs::symlink("missing", store.join("current")).unwrap();
    let before = fs::read(host.0.join(".current_work.md")).unwrap();
    let out = run(
        &host,
        &[
            "--title",
            "finish",
            "--body",
            "done",
            "--claim-id",
            &id,
            "--memory-embedding",
            "embedding.json",
            "--memory-store",
            store.to_str().unwrap(),
            "--memory-catalog",
            "catalog.toml",
        ],
    );
    assert!(!out.status.success());
    assert_eq!(fs::read(host.0.join(".current_work.md")).unwrap(), before);
    assert_eq!(lifecycle::list(&host.0).unwrap().0.len(), 1);
    assert!(fs::symlink_metadata(store.join("current"))
        .unwrap()
        .file_type()
        .is_symlink());
}
