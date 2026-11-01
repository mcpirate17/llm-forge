use super::*;
use std::fs;
use std::os::unix::{
    ffi::OsStrExt,
    fs::{symlink, MetadataExt as StdMetadataExt},
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Barrier,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-cache-storage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn cache(&self) -> PathBuf {
        self.0.join(".forge/context-cache")
    }
    fn prepare(&self) {
        fs::create_dir_all(self.cache()).unwrap();
    }
    fn temps(&self) -> Vec<PathBuf> {
        fs::read_dir(self.cache())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|v| v == "tmp"))
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn hex(n: u32) -> String {
    format!("{n:064x}")
}

#[test]
fn absent_reads_never_create_cache_directories_and_invalid_keys_do_not_write() {
    let fixture = Fixture::new();
    assert_eq!(read(&fixture.0, &hex(1), MAX_BYTES).unwrap(), None);
    assert!(!fixture.0.join(".forge").exists());
    for invalid in [
        "../outside",
        &"A".repeat(64),
        &"0".repeat(63),
        &"g".repeat(64),
    ] {
        assert!(publish(&fixture.0, invalid, b"{}", 128).is_err());
        assert!(read(&fixture.0, invalid, MAX_BYTES).is_err());
    }
    assert!(!fixture.0.join(".forge").exists());
    assert!(read(&fixture.0, &hex(1), MAX_BYTES + 1).is_err());
}

#[test]
fn publication_roundtrips_private_bytes_without_touching_sources() {
    let fixture = Fixture::new();
    let source = fixture.0.join("source.rs");
    fs::write(&source, b"source bytes").unwrap();
    publish(&fixture.0, &hex(1), b"{\"projection\":1}", 128).unwrap();
    assert_eq!(
        read(&fixture.0, &hex(1), MAX_BYTES).unwrap().unwrap(),
        b"{\"projection\":1}"
    );
    assert_eq!(fs::read(source).unwrap(), b"source bytes");
    assert_eq!(
        fs::metadata(fixture.0.join(".forge")).unwrap().mode() & 0o777,
        0o700
    );
    assert_eq!(fs::metadata(fixture.cache()).unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        fs::metadata(fixture.cache().join(format!("{}.json", hex(1))))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    assert!(fixture.temps().is_empty());
}

#[test]
fn outside_symlinked_parent_and_cache_directory_never_receive_writes() {
    for forge_link in [true, false] {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        if forge_link {
            symlink(&outside.0, fixture.0.join(".forge")).unwrap();
        } else {
            fs::create_dir(fixture.0.join(".forge")).unwrap();
            symlink(&outside.0, fixture.cache()).unwrap();
        }
        assert!(read(&fixture.0, &hex(1), MAX_BYTES).is_err());
        assert!(publish(&fixture.0, &hex(1), b"{}", 128).is_err());
        assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 0);
    }
}

