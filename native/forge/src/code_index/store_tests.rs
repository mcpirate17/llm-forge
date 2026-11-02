use super::*;
use crate::code_index::{build, BuildHeader, EmbeddingRow};
use conductor_native::vector_index::{Config, Space};
use std::cell::Cell;

fn fixture() -> (tempfile::TempDir, Store, Manifest, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::create_dir(&source).unwrap();
    let store = Store::create(&dir.path().join("index")).unwrap();
    let prior = build(
        &store,
        &source,
        BuildHeader {
            version: 1,
            rows: 0,
            space: Space {
                dimension: 2,
                model_sha256: [1; 32],
            },
            config: Config::default(),
        },
        std::iter::empty::<anyhow::Result<EmbeddingRow>>(),
    )
    .unwrap();
    (dir, store, prior, source)
}
fn pending(store: &Store, prior: &Manifest) -> (Staging, Manifest) {
    let staging = store.begin().unwrap();
    let mut manifest = prior.clone();
    manifest.generation = staging.id.clone();
    (staging, manifest)
}
fn no_temporary_pointer(store: &Store) {
    assert!(fs::read_dir(store.root()).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("pointer-")
    }));
}
#[test]
fn real_publication_contention_fails_fast_without_advancing_current() {
    let (_dir, store, prior, _) = fixture();
    let (staging, manifest) = pending(&store, &prior);
    let path = staging.path.clone();
    let _held = lock(&store.root().join("publication.lock"), true).unwrap();
    let called = Cell::new(false);
    let result = staging.publish_checked(&manifest, &mut || Ok(()), &mut || {
        called.set(true);
        Ok(())
    });
    assert!(result.is_err());
    assert!(!called.get());
    assert_eq!(store.current().unwrap(), prior.generation);
    assert!(!path.exists());
    no_temporary_pointer(&store);
}
#[test]
fn final_source_drift_refuses_after_pointer_sync_and_removes_pointer() {
    let (_dir, store, prior, source) = fixture();
    let file = source.join("empty.txt");
    fs::write(&file, "").unwrap();
    let (staging, manifest) = pending(&store, &prior);
    let path = staging.path.clone();
    let mut checkpoint = || {
        // This callback is inside publication.lock and follows pointer sync.
        assert!(try_lock(&store.root().join("publication.lock"), true)?.is_none());
        assert!(fs::read_dir(store.root())?.any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("pointer-")));
        fs::write(&file, "changed")?;
        Ok(())
    };
    let mut final_check = || {
        ensure!(
            fs::read(&file)?.is_empty(),
            "source changed before publication"
        );
        Ok(())
    };
    assert!(staging
        .publish_checked(&manifest, &mut checkpoint, &mut final_check)
        .is_err());
    assert_eq!(store.current().unwrap(), prior.generation);
    assert!(!path.exists());
    no_temporary_pointer(&store);
}
#[test]
fn deadline_expiring_during_final_check_refuses_before_pointer_rename() {
    let (_dir, store, prior, _) = fixture();
    let (staging, manifest) = pending(&store, &prior);
    let path = staging.path.clone();
    let expired = Cell::new(false);
    let mut checkpoint = || {
        ensure!(!expired.get(), "source refresh deadline expired");
        Ok(())
    };
    let mut final_check = || {
        expired.set(true);
        Ok(())
    };
    assert!(staging
        .publish_checked(&manifest, &mut checkpoint, &mut final_check)
        .is_err());
    assert_eq!(store.current().unwrap(), prior.generation);
    assert!(!path.exists());
    no_temporary_pointer(&store);
}
