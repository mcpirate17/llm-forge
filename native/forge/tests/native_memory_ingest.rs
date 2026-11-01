#[path = "support/embedding_fixture.rs"]
mod embedding_fixture;
#[allow(dead_code)]
mod support;
use embedding_fixture::{config, inventory, reply, server, Reply, Requests};
use forge::embedding::Config;
use forge::memory_ingest::{fingerprint, ingest, search, Report, Settings};
use forge::memory_store::Store;
use serde_json::{json, Value};
use std::fs;
use std::process::Command;
use std::thread::JoinHandle;
use std::time::Duration;

struct Fixture(support::Host);
impl Fixture {
    fn new(entries: &[(&str, &str)]) -> Self {
        let host = support::Host::new();
        fs::write(host.0.join("pyproject.toml"), "").unwrap();
        let fixture = Self(host);
        fixture.catalog(entries);
        fixture
    }
    fn catalog(&self, entries: &[(&str, &str)]) {
        let mut text = "schema_version=1\n".to_owned();
        for (id, mode) in entries {
            text.push_str(&format!(
                "[[source]]\nid='{id}'\nkind='index'\nroot='{id}'\nchunk='{mode}'\n"
            ));
        }
        fs::write(self.0 .0.join("catalog.toml"), text).unwrap();
    }
    fn file(&self, relative: &str, text: impl AsRef<[u8]>) {
        let path = self.0 .0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn settings(&self, embedding: Config, selected: &[&str], full: bool) -> Settings {
        Settings {
            host: self.0 .0.clone(),
            store: self.0 .0.join("store"),
            catalog: Some(self.0 .0.join("catalog.toml")),
            sources: selected.iter().map(|s| (*s).to_owned()).collect(),
            full,
            allow_missing_roots: false,
            timeout_seconds: 10,
            embedding,
        }
    }
    fn store(&self) -> Store {
        Store::open(&self.0 .0.join("store")).unwrap()
    }
    fn rows(&self) -> Vec<Value> {
        let pin = self.store().pin().unwrap();
        fs::read_to_string(pin.source)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

fn dead() -> Config {
    let (endpoint, worker) = server(Vec::new());
    assert!(worker.join().unwrap().is_empty());
    config(endpoint)
}
fn batch_replies(config: &Config, counts: &[usize]) -> Vec<Reply> {
    let mut tags = inventory(7);
    tags["models"][0]["name"] = json!(config.model);
    tags["models"][0]["digest"] = json!(config
        .artifact_sha256
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>());
    counts.iter().flat_map(|count| [
        reply("/api/tags", tags.clone()),
        reply("/api/embed", json!({"model":config.model,"embeddings":vec![vec![3.0,4.0];*count],"prompt_eval_count":8 * count})),
        reply("/api/tags", tags.clone()),
    ]).collect()
}
fn serving(mut config: Config, counts: &[usize]) -> (Config, JoinHandle<Requests>) {
    let (endpoint, worker) = server(batch_replies(&config, counts));
    config.endpoint = endpoint;
    (config, worker)
}
fn build(
    fixture: &Fixture,
    config: Config,
    counts: &[usize],
    selected: &[&str],
    full: bool,
) -> (Report, Requests) {
    let (config, worker) = serving(config, counts);
    let result = ingest(fixture.settings(config, selected, full));
    let requests = worker.join().unwrap();
    (result.unwrap(), requests)
}

#[test]
fn initial_ingestion_query_and_generation_metadata_bind_the_native_model_recipe() {
    let fixture = Fixture::new(&[("notes", "whole")]);
    fixture.file(
        "notes/native.md",
        "# Native notes\nA durable contextual fact.",
    );
    let base = dead();
    let expected_fingerprint = fingerprint(&base).unwrap();
    let (report, requests) = build(&fixture, base.clone(), &[1], &[], true);
    assert_eq!((report.rows, report.embedded, report.reused), (1, 1, 0));
    assert!(report.changed);
    assert_eq!(report.embedding_input_tokens, 8);
    assert_eq!(requests.len(), 3);
    let request: Value = serde_json::from_slice(&requests[1].1).unwrap();
    assert_eq!(request["options"]["num_ctx"], 2048);
    assert_eq!(request["options"]["num_gpu"], 0);
    assert_eq!(request["truncate"], false);
    let pin = fixture.store().pin().unwrap();
    assert_eq!(pin.manifest.generation, report.generation);
    assert_eq!(pin.manifest.fingerprint, expected_fingerprint);
    assert_eq!(pin.manifest.dimension, 2);
    assert_eq!(pin.manifest.sources, ["notes"]);
    pin.reader.verify_source_hash().unwrap();
    let rows = fixture.rows();
    assert_eq!(rows[0]["schema_version"], 3);
    assert_eq!(rows[0]["embedding"]["fingerprint"], expected_fingerprint);
    assert_eq!(rows[0]["embedding"]["native_recipe"], "forge-memory-v1");
    assert_eq!(rows[0]["embedding"]["paid"], false);
    assert_eq!(rows[0]["embedding"]["num_ctx"], 2048);
    assert_eq!(rows[0]["embedding"]["num_gpu"], 0);
    assert_eq!(rows[0]["source_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(rows[0]["recipe_sha256"].as_str().unwrap().len(), 64);
    let (query_config, worker) = serving(base, &[1]);
    let result = search(&fixture.store(), query_config, "  native fact  ", 1).unwrap();
    let requests = worker.join().unwrap();
    assert_eq!(result["generation"], report.generation);
    assert_eq!(result["embedding_fingerprint"], expected_fingerprint);
    assert!(result["hits"][0]["text"]
        .as_str()
        .unwrap()
        .contains("durable contextual fact"));
    let request: Value = serde_json::from_slice(&requests[1].1).unwrap();
    assert_eq!(
        request["input"][0],
        "Instruct: Retrieve the developer note that answers the search query.\nQuery: native fact"
    );
}

#[test]
fn unchanged_incremental_reuses_rows_with_no_http_and_keeps_the_current_generation() {
    let fixture = Fixture::new(&[("notes", "whole")]);
    fixture.file("notes/a.md", "An unchanged note.");
    let (first, _) = build(&fixture, dead(), &[1], &[], true);
    let result = ingest(fixture.settings(dead(), &[], false)).unwrap();
    assert!(!result.changed);
    assert_eq!(result.generation, first.generation);
    assert_eq!(
        (
            result.rows,
            result.reused,
            result.embedded,
            result.embedding_input_tokens
        ),
        (1, 1, 0, 0)
    );
    assert_eq!(fixture.store().current().unwrap(), first.generation);
}

#[test]
fn partial_updates_preserve_other_sources_and_remove_only_selected_deleted_rows() {
    let fixture = Fixture::new(&[("a", "whole"), ("b", "whole")]);
    fixture.file("a/keep.md", "old selected fact");
    fixture.file("a/remove.md", "selected deleted fact");
    fixture.file("b/stable.md", "unselected retained fact");
    let error = ingest(fixture.settings(dead(), &["a"], false))
        .err()
        .unwrap();
    assert!(error.to_string().contains("partial ingestion"));
    build(&fixture, dead(), &[1, 1, 1], &[], true);
    fs::remove_file(fixture.0 .0.join("a/remove.md")).unwrap();
    fixture.file("a/keep.md", "new selected fact");
    let (result, requests) = build(&fixture, dead(), &[1], &["a"], false);
    assert_eq!(requests.len(), 3);
    assert_eq!(
        (
            result.rows,
            result.preserved,
            result.removed,
            result.embedded
        ),
        (2, 1, 1, 1)
    );
    let rows = fixture.rows();
    assert!(rows
        .iter()
        .any(|row| row["source"] == "b" && row["text"] == "unselected retained fact"));
    assert!(rows
        .iter()
        .any(|row| row["source"] == "a" && row["text"] == "new selected fact"));
    fs::remove_file(fixture.0 .0.join("a/keep.md")).unwrap();
    let result = ingest(fixture.settings(dead(), &["a"], false)).unwrap();
    assert_eq!(
        (
            result.rows,
            result.preserved,
            result.removed,
            result.embedded
        ),
        (1, 1, 1, 0)
    );
    assert_eq!(fixture.rows()[0]["source"], "b");
}

#[test]
fn chunk_recipe_change_reembeds_unchanged_source_bytes() {
    let fixture = Fixture::new(&[("notes", "whole")]);
    fixture.file(
        "notes/a.md",
        format!(
            "# First\n{}\n# Second\n{}\n",
            "a".repeat(500),
            "b".repeat(500)
        ),
    );
    let (first, _) = build(&fixture, dead(), &[1], &[], true);
    let first_row = fixture.rows().remove(0);
    fixture.catalog(&[("notes", "heading")]);
    let (result, requests) = build(&fixture, dead(), &[2], &[], false);
    assert_eq!((result.rows, result.reused, result.embedded), (2, 0, 2));
    assert_ne!(result.generation, first.generation);
    assert_eq!(requests.len(), 3);
    for row in fixture.rows() {
        assert_eq!(row["source_sha256"], first_row["source_sha256"]);
        assert_ne!(row["recipe_sha256"], first_row["recipe_sha256"]);
    }
}

#[test]
fn numerical_and_model_recipe_changes_refuse_partial_updates_and_require_full_reindex() {
    let fixture = Fixture::new(&[("a", "whole"), ("b", "whole")]);
    fixture.file("a/a.md", "fact a");
    fixture.file("b/b.md", "fact b");
    build(&fixture, dead(), &[1, 1], &[], true);
    let mut recipe = dead();
    for change in 0..2 {
        let previous = fixture.store().current().unwrap();
        if change == 0 {
            recipe.threads = 2;
        } else {
            recipe.model = "changed-embed:test".into();
            recipe.artifact_sha256 = [8; 32];
        }
        let error = ingest(fixture.settings(recipe.clone(), &["a"], false))
            .err()
            .unwrap();
        assert!(error.to_string().contains("full ingestion required"));
        assert_eq!(fixture.store().current().unwrap(), previous);
        let (result, _) = build(&fixture, recipe.clone(), &[1, 1], &[], true);
        assert_eq!((result.rows, result.reused, result.embedded), (2, 0, 2));
        assert_eq!(
            fixture.store().pin().unwrap().manifest.fingerprint,
            fingerprint(&recipe).unwrap()
        );
    }
    let error = search(&fixture.store(), dead(), "fact", 1).unwrap_err();
    assert!(error.to_string().contains("recipe differs"));
}

#[test]
fn read_missing_root_and_embedding_failures_leave_the_published_generation_intact() {
    let fixture = Fixture::new(&[("notes", "whole")]);
    fixture.file("notes/a.md", "original retained fact");
    let (first, _) = build(&fixture, dead(), &[1], &[], true);
    for bytes in [vec![0xff], b"nul\0body".to_vec()] {
        fixture.file("notes/a.md", bytes);
        assert!(ingest(fixture.settings(dead(), &[], false)).is_err());
        assert_eq!(fixture.store().current().unwrap(), first.generation);
    }
    fs::remove_file(fixture.0 .0.join("notes/a.md")).unwrap();
    fs::remove_dir(fixture.0 .0.join("notes")).unwrap();
    let error = ingest(fixture.settings(dead(), &[], false)).err().unwrap();
    assert!(error.to_string().contains("missing"));
    assert_eq!(fixture.store().current().unwrap(), first.generation);
    fixture.file("notes/a.md", "changed fact requiring embedding");
    let mut refused = reply("/api/embed", json!({"error":"fixture refusal"}));
    refused.status = 503;
    let (endpoint, worker) = server(vec![reply("/api/tags", inventory(7)), refused]);
    assert!(ingest(fixture.settings(config(endpoint), &[], false)).is_err());
    assert_eq!(worker.join().unwrap().len(), 2);
    assert_eq!(fixture.store().current().unwrap(), first.generation);
    // A successful embedding does not authorize publication after a source
    // becomes unreadable during the final source-identity recheck.
    let path = fixture.0 .0.join("notes/a.md");
    let mut final_tags = reply("/api/tags", inventory(7));
    final_tags.before_reply = Some(Box::new(move || fs::remove_file(path).unwrap()));
    let (endpoint, worker) = server(vec![
        reply("/api/tags", inventory(7)),
        reply("/api/embed", embedding_fixture::output()),
        final_tags,
    ]);
    assert!(ingest(fixture.settings(config(endpoint), &[], false)).is_err());
    assert_eq!(worker.join().unwrap().len(), 3);
    assert_eq!(fixture.store().current().unwrap(), first.generation);
    let pin = fixture.store().pin().unwrap();
    pin.reader.verify_source_hash().unwrap();
    assert!(fixture.rows()[0]["text"]
        .as_str()
        .unwrap()
        .contains("original retained fact"));
}

#[test]
fn pinned_generation_remains_queryable_while_publications_collect_unpinned_history() {
    let fixture = Fixture::new(&[("notes", "whole")]);
    fixture.file("notes/a.md", "first pinned fact");
    let (first, _) = build(&fixture, dead(), &[1], &[], true);
    let pin = fixture.store().pin().unwrap();
    let mut generations = Vec::new();
    for number in 2..=4 {
        fixture.file("notes/a.md", format!("updated fact {number}"));
        generations.push(build(&fixture, dead(), &[1], &[], false).0.generation);
    }
    assert!(fixture.store().root().join(&first.generation).exists());
    assert!(
        !fixture.store().root().join(&generations[0]).exists(),
        "eligible unpinned generation should be collected"
    );
    let hits = pin
        .reader
        .query(&[0.6, 0.8], 1, &pin.manifest.fingerprint)
        .unwrap();
    assert_eq!(hits[0].4, "first pinned fact");
    assert!(fixture.rows()[0]["text"]
        .as_str()
        .unwrap()
        .contains("updated fact 4"));
}

#[test]
fn public_ingest_search_and_inspect_work_without_path_or_an_interpreter() {
    let fixture = Fixture::new(&[("notes", "whole")]);
    fixture.file("notes/a.md", "CLI searchable fact");
    let (config, worker) = serving(dead(), &[1, 1]);
    let config_path = fixture.0 .0.join("embedding.json");
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    let command = |action: &str| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
        command
            .env_clear()
            .env("PATH", "")
            .env("HOME", &fixture.0 .0)
            .args(["memory", action, "--store"])
            .arg(fixture.0 .0.join("store"));
        command
    };
    let mut ingest = command("ingest");
    ingest
        .arg("--host")
        .arg(&fixture.0 .0)
        .arg("--catalog")
        .arg(fixture.0 .0.join("catalog.toml"))
        .arg("--embedding")
        .arg(&config_path)
        .arg("--full");
    let output = forge::process_probe::run(&mut ingest, Duration::from_secs(10), 65536).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let initial: Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut search = command("search");
    search
        .arg("--embedding")
        .arg(&config_path)
        .args(["--task", "searchable", "--top-k", "1"]);
    let output = forge::process_probe::run(&mut search, Duration::from_secs(10), 65536).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let queried: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(queried["generation"], initial["generation"]);
    assert_eq!(queried["hits"][0]["text"], "CLI searchable fact");
    let output =
        forge::process_probe::run(&mut command("inspect"), Duration::from_secs(5), 65536).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(manifest["generation"], initial["generation"]);
    assert_eq!(worker.join().unwrap().len(), 6);
}
