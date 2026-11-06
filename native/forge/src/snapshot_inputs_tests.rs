use super::*;
use std::{fs, os::unix::fs::PermissionsExt};

struct Tree {
    source: Source,
    snapshot: Snapshot,
    root: tempfile::TempDir,
}
impl Tree {
    fn new(files: &[(&str, &[u8])]) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let repository = git2::Repository::init(root.path().join("repo")).unwrap();
        for (name, bytes) in files {
            let path = repository.workdir().unwrap().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
            let mut index = repository.index().unwrap();
            index.add_path(Path::new(name)).unwrap();
            index.write().unwrap();
        }
        let tree = repository
            .find_tree(repository.index().unwrap().write_tree().unwrap())
            .unwrap();
        let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
        let commit = repository
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "authored inputs",
                &tree,
                &[],
            )
            .unwrap();
        let source = Source::open(repository.workdir().unwrap()).unwrap();
        let oid = source.commit_tree(&commit.to_string()).unwrap();
        let snapshot = Snapshot::create(
            &source,
            &oid,
            &root.path().join("snapshot"),
            crate::candidate_snapshot::Limits::default(),
        )
        .unwrap();
        Self {
            source,
            snapshot,
            root,
        }
    }
    fn inputs(&self) -> SnapshotInputs<'_> {
        SnapshotInputs::new(
            &self.source,
            &self.snapshot,
            &self.snapshot.report().tree_oid,
        )
        .unwrap()
    }
    fn registry(&self, bytes: &[u8]) -> (PathBuf, String) {
        use sha2::{Digest, Sha256};
        let path = self.root.path().join("external.tsv");
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        (path, format!("{:x}", Sha256::digest(bytes)))
    }
}

#[test]
fn held_blob_and_absence_checks_bind_snapshot_not_dirty_host() {
    let tree = Tree::new(&[("pkg/m.py", b"value=1\n")]);
    fs::write(tree.source.root().join("pkg/m.py"), b"dirty host\n").unwrap();
    let mut inputs = tree.inputs();
    assert_eq!(inputs.read("pkg/m.py").unwrap(), "value=1\n");
    assert!(inputs.absent("pkg/m.py").is_err());
    inputs.absent("missing.toml").unwrap();
    inputs.verify().unwrap();
    fs::write(tree.snapshot.path().join("missing.toml"), b"appeared\n").unwrap();
    assert!(inputs.verify().is_err());
}

#[test]
fn cached_limits_and_held_blob_drift_fail_closed() {
    let tree = Tree::new(&[("input.txt", b"captured bytes\n")]);
    let mut inputs = tree.inputs();
    assert_eq!(inputs.read("input.txt").unwrap(), "captured bytes\n");
    assert!(inputs.read_with_limit("input.txt", 4).is_err());
    assert!(inputs.read_with_limit("input.txt", 0).is_err());
    assert!(inputs
        .read_with_limit("input.txt", 2 * 1024 * 1024 + 1)
        .is_err());
    fs::write(tree.snapshot.path().join("input.txt"), b"replaced bytes\n").unwrap();
    assert!(inputs.verify().is_err());
}

#[test]
fn external_registry_uses_shared_budget_and_retains_drift_binding() {
    let data = vec![b'x'; 2 * 1024 * 1024];
    let names: Vec<_> = (0..16).map(|i| format!("data/{i}.txt")).collect();
    let files: Vec<_> = names
        .iter()
        .map(|name| (name.as_str(), data.as_slice()))
        .collect();
    let tree = Tree::new(&files);
    let (path, digest) = tree.registry(&vec![b'r'; 1024 * 1024]);
    let mut inputs = tree.inputs();
    for name in &names[..15] {
        inputs.read_bytes_with_limit(name, data.len()).unwrap();
    }
    inputs.external_registry(&path, &digest).unwrap();
    assert!(inputs
        .read_bytes_with_limit(&names[15], data.len())
        .is_err());
    inputs.verify().unwrap();
    fs::write(&path, b"external drift\n").unwrap();
    assert!(inputs.verify().is_err());
}

#[test]
fn nonregular_blobs_invalid_text_and_wrong_context_are_refused() {
    let tree = Tree::new(&[
        ("valid.py", b"value=1\n"),
        ("binary", &[0xff]),
        ("nul", b"a\0b"),
    ]);
    let mut inputs = tree.inputs();
    assert!(inputs.read("binary").is_err());
    assert!(inputs.read("nul").is_err());
    assert!(inputs.read("absent").is_err());
    let path = tree.snapshot.path().join("valid.py");
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("binary", &path).unwrap();
    assert!(inputs.read("valid.py").is_err());
    let other = Tree::new(&[("valid.py", b"value=1\n")]);
    assert!(inputs
        .check_context(&other.source, &other.snapshot)
        .is_err());
    assert!(SnapshotInputs::new(&tree.source, &tree.snapshot, &"0".repeat(40)).is_err());
}
