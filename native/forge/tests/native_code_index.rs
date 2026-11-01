use conductor_native::{
    syntax::fingerprint,
    vector_index::{Config, Space},
};
use forge::code_index::{
    build, BuildHeader, EmbeddingRow, QueryRequest, Reader, SearchMode, Store,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "forge-code-index-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        for (path, text) in [
            ("a.rs", "pub fn alpha() {}\n"),
            ("b.rs", "pub fn beta() {}\n"),
            ("c.rs", "pub fn needle() {}\n"),
        ] {
            fs::write(source.join(path), text).unwrap();
        }
        let store = Store::create(&root.join("index")).unwrap();
        Self {
            root,
            source,
            store,
        }
    }
    fn rows(&self) -> Vec<anyhow::Result<EmbeddingRow>> {
        ["a.rs", "b.rs", "c.rs"]
            .into_iter()
            .zip([vec![1.0, 0.0], vec![0.8, 0.2], vec![0.0, 1.0]])
            .map(|(path, vector)| {
                let source = fs::read_to_string(self.source.join(path)).unwrap();
                Ok(EmbeddingRow {
                    path: path.into(),
                    source_sha256: fingerprint(&source),
                    start_byte: 0,
                    end_byte: source.len(),
                    vector,
                })
            })
            .collect()
    }
    fn build(&self) -> String {
        build(&self.store, &self.source, header(3), self.rows())
            .unwrap()
            .generation
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn space() -> Space {
    Space {
        dimension: 2,
        model_sha256: [4; 32],
    }
}
fn header(rows: usize) -> BuildHeader {
    BuildHeader {
        version: 1,
        rows,
        space: space(),
        config: Config::default(),
    }
}
fn request(text: &str, semantic: bool) -> QueryRequest {
    QueryRequest {
        text: text.into(),
        vector: semantic.then(|| vec![1.0, 0.0]),
        space: semantic.then(space),
        mode: SearchMode::Exact,
        limit: 3,
        candidates: 3,
        ef: 64,
    }
}

#[test]
fn lexical_and_semantic_candidates_fuse_with_exact_scores_and_current_source_spans() {
    let fixture = Fixture::new();
    let id = fixture.build();
    let mut reader = Reader::open(&fixture.store).unwrap();
    let lexical = reader
        .search(&fixture.source, request("needle", false))
        .unwrap();
    assert_eq!(lexical.generation, id);
    assert_eq!(lexical.hits.len(), 1);
    assert_eq!(lexical.hits[0].path, "c.rs");
    let semantic = reader.search(&fixture.source, request("", true)).unwrap();
    assert_eq!(semantic.hits[0].path, "a.rs");
    assert_eq!(semantic.hits[0].cosine, Some(1.0));
    let hybrid = reader
        .search(&fixture.source, request("needle", true))
        .unwrap();
    assert_eq!(hybrid.hits.len(), 3);
    assert_eq!(hybrid.hits[0].path, "c.rs");
    for hit in &hybrid.hits {
        let actual = fs::read_to_string(fixture.source.join(&hit.path)).unwrap();
        assert_eq!(hit.text, actual[hit.start_byte..hit.end_byte]);
        assert_eq!(hit.source_sha256, fingerprint(&actual));
    }
    let mut approximate = request("needle", true);
    approximate.mode = SearchMode::Hnsw;
    let approximate = reader.search(&fixture.source, approximate).unwrap();
    assert_eq!(
        approximate.hits.iter().map(|h| &h.path).collect::<Vec<_>>(),
        hybrid.hits.iter().map(|h| &h.path).collect::<Vec<_>>()
    );
    for text in [
        "\"needle\" OR other",
        "needle) OR (path:*",
        "\"",
        "*",
        "NOT",
        "NEAR(a b)",
    ] {
        reader
            .search(&fixture.source, request(text, false))
            .unwrap();
    }
    assert!(reader
        .search(&fixture.root, request("needle", false))
        .is_err());
    let mut wrong = request("needle", true);
    wrong.space.as_mut().unwrap().model_sha256[0] ^= 1;
    assert!(reader.search(&fixture.source, wrong).is_err());
}

#[test]
fn stale_deleted_and_new_sources_have_explicit_snapshot_coverage() {
    let fixture = Fixture::new();
    fixture.build();
    let mut reader = Reader::open(&fixture.store).unwrap();
    fs::write(fixture.source.join("a.rs"), "pub fn changed() {}\n").unwrap();
    fs::remove_file(fixture.source.join("b.rs")).unwrap();
    fs::write(fixture.source.join("new.rs"), "pub fn needle_new() {}\n").unwrap();
    let result = reader
        .search(&fixture.source, request("needle", true))
        .unwrap();
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].path, "c.rs");
    assert_eq!(result.stale_candidates, 1);
    assert_eq!(result.unreadable_candidates, 1);
    assert_eq!(result.read_errors.len(), 1);
    assert_eq!(result.read_errors[0].path, "b.rs");
    assert!(!result.read_errors[0].error.is_empty());
    assert!(result.coverage.contains("new paths require refresh"));
}