#[test]
fn symlink_fifo_and_hardlinked_targets_are_neither_read_nor_replaced() {
    let fixture = Fixture::new();
    fixture.prepare();
    let outside = fixture.0.join("source.rs");
    fs::write(&outside, b"do not replace").unwrap();
    let target = fixture.cache().join(format!("{}.json", hex(1)));
    symlink(&outside, &target).unwrap();
    assert!(read(&fixture.0, &hex(1), MAX_BYTES).is_err());
    assert!(publish(&fixture.0, &hex(1), b"{}", 128).is_err());
    fs::remove_file(&target).unwrap();
    let name = std::ffi::CString::new(target.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(read(&fixture.0, &hex(1), MAX_BYTES).is_err());
    assert!(publish(&fixture.0, &hex(1), b"{}", 128).is_err());
    fs::remove_file(&target).unwrap();
    fs::hard_link(&outside, &target).unwrap();
    assert!(read(&fixture.0, &hex(1), MAX_BYTES).is_err());
    assert!(publish(&fixture.0, &hex(1), b"{}", 128).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"do not replace");
}

#[test]
fn reads_and_writes_enforce_fixed_payload_bounds() {
    let fixture = Fixture::new();
    fixture.prepare();
    fs::write(
        fixture.cache().join(format!("{}.json", hex(1))),
        vec![b'x'; MAX_BYTES + 1],
    )
    .unwrap();
    assert!(read(&fixture.0, &hex(1), MAX_BYTES)
        .unwrap_err()
        .to_string()
        .contains("byte limit"));
    assert!(publish(&fixture.0, &hex(2), &vec![b'x'; MAX_BYTES + 1], 128).is_err());
    assert!(!fixture.cache().join(format!("{}.json", hex(2))).exists());
    publish(&fixture.0, &hex(2), &vec![b'x'; MAX_BYTES], 128).unwrap();
    assert_eq!(
        read(&fixture.0, &hex(2), MAX_BYTES).unwrap().unwrap().len(),
        MAX_BYTES
    );
}

#[test]
fn atomic_replacement_during_read_is_refused() {
    let fixture = Fixture::new();
    publish(&fixture.0, &hex(1), b"old", 128).unwrap();
    let target = fixture.cache().join(format!("{}.json", hex(1)));
    let replacement = fixture.0.join("replacement");
    fs::write(&replacement, b"new").unwrap();
    let result = read_checked(&fixture.0, &hex(1), MAX_BYTES, |at, _| {
        if at == Boundary::Read {
            fs::rename(&replacement, &target)?;
        }
        Ok(())
    });
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("changed during read"));
}

#[test]
fn injected_failure_cleans_only_owned_stage_and_preserves_existing_target() {
    let fixture = Fixture::new();
    publish(&fixture.0, &hex(1), b"old", 128).unwrap();
    let unrelated = fixture.cache().join("unrelated.tmp");
    fs::write(&unrelated, b"preserve").unwrap();
    let result = publish_checked(&fixture.0, &hex(1), b"new", 128, MAX_SCAN, |at, _| {
        if at == Boundary::Staged {
            anyhow::bail!("injected publication failure");
        }
        Ok(())
    });
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("injected publication failure"));
    assert_eq!(
        read(&fixture.0, &hex(1), MAX_BYTES).unwrap().unwrap(),
        b"old"
    );
    assert_eq!(fixture.temps(), vec![unrelated]);
}

#[test]
fn stage_replacement_is_not_unlinked_and_cleanup_error_is_preserved() {
    let fixture = Fixture::new();
    let outside = fixture.0.join("source.rs");
    fs::write(&outside, b"source").unwrap();
    let mut changed = None;
    let result = publish_checked(&fixture.0, &hex(1), b"new", 128, MAX_SCAN, |at, parent| {
        if at == Boundary::Staged {
            let stage = fs::read_dir(parent)?
                .map(|v| v.unwrap().path())
                .find(|v| v.extension().is_some_and(|ext| ext == "tmp"))
                .unwrap();
            fs::rename(&stage, parent.join("retained-original-stage"))?;
            symlink(&outside, &stage)?;
            changed = Some(stage);
        }
        Ok(())
    });
    let error = result.unwrap_err().to_string();
    assert!(error.contains("cleanup failed"), "{error}");
    assert!(fs::symlink_metadata(changed.unwrap())
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(outside).unwrap(), b"source");
}

#[test]
fn parent_replacement_after_staging_refuses_publication_and_cleans_held_stage() {
    let fixture = Fixture::new();
    let moved = fixture.0.join("moved-cache");
    let result = publish_checked(&fixture.0, &hex(1), b"new", 128, MAX_SCAN, |at, parent| {
        if at == Boundary::Staged {
            fs::rename(parent, &moved)?;
            fs::create_dir(parent)?;
        }
        Ok(())
    });
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("directory changed"));
    assert!(!fixture.cache().join(format!("{}.json", hex(1))).exists());
    assert_eq!(
        fs::read_dir(&moved)
            .unwrap()
            .filter(|v| v
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "tmp"))
            .count(),
        0
    );
}

