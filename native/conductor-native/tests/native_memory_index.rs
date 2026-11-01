#![cfg(feature = "vector-search")]
use conductor_native::memory_index_sidecar as sidecar;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    binary: PathBuf,
}
impl Fixture {
    fn new(rows: &[Value]) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "forge-native-memory-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let source = root.join("rows.jsonl");
        let binary = root.join("vectors.bin");
        let text = rows
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&source, text).unwrap();
        Self {
            root,
            source,
            binary,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn row(id: usize, vector: Vec<f64>) -> Value {
    json!({"schema_version":3,"embedding":{"fingerprint":"sha256:native-test","dimension":vector.len(),"paid":true},
        "source_sha256":format!("{id:064x}"),"source":"code","path":format!("{id}.rs"),"title":format!("row {id}"),"text":"fn test() {}","vector":vector})
}

#[test]
fn compiler_rejects_special_oversized_and_unbounded_source_files() {
    let fixture = Fixture::new(&[row(0, vec![1.0])]);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&fixture.source)
        .unwrap();
    file.set_len(512 * 1024 * 1024 + 1).unwrap();
    assert!(sidecar::build_sidecar(&fixture.source, &fixture.binary)
        .unwrap_err()
        .contains("512 MiB"));
    let link = fixture.root.join("link");
    std::os::unix::fs::symlink(&fixture.source, &link).unwrap();
    assert!(sidecar::build_sidecar(&link, &fixture.binary).is_err());
    assert!(sidecar::build_sidecar(&fixture.root, &fixture.binary)
        .unwrap_err()
        .contains("regular file"));
    fs::write(&fixture.source, vec![b' '; 1024 * 1024 + 1]).unwrap();
    assert!(sidecar::build_sidecar(&fixture.source, &fixture.binary)
        .unwrap_err()
        .contains("exceeds 1 MiB"));
}

#[test]
fn mapped_entrypoints_preserve_stable_ties_and_source_offsets() {
    let fixture = Fixture::new(&[
        row(0, vec![1.0, 0.0]),
        row(1, vec![1.0, 0.0]),
        row(2, vec![0.0, 1.0]),
    ]);
    assert_eq!(
        sidecar::build_sidecar(&fixture.source, &fixture.binary).unwrap(),
        3
    );
    assert_eq!(
        sidecar::header(&fixture.binary).unwrap(),
        ("sha256:native-test".into(), 2, 3)
    );
    assert!(sidecar::sidecar_is_fresh(&fixture.source, &fixture.binary).unwrap());
    let reader = sidecar::Reader::open(&fixture.source, &fixture.binary).unwrap();
    let hits = reader.query(&[1.0, 0.0], 2, "sha256:native-test").unwrap();
    assert_eq!(
        hits.iter().map(|h| h.2.as_str()).collect::<Vec<_>>(),
        vec!["0.rs", "1.rs"]
    );
    let mapped = sidecar::query_sidecar(&fixture.binary, &[1.0, 0.0], 2).unwrap();
    assert_eq!(mapped[0], (1.0, 0));
    assert_eq!(mapped[1].0, 1.0);
    assert!(mapped[1].1 > 0);
}

#[test]
fn native_queries_reject_mixed_spaces_nonfinite_values_and_unbounded_requests() {
    let mixed = Fixture::new(&[row(0, vec![1.0]), row(1, vec![1.0, 2.0])]);
    assert!(sidecar::build_sidecar(&mixed.source, &mixed.binary)
        .unwrap_err()
        .contains("mixed memory embedding space"));
    let fixture = Fixture::new(&[row(0, vec![1.0])]);
    sidecar::build_sidecar(&fixture.source, &fixture.binary).unwrap();
    for query in [vec![], vec![f64::NAN], vec![f64::INFINITY]] {
        assert!(sidecar::query_sidecar(&fixture.binary, &query, 1).is_err());
    }
    for count in [0, 4097, usize::MAX] {
        assert!(sidecar::query_sidecar(&fixture.binary, &[1.0], count).is_err());
    }
    let reader = sidecar::Reader::open(&fixture.source, &fixture.binary).unwrap();
    assert!(reader.query(&[1.0], 1, "other-space").is_err());
    let huge = Fixture::new(&[row(0, vec![f64::MAX])]);
    assert!(sidecar::build_sidecar(&huge.source, &huge.binary).is_err());
}
