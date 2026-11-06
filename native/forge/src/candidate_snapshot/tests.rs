use super::*;
use git2::{Oid, Repository};
use std::os::unix::fs::PermissionsExt as _;

struct Fixture {
    root: PathBuf,
    repository: Repository,
}

#[test]
fn explicit_discard_failure_does_not_retry_destructive_drop() {
    let fixture = Fixture::new();
    let tree = fixture.tree(&[("sentinel", b"keep", 0o100644)]);
    let snapshot = Snapshot::create(
        &fixture.source(),
        &tree,
        &fixture.output(),
        Limits::default(),
    )
    .unwrap();
    let path = snapshot.path();
    let result =
        snapshot.discard_using(|_, _| Err(std::io::Error::other("injected discard failure")));
    assert!(result.is_err());
    assert_eq!(std::fs::read(path.join("sentinel")).unwrap(), b"keep");
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(crate::native_id::new("forge-snapshot-test").unwrap());
        std::fs::create_dir(&root).unwrap();
        let repository = Repository::init(root.join("repo")).unwrap();
        Self { root, repository }
    }
    fn source(&self) -> Source {
        Source::open(&self.root.join("repo")).unwrap()
    }
    fn output(&self) -> PathBuf {
        self.root.join("output")
    }
    fn tree(&self, files: &[(&str, &[u8], i32)]) -> String {
        let objects: Vec<_> = files
            .iter()
            .map(|(name, data, mode)| (*name, self.repository.blob(data).unwrap(), *mode))
            .collect();
        self.tree_objects(&objects).to_string()
    }
    fn tree_objects(&self, entries: &[(&str, Oid, i32)]) -> Oid {
        let mut tree = self.repository.treebuilder(None).unwrap();
        let mut dirs: BTreeMap<&str, Vec<_>> = BTreeMap::new();
        for &(name, oid, mode) in entries {
            if let Some((directory, rest)) = name.split_once('/') {
                dirs.entry(directory).or_default().push((rest, oid, mode));
            } else {
                tree.insert(name, oid, mode).unwrap();
            }
        }
        for (name, children) in dirs {
            tree.insert(name, self.tree_objects(&children), 0o040000)
                .unwrap();
        }
        tree.write().unwrap()
    }
    fn raw_tree(&self, name: &[u8], content: &[u8], mode: &str) -> String {
        let blob = self.repository.blob(content).unwrap();
        let mut bytes = format!("{mode} ").into_bytes();
        bytes.extend_from_slice(name);
        bytes.push(0);
        bytes.extend_from_slice(blob.as_bytes());
        self.repository
            .odb()
            .unwrap()
            .write(git2::ObjectType::Tree, &bytes)
            .unwrap()
            .to_string()
    }
    fn count_outputs(&self) -> usize {
        std::fs::read_dir(&self.root).unwrap().count() - 1
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn preserves_exact_bytes_modes_links_and_skips_gitlinks() {
    let fixture = Fixture::new();
    let text = fixture.repository.blob(b"#!/bin/sh\nexit 0\n").unwrap();
    let binary = fixture.repository.blob(&[0, 255, 13, 10]).unwrap();
    let link = fixture.repository.blob(b"../run").unwrap();
    let tree = fixture
        .tree_objects(&[
            ("run", text, 0o100755),
            ("data/bin", binary, 0o100644),
            ("data/link", link, 0o120000),
            (
                "external",
                Oid::from_str(&"1".repeat(40)).unwrap(),
                0o160000,
            ),
        ])
        .to_string();
    let source = fixture.source();
    let preview = preflight(&source, &tree, &fixture.output(), Limits::default()).unwrap();
    assert!(!preview.published);
    assert_eq!(fixture.count_outputs(), 0);
    let snapshot = Snapshot::create(&source, &tree, &fixture.output(), Limits::default()).unwrap();
    assert!(!fixture.output().exists());
    let staged = snapshot.path();
    assert_eq!(
        std::fs::metadata(&staged).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::read(staged.join("data/bin")).unwrap(),
        [0, 255, 13, 10]
    );
    assert_eq!(
        std::fs::read_link(staged.join("data/link")).unwrap(),
        Path::new("../run")
    );
    assert_eq!(
        std::fs::metadata(staged.join("run"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        std::fs::metadata(staged.join("data/bin"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    assert!(!staged.join(".git").exists() && !staged.join("external").exists());
    let report = snapshot.publish().unwrap();
    assert!(report.published && fixture.output().is_dir());
    assert_eq!(
        (
            report.entries,
            report.materialized_files,
            report.skipped_gitlinks
        ),
        (4, 3, 1)
    );
    assert_eq!(report.materialized_bytes, 27);
    assert!(!staged.exists());
}

#[test]
fn drop_cleans_private_snapshot_and_late_destination_race_never_overwrites() {
    let fixture = Fixture::new();
    let tree = fixture.tree(&[("data", b"exact", 0o100644)]);
    let snapshot = Snapshot::create(
        &fixture.source(),
        &tree,
        &fixture.output(),
        Limits::default(),
    )
    .unwrap();
    let staging = snapshot.path();
    drop(snapshot);
    assert!(!staging.exists() && fixture.count_outputs() == 0);
    let snapshot = Snapshot::create(
        &fixture.source(),
        &tree,
        &fixture.output(),
        Limits::default(),
    )
    .unwrap();
    let staging = snapshot.path();
    std::fs::write(fixture.output(), b"other writer").unwrap();
    assert!(snapshot.publish().is_err());
    assert_eq!(std::fs::read(fixture.output()).unwrap(), b"other writer");
    assert!(!staging.exists());
}

#[test]
fn refuses_existing_outputs_and_byte_limits_before_creating_staging() {
    let fixture = Fixture::new();
    let source = fixture.source();
    let tree = fixture.tree(&[("a", b"1234", 0o100644), ("b", b"5678", 0o100644)]);
    for limits in [
        Limits {
            max_blob_bytes: 3,
            max_tree_bytes: 100,
        },
        Limits {
            max_blob_bytes: 4,
            max_tree_bytes: 7,
        },
        Limits {
            max_blob_bytes: u64::MAX,
            max_tree_bytes: 0,
        },
    ] {
        assert!(Snapshot::create(&source, &tree, &fixture.output(), limits).is_err());
        assert_eq!(fixture.count_outputs(), 0);
    }
    std::fs::create_dir(fixture.output()).unwrap();
    assert!(preflight(&source, &tree, &fixture.output(), Limits::default()).is_err());
    std::fs::remove_dir(fixture.output()).unwrap();
    std::os::unix::fs::symlink("missing", fixture.output()).unwrap();
    assert!(Snapshot::create(&source, &tree, &fixture.output(), Limits::default()).is_err());
    assert_eq!(
        std::fs::read_link(fixture.output()).unwrap(),
        Path::new("missing")
    );
}

#[test]
fn rejects_unsafe_links_and_reserved_paths_without_side_effects() {
    let fixture = Fixture::new();
    let source = fixture.source();
    for target in [
        b"/etc/passwd".as_slice(),
        b"../escape",
        b"a",
        b"",
        b"bad\0target",
        b"\xff",
        b".git/config",
    ] {
        let tree = fixture.tree(&[("a", target, 0o120000)]);
        assert!(
            Snapshot::create(&source, &tree, &fixture.output(), Limits::default()).is_err(),
            "{target:?}"
        );
        assert_eq!(fixture.count_outputs(), 0);
    }
    for path in [b".git".as_slice(), b".GiT", b"\xff"] {
        let tree = fixture.raw_tree(path, b"data", "100644");
        assert!(preflight(&source, &tree, &fixture.output(), Limits::default()).is_err());
    }
    for tree in ["HEAD", "abcd", &"f".repeat(40)] {
        assert!(preflight(&source, tree, &fixture.output(), Limits::default()).is_err());
    }
    assert_eq!(fixture.count_outputs(), 0);
}

#[test]
fn resolves_symlink_chains_before_dotdot_and_independently_of_tree_order() {
    let fixture = Fixture::new();
    let source = fixture.source();
    // Lexically 'z/../outside' looks contained; z expands to the snapshot root first.
    let escape = fixture.tree(&[("a", b"z/../outside", 0o120000), ("z", b".", 0o120000)]);
    assert!(preflight(&source, &escape, &fixture.output(), Limits::default()).is_err());
    let traverse_file =
        fixture.tree(&[("a", b"file/../target", 0o120000), ("file", b"x", 0o100644)]);
    assert!(preflight(
        &source,
        &traverse_file,
        &fixture.output(),
        Limits::default()
    )
    .is_err());
    let looped = fixture.tree(&[("a", b"z", 0o120000), ("z", b"a", 0o120000)]);
    assert!(preflight(&source, &looped, &fixture.output(), Limits::default()).is_err());
    let valid = fixture.tree(&[
        ("a", b"nested/link", 0o120000),
        ("nested/link", b"../file", 0o120000),
        ("file", b"target", 0o100644),
        ("dangling", b"internal/missing", 0o120000),
    ]);
    let snapshot = Snapshot::create(&source, &valid, &fixture.output(), Limits::default()).unwrap();
    assert_eq!(std::fs::read(snapshot.path().join("a")).unwrap(), b"target");
}

#[test]
fn forty_link_chain_matches_linux_resolution_limit() {
    let fixture = Fixture::new();
    let names: Vec<_> = (0..41).map(|n| format!("link{n:02}")).collect();
    for count in [40, 41] {
        let mut entries: Vec<_> = (0..count)
            .map(|n| {
                (
                    names[n].as_str(),
                    if n + 1 == count {
                        b"file".as_slice()
                    } else {
                        names[n + 1].as_bytes()
                    },
                    0o120000,
                )
            })
            .collect();
        entries.push(("file", b"data", 0o100644));
        let tree = fixture.tree(&entries);
        let result = Snapshot::create(
            &fixture.source(),
            &tree,
            &fixture.output(),
            Limits::default(),
        );
        if count == 40 {
            let snapshot = result.unwrap();
            assert_eq!(
                std::fs::read(snapshot.path().join("link00")).unwrap(),
                b"data"
            );
        } else {
            assert!(result.is_err());
        }
        assert_eq!(fixture.count_outputs(), 0);
    }
}

#[test]
fn metadata_failure_removes_only_the_new_empty_staging_directory() {
    let fixture = Fixture::new();
    let parent = Dir::open_ambient_dir(&fixture.root, cap_std::ambient_authority()).unwrap();
    parent.create_dir("owned-staging").unwrap();
    let injected = std::io::Error::from_raw_os_error(libc::EIO);
    assert!(staging_identity(
        &parent,
        std::ffi::OsStr::new("owned-staging"),
        Err(injected)
    )
    .is_err());
    assert!(!fixture.root.join("owned-staging").exists());
    assert!(fixture.root.join("repo").is_dir());
}

#[test]
fn publishes_multiple_binary_blobs_with_exact_aggregate_limit() {
    let fixture = Fixture::new();
    let data: Vec<u8> = (0..1_048_576).map(|n| (n % 251) as u8).collect();
    let tree = fixture.tree(&[
        ("one", &data, 0o100644),
        ("two", &data, 0o100644),
        ("nested/three", &data, 0o100644),
    ]);
    let limits = Limits {
        max_blob_bytes: data.len() as u64,
        max_tree_bytes: 3 * data.len() as u64,
    };
    let report = Snapshot::create(&fixture.source(), &tree, &fixture.output(), limits)
        .unwrap()
        .publish()
        .unwrap();
    assert_eq!(report.materialized_bytes, 3 * data.len() as u64);
    for path in ["one", "two", "nested/three"] {
        assert_eq!(std::fs::read(fixture.output().join(path)).unwrap(), data);
    }
}

#[test]
fn renamed_parent_refuses_publication_and_cleans_through_held_directory() {
    let fixture = Fixture::new();
    let parent = fixture.root.join("parent");
    let moved = fixture.root.join("moved");
    std::fs::create_dir(&parent).unwrap();
    let tree = fixture.tree(&[("file", b"data", 0o100644)]);
    let snapshot = Snapshot::create(
        &fixture.source(),
        &tree,
        &parent.join("out"),
        Limits::default(),
    )
    .unwrap();
    std::fs::rename(&parent, &moved).unwrap();
    assert!(snapshot.publish().is_err());
    assert_eq!(std::fs::read_dir(moved).unwrap().count(), 0);
}

#[test]
fn concurrent_publishers_have_one_winner_without_staging_leaks() {
    let fixture = Fixture::new();
    let tree = fixture.tree(&[("file", b"immutable bytes", 0o100644)]);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
    let threads: Vec<_> = (0..16)
        .map(|_| {
            let repo = fixture.root.join("repo");
            let output = fixture.output();
            let tree = tree.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let source = Source::open(&repo).unwrap();
                barrier.wait();
                Snapshot::create(&source, &tree, &output, Limits::default())
                    .and_then(Snapshot::publish)
            })
        })
        .collect();
    let winners = threads
        .into_iter()
        .filter_map(|thread| thread.join().unwrap().ok())
        .count();
    assert_eq!(winners, 1);
    assert_eq!(fixture.count_outputs(), 1);
    assert_eq!(
        std::fs::read(fixture.output().join("file")).unwrap(),
        b"immutable bytes"
    );
}

#[path = "verify_tests.rs"]
mod verify_tests;

#[path = "directory_sync_tests.rs"]
mod directory_sync_tests;
