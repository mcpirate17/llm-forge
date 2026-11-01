#![cfg(feature = "vector-search")]
use conductor_native::memory_index_sidecar::{self as snapshot, Reader};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{symlink, FileExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::Arc;

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    mapped: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .join(conductor_native::native_id::new("memory-snapshot-test").unwrap());
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Self {
            source: root.join("memory.jsonl"),
            mapped: root.join("memory.sidecar"),
            root,
        };
        let rows:Vec<_>=(0..200).map(|i| json!({"schema_version":3,
            "embedding":{"fingerprint":"sha256:test","dimension":2,"paid":true},
            "source_sha256":format!("{i:064x}"),"source":"notes","path":format!("notes/{i}.md"),
            "title":format!("row {i}"),"text":"body ".repeat(50),"vector":[1.0-i as f64/200.0,i as f64/200.0]}).to_string()).collect();
        fs::write(&fixture.source, rows.join("\n")).unwrap();
        fixture
    }
    fn build(&self) {
        assert_eq!(
            snapshot::build_sidecar(&self.source, &self.mapped).unwrap(),
            200
        );
    }
    fn reader(&self) -> Reader {
        Reader::open(&self.source, &self.mapped).unwrap()
    }
    fn replace(&self, bytes: &[u8]) {
        let path = self.root.join("replacement");
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
        fs::rename(path, &self.mapped).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn mapped_queries_bind_source_space_and_match_f32_reference_with_stable_ties() {
    let f = Fixture::new();
    f.build();
    let reader = f.reader();
    reader.verify_source_hash().unwrap();
    let metadata = reader.metadata();
    assert_eq!(metadata.rows, 200);
    assert_eq!(metadata.dimension, 2);
    assert!(snapshot::Reader::open_limited(&f.source, &f.mapped, 199, 1024 * 1024).is_err());
    assert!(snapshot::Reader::open_limited(
        &f.source,
        &f.mapped,
        200,
        metadata.mapped_bytes as u64 - 1
    )
    .is_err());
    assert!(snapshot::Reader::open_limited(
        &f.source,
        &f.mapped,
        200,
        metadata.mapped_bytes as u64
    )
    .is_ok());
    assert_eq!(
        metadata.source_sha256,
        <[u8; 32]>::from(Sha256::digest(fs::read(&f.source).unwrap()))
    );
    let hits = reader.query(&[1.0, 0.0], 3, "sha256:test").unwrap();
    assert_eq!(
        hits.iter().map(|hit| hit.2.as_str()).collect::<Vec<_>>(),
        ["notes/0.md", "notes/1.md", "notes/2.md"]
    );
    assert_eq!(hits[1].0, 0.995_f32 as f64);
    assert!(reader.query(&[1.0], 3, "sha256:test").is_err());
    assert!(reader.query(&[1.0, 0.0], 3, "sha256:other").is_err());
    for count in [0, 65, usize::MAX] {
        assert!(reader.query(&[1.0, 0.0], count, "sha256:test").is_err());
    }
    assert!(reader.query(&[f64::NAN, 0.0], 3, "sha256:test").is_err());
    assert_eq!(
        fs::metadata(&f.mapped).unwrap().permissions().mode() & 0o777,
        0o400
    );
}

#[test]
fn changed_source_after_open_and_same_length_same_mtime_changes_are_stale() {
    let f = Fixture::new();
    f.build();
    let reader = f.reader();
    let file = fs::OpenOptions::new().write(true).open(&f.source).unwrap();
    let modified = file.metadata().unwrap().modified().unwrap();
    file.write_all_at(b" ", 1).unwrap();
    file.set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert!(!snapshot::sidecar_is_fresh(&f.source, &f.mapped).unwrap());
    assert!(reader
        .query(&[1.0, 0.0], 3, "sha256:test")
        .unwrap_err()
        .to_string()
        .contains("stale"));
    assert!(reader.verify_source_hash().is_err());
}

#[test]
fn corruption_of_header_matrix_and_offsets_is_rejected_before_query() {
    let f = Fixture::new();
    f.build();
    let original = fs::read(&f.mapped).unwrap();
    for offset in [0, 16, 80, 112, 296, 383, 384, original.len() - 1] {
        let mut bytes = original.clone();
        bytes[offset] ^= 1;
        f.replace(&bytes);
        assert!(Reader::open(&f.source, &f.mapped).is_err(), "byte {offset}");
        assert!(!snapshot::sidecar_is_fresh(&f.source, &f.mapped).unwrap());
    }
    f.replace(&original[..100]);
    assert!(Reader::open(&f.source, &f.mapped).is_err());
}

#[test]
fn snapshot_replacement_preserves_existing_maps_and_concurrent_source_reads() {
    let f = Fixture::new();
    f.build();
    let reader = Arc::new(f.reader());
    let workers: Vec<_> = (0..8)
        .map(|i| {
            let reader = reader.clone();
            std::thread::spawn(move || {
                for _ in 0..30 {
                    let hits = reader
                        .query(
                            if i % 2 == 0 { &[1.0, 0.0] } else { &[0.0, 1.0] },
                            3,
                            "sha256:test",
                        )
                        .unwrap();
                    assert_eq!(
                        hits[0].2,
                        if i % 2 == 0 {
                            "notes/0.md"
                        } else {
                            "notes/199.md"
                        }
                    );
                }
            })
        })
        .collect();
    for _ in 0..3 {
        f.build();
    }
    for worker in workers {
        worker.join().unwrap();
    }
    reader.verify_source_hash().unwrap();
}

#[test]
fn unsafe_outputs_and_busy_writers_preserve_source_and_existing_snapshot() {
    let f = Fixture::new();
    f.build();
    let before = fs::read(&f.mapped).unwrap();
    let source = fs::read(&f.source).unwrap();
    assert!(snapshot::build_sidecar(&f.source, &f.source).is_err());
    assert_eq!(fs::read(&f.source).unwrap(), source);
    let target = f.root.join("target");
    fs::write(&target, b"preserve").unwrap();
    let link = f.root.join("symlink");
    symlink(&target, &link).unwrap();
    assert!(snapshot::build_sidecar(&f.source, &link).is_err());
    assert_eq!(fs::read(&target).unwrap(), b"preserve");
    let lock = File::open(f.root.join("memory.sidecar.lock")).unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert!(snapshot::build_sidecar(&f.source, &f.mapped)
        .unwrap_err()
        .contains("busy"));
    assert_eq!(fs::read(&f.mapped).unwrap(), before);
    assert!(!fs::read_dir(&f.root).unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with("memory-pending")));
}