#[test]
fn safe_pruning_retains_only_bounded_managed_entries_and_preserves_other_files() {
    let fixture = Fixture::new();
    fixture.prepare();
    for n in 0..4 {
        fs::write(fixture.cache().join(format!("{}.json", hex(n))), b"cache").unwrap();
    }
    let unrelated = fixture.cache().join("notes.json");
    fs::write(&unrelated, b"not cache").unwrap();
    let upper = fixture.cache().join(format!("{}.json", "A".repeat(64)));
    fs::write(&upper, b"upper").unwrap();
    let outside = fixture.0.join("source.rs");
    fs::write(&outside, b"source").unwrap();
    let alias = fixture.cache().join(format!("{}.json", hex(90)));
    symlink(&outside, &alias).unwrap();
    publish(&fixture.0, &hex(10), b"new", 2).unwrap();
    let managed = fs::read_dir(fixture.cache())
        .unwrap()
        .filter_map(|v| {
            let path = v.unwrap().path();
            let name = path.file_name()?.to_str()?.to_owned();
            let stem = name.strip_suffix(".json")?;
            (key(stem).is_ok() && fs::symlink_metadata(&path).unwrap().is_file()).then_some(path)
        })
        .count();
    assert_eq!(managed, 2);
    assert_eq!(fs::read(unrelated).unwrap(), b"not cache");
    assert_eq!(fs::read(upper).unwrap(), b"upper");
    assert!(fs::symlink_metadata(alias)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read(outside).unwrap(), b"source");
}

#[test]
fn bounded_scan_failure_does_not_prune_or_leave_stages() {
    let fixture = Fixture::new();
    publish(&fixture.0, &hex(1), b"old", 128).unwrap();
    fs::write(fixture.cache().join("unrelated-a"), b"a").unwrap();
    fs::write(fixture.cache().join("unrelated-b"), b"b").unwrap();
    let result = publish_checked(&fixture.0, &hex(2), b"new", 2, 3, |_, _| Ok(()));
    assert!(result.unwrap_err().to_string().contains("scan limit"));
    assert_eq!(
        read(&fixture.0, &hex(1), MAX_BYTES).unwrap().unwrap(),
        b"old"
    );
    assert!(fixture.temps().is_empty());
    assert!(publish(&fixture.0, &hex(2), b"new", 129).is_err());
}

#[test]
fn concurrent_writers_publish_complete_values_and_leave_no_owned_temporary_files() {
    let fixture = Fixture::new();
    let barrier = Arc::new(Barrier::new(3));
    let a = vec![b'a'; 32 * 1024];
    let b = vec![b'b'; 32 * 1024];
    let workers: Vec<_> = [a.clone(), b.clone()]
        .into_iter()
        .map(|bytes| {
            let host = fixture.0.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                publish(&host, &hex(1), &bytes, 128)
            })
        })
        .collect();
    barrier.wait();
    for worker in workers {
        worker.join().unwrap().unwrap();
    }
    let value = read(&fixture.0, &hex(1), MAX_BYTES).unwrap().unwrap();
    assert!(value == a || value == b);
    assert!(fixture.temps().is_empty());
}