#[test]
fn failed_builds_preserve_current_generation_and_remove_only_their_staging() {
    let fixture = Fixture::new();
    let current = fixture.build();
    for case in 0..7 {
        let mut rows = fixture.rows();
        match case {
            0 => {
                rows.swap(0, 1);
            }
            1 => {
                rows[0].as_mut().unwrap().source_sha256 = "0".repeat(64);
            }
            2 => {
                rows[0].as_mut().unwrap().path = "../outside.rs".into();
            }
            3 => {
                rows[0].as_mut().unwrap().end_byte = usize::MAX;
            }
            4 => {
                rows[0].as_mut().unwrap().vector = vec![f32::NAN, 0.0];
            }
            5 => {
                rows.pop();
            }
            6 => {
                rows[0] = Err(anyhow::anyhow!("stream I/O failed"));
            }
            _ => unreachable!(),
        }
        assert!(build(&fixture.store, &fixture.source, header(3), rows).is_err());
        assert_eq!(fixture.store.current().unwrap(), current);
        let count = fs::read_dir(fixture.store.root())
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_type().unwrap().is_dir())
            .count();
        assert_eq!(count, 1, "leaked failed generation for case {case}");
        Reader::open(&fixture.store).unwrap();
    }
}

#[test]
fn published_generations_are_immutable_and_leased_readers_survive_refresh_and_collection() {
    let fixture = Fixture::new();
    let original = fixture.build();
    let mut old = Reader::open(&fixture.store).unwrap();
    let second = fixture.build();
    let third = fixture.build();
    let fourth = fixture.build();
    assert_eq!(fixture.store.current().unwrap(), fourth);
    assert!(fixture.store.root().join(&original).exists());
    assert!(!fixture.store.root().join(&second).exists());
    assert!(fixture.store.root().join(&third).exists());
    assert_eq!(
        old.search(&fixture.source, request("needle", true))
            .unwrap()
            .generation,
        original
    );
    drop(old);
    fixture.build();
    assert!(!fixture.store.root().join(original).exists());
    let current = fixture.store.current().unwrap();
    let component = fixture.store.root().join(current).join("chunks.sqlite");
    fs::set_permissions(&component, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(Reader::open(&fixture.store).is_err());
    let mut bytes = fs::read(&component).unwrap();
    let end = bytes.len() - 1;
    bytes[end] ^= 1;
    fs::write(&component, bytes).unwrap();
    fs::set_permissions(&component, fs::Permissions::from_mode(0o400)).unwrap();
    assert!(Reader::open(&fixture.store).is_err());
}

#[test]
fn snapshot_query_is_concurrent_with_builder_publication() {
    let fixture = Fixture::new();
    fixture.build();
    let mut reader = Reader::open(&fixture.store).unwrap();
    let source = fixture.source.clone();
    let worker = std::thread::spawn(move || {
        for _ in 0..100 {
            assert_eq!(
                reader
                    .search(&source, request("needle", true))
                    .unwrap()
                    .hits
                    .len(),
                3
            );
        }
    });
    for _ in 0..4 {
        fixture.build();
    }
    worker.join().unwrap();
}

#[test]
fn cli_build_query_and_status_work_with_empty_path_and_reject_oversized_frames() {
    let fixture = Fixture::new();
    let input = fixture.root.join("embeddings.jsonl");
    let mut lines = vec![serde_json::to_string(&header(3)).unwrap()];
    lines.extend(
        fixture
            .rows()
            .into_iter()
            .map(|row| serde_json::to_string(&row.unwrap()).unwrap()),
    );
    fs::write(&input, lines.join("\n")).unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_forge"));
    command
        .env_clear()
        .env("PATH", "")
        .args(["code-index", "build", "--index"])
        .arg(fixture.store.root())
        .arg("--source-root")
        .arg(&fixture.source)
        .arg("--input")
        .arg(&input);
    let result =
        forge::process_probe::run(&mut command, std::time::Duration::from_secs(5), 65536).unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let query_file = fixture.root.join("query.json");
    fs::write(
        &query_file,
        serde_json::to_vec(&request("needle", true)).unwrap(),
    )
    .unwrap();
    for action in ["query", "status"] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_forge"));
        command
            .env_clear()
            .env("PATH", "")
            .args(["code-index", action, "--index"])
            .arg(fixture.store.root());
        if action == "query" {
            command
                .arg("--source-root")
                .arg(&fixture.source)
                .arg("--request")
                .arg(&query_file);
        }
        let result =
            forge::process_probe::run(&mut command, std::time::Duration::from_secs(5), 65536)
                .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        if action == "query" {
            assert_eq!(value["hits"][0]["path"], "c.rs");
        }
    }
    let current = fixture.store.current().unwrap();
    fs::write(&input, vec![b' '; 1024 * 1024 + 1]).unwrap();
    let result =
        forge::process_probe::run(&mut command, std::time::Duration::from_secs(5), 65536).unwrap();
    assert!(!result.status.success());
    assert_eq!(fixture.store.current().unwrap(), current);
}