#[test]
fn invalid_rebuild_leaves_prior_generation_and_rejects_hostile_file_types() {
    let f = Fixture::new();
    f.build();
    let before = fs::read(&f.mapped).unwrap();
    let mut append = fs::OpenOptions::new().append(true).open(&f.source).unwrap();
    append.write_all(b"\nnot-json").unwrap();
    assert!(snapshot::build_sidecar(&f.source, &f.mapped).is_err());
    assert_eq!(fs::read(&f.mapped).unwrap(), before);
    let link = f.root.join("alias");
    fs::hard_link(&f.mapped, &link).unwrap();
    assert!(Reader::open(&f.source, &f.mapped).is_err());
    fs::remove_file(link).unwrap();
    fs::set_permissions(&f.mapped, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(Reader::open(&f.source, &f.mapped).is_err());
    fs::remove_file(&f.mapped).unwrap();
    symlink(&f.source, &f.mapped).unwrap();
    assert!(Reader::open(&f.source, &f.mapped).is_err());
}

#[test]
fn all_preview_fields_are_validated_at_build_even_for_low_ranked_rows() {
    let f = Fixture::new();
    let text = fs::read_to_string(&f.source).unwrap();
    let mut rows: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    rows.last_mut().unwrap()["title"] = json!(5);
    fs::write(
        &f.source,
        rows.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    assert!(snapshot::build_sidecar(&f.source, &f.mapped)
        .unwrap_err()
        .contains("title"));
    assert!(!f.mapped.exists());
}

#[test]
fn atomic_source_replacement_cannot_attach_old_scores_to_new_documents() {
    let f = Fixture::new();
    f.build();
    let old = f.reader();
    let text = fs::read_to_string(&f.source).unwrap();
    let mut changed = text.lines().rev().collect::<Vec<_>>().join("\n");
    if text.ends_with('\n') {
        changed.push('\n');
    }
    assert_eq!(changed.len(), text.len());
    let replacement = f.root.join("new-source");
    fs::write(&replacement, changed).unwrap();
    fs::rename(replacement, &f.source).unwrap();
    assert!(snapshot::Reader::open(&f.source, &f.mapped).is_err());
    // A held old source can only yield its original bytes or an explicit stale
    // error; it must never reopen the replacement path to retrieve previews.
    match old.query(&[1.0, 0.0], 1, "sha256:test") {
        Ok(hits) => assert_eq!(hits[0].2, "notes/0.md"),
        Err(error) => assert!(error.to_string().contains("stale")),
    }
    f.build();
    assert_eq!(
        f.reader().query(&[1.0, 0.0], 1, "sha256:test").unwrap()[0].2,
        "notes/0.md"
    );
}