#[test]
fn growth_after_admission_hits_read_sentinel_without_unbounded_allocation() {
    let fixture = Fixture::new();
    publish(&fixture.0, &hex(1), b"1234", 128).unwrap();
    let path = fixture.cache().join(format!("{}.json", hex(1)));
    let result = read_checked(&fixture.0, &hex(1), 4, |at, _| {
        if at == Boundary::BeforeRead {
            fs::write(&path, b"12345")?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("byte limit"));
}

#[test]
fn host_replacement_after_staging_cannot_redirect_publication() {
    let fixture = Fixture::new();
    let host = fixture.0.join("host");
    let replacement = fixture.0.join("replacement");
    let moved = fixture.0.join("moved");
    fs::create_dir(&host).unwrap();
    fs::create_dir(&replacement).unwrap();
    let result = publish_checked(&host, &hex(1), b"new", 128, MAX_SCAN, |at, _| {
        if at == Boundary::Staged {
            fs::rename(&host, &moved)?;
            fs::rename(&replacement, &host)?;
        }
        Ok(())
    });
    assert!(result.unwrap_err().to_string().contains("host changed"));
    assert!(!host.join(".forge").exists());
    let directory = moved.join(".forge/context-cache");
    assert_eq!(
        fs::read_dir(directory)
            .unwrap()
            .filter(|entry| entry
                .as_ref()
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "tmp"))
            .count(),
        0
    );
}

#[test]
fn unsafe_lock_path_never_redirects_or_truncates_other_files() {
    let fixture = Fixture::new();
    fixture.prepare();
    let source = fixture.0.join("source.rs");
    fs::write(&source, b"source").unwrap();
    symlink(&source, fixture.cache().join(LOCK)).unwrap();
    assert!(publish(&fixture.0, &hex(1), b"new", 128).is_err());
    assert_eq!(fs::read(source).unwrap(), b"source");
    assert!(fixture.temps().is_empty());
}

#[test]
fn in_place_stage_mutation_is_refused_and_owned_inode_is_cleaned() {
    let fixture = Fixture::new();
    let result = publish_checked(
        &fixture.0,
        &hex(1),
        b"original",
        128,
        MAX_SCAN,
        |at, parent| {
            if at == Boundary::Staged {
                let stage = fs::read_dir(parent)?
                    .map(|v| v.unwrap().path())
                    .find(|v| v.extension().is_some_and(|ext| ext == "tmp"))
                    .unwrap();
                fs::write(stage, b"modified stage")?;
            }
            Ok(())
        },
    );
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("stage path changed"));
    assert!(fixture.temps().is_empty());
    assert_eq!(read(&fixture.0, &hex(1), MAX_BYTES).unwrap(), None);
}

#[test]
fn final_failure_before_rename_retains_all_previous_cache_entries() {
    let fixture = Fixture::new();
    publish(&fixture.0, &hex(1), b"one", 128).unwrap();
    publish(&fixture.0, &hex(2), b"two", 128).unwrap();
    let result = publish_checked(&fixture.0, &hex(3), b"new", 1, MAX_SCAN, |at, _| {
        if at == Boundary::BeforeRename {
            anyhow::bail!("injected final prepublication failure");
        }
        Ok(())
    });
    let error = result.unwrap_err().to_string();
    assert!(error.contains("prepublication failure"));
    assert!(!error.contains("published but"));
    assert_eq!(
        read(&fixture.0, &hex(1), MAX_BYTES).unwrap().unwrap(),
        b"one"
    );
    assert_eq!(
        read(&fixture.0, &hex(2), MAX_BYTES).unwrap().unwrap(),
        b"two"
    );
    assert_eq!(read(&fixture.0, &hex(3), MAX_BYTES).unwrap(), None);
    assert!(fixture.temps().is_empty());
}

#[test]
fn partial_prune_failure_explicitly_reports_visible_publication_without_stage() {
    let fixture = Fixture::new();
    for n in 1..=3 {
        publish(&fixture.0, &hex(n), b"old", 128).unwrap();
    }
    let mut removed = None;
    let result = publish_checked(
        &fixture.0,
        &hex(4),
        b"published",
        1,
        MAX_SCAN,
        |at, parent| {
            if at == Boundary::BeforePrune {
                let storage = Storage::open(&fixture.0, false)?.unwrap();
                let candidates = storage.scan(&format!("{}.json", hex(4)), MAX_SCAN)?;
                removed = Some(parent.join(&candidates[0].name));
                fs::write(parent.join(&candidates[1].name), b"changed candidate")?;
            }
            Ok(())
        },
    );
    let error = result.unwrap_err();
    let diagnostic = format!("{error:#}");
    assert!(
        diagnostic.contains("published but cleanup/prune/sync incomplete"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("pruning incomplete"), "{diagnostic}");
    assert!(
        diagnostic.contains("may already have been removed"),
        "{diagnostic}"
    );
    assert!(!removed.unwrap().exists());
    assert_eq!(
        read(&fixture.0, &hex(4), MAX_BYTES).unwrap().unwrap(),
        b"published"
    );
    assert!(fixture.temps().is_empty());
}