#[test]
fn escaped_output_and_failed_source_reads_obey_byte_budgets() {
    let fixture = Fixture::new();
    let content = "\u{1}".repeat(30 * 16384);
    fs::write(fixture.source.join("a.rs"), &content).unwrap();
    let rows = (0..30).map(|i| {
        Ok(EmbeddingRow {
            path: "a.rs".into(),
            source_sha256: fingerprint(&content),
            start_byte: i * 16384,
            end_byte: (i + 1) * 16384,
            vector: vec![1.0, 0.0],
        })
    });
    build(&fixture.store, &fixture.source, header(30), rows).unwrap();
    let mut query = request("", true);
    query.limit = 30;
    query.candidates = 30;
    let result = Reader::open(&fixture.store)
        .unwrap()
        .search(&fixture.source, query)
        .unwrap();
    assert!(result.output_truncated);
    assert!(!result.hits.is_empty());
    assert!(serde_json::to_vec(&result).unwrap().len() <= 1024 * 1024);

    let content = "x\n".repeat(70);
    fs::write(fixture.source.join("a.rs"), &content).unwrap();
    let rows = (0..70).map(|i| {
        Ok(EmbeddingRow {
            path: "a.rs".into(),
            source_sha256: fingerprint(&content),
            start_byte: i * 2,
            end_byte: (i + 1) * 2,
            vector: vec![1.0, 0.0],
        })
    });
    build(&fixture.store, &fixture.source, header(70), rows).unwrap();
    let mut reader = Reader::open(&fixture.store).unwrap();
    fs::write(fixture.source.join("a.rs"), vec![0xff; 1024 * 1024]).unwrap();
    let mut query = request("", true);
    query.limit = 70;
    query.candidates = 70;
    query.ef = 70;
    let result = reader.search(&fixture.source, query).unwrap();
    assert!(result.freshness_budget_exhausted);
    assert!(result.hits.is_empty());
    assert_eq!(result.unreadable_candidates, 64);
    assert_eq!(result.read_errors.len(), 4);
    assert_eq!(result.source_read_budget_charged, 64 * 1024 * 1024);
}

#[test]
fn publishing_an_empty_corpus_clears_search_without_destroying_pinned_readers() {
    let fixture = Fixture::new();
    let prior = fixture.build();
    let mut old = Reader::open(&fixture.store).unwrap();
    let empty = build(&fixture.store, &fixture.source, header(0), Vec::new()).unwrap();
    assert_ne!(empty.generation, prior);
    assert_eq!(empty.rows, 0);
    let mut reader = Reader::open(&fixture.store).unwrap();
    for semantic in [true, false] {
        let result = reader
            .search(&fixture.source, request("needle", semantic))
            .unwrap();
        assert!(result.hits.is_empty());
        assert_eq!(result.indexed_rows, 0);
    }
    assert!(!old
        .search(&fixture.source, request("needle", true))
        .unwrap()
        .hits
        .is_empty());
}

#[test]
fn reader_limits_reject_large_generations_before_mapping() {
    let fixture = Fixture::new();
    fixture.build();
    for limits in [
        forge::code_index::ReadLimits {
            max_rows: 2,
            max_mapped_bytes: 1024 * 1024,
        },
        forge::code_index::ReadLimits {
            max_rows: 3,
            max_mapped_bytes: 264,
        },
    ] {
        assert!(Reader::open_limited(&fixture.store, limits).is_err());
    }
    assert!(Reader::open_limited(
        &fixture.store,
        forge::code_index::ReadLimits {
            max_rows: 3,
            max_mapped_bytes: 1024 * 1024
        }
    )
    .is_ok());
}
