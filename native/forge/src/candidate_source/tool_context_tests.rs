use super::*;
use git2::{Index, Signature, Time};
use std::os::unix::fs::{symlink, PermissionsExt};

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    git: PathBuf,
    index: PathBuf,
    tree: String,
    base: String,
}

impl Fixture {
    fn new(symbolic: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("candidate");
        let runtime = tmp.path().join("runtime");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&runtime).unwrap();
        let git = runtime.join("git");
        let repo = Repository::init_bare(&git).unwrap();
        let blob = repo.blob(b"snapshot\n").unwrap();
        let mut builder = repo.treebuilder(None).unwrap();
        builder.insert("source.c", blob, 0o100644).unwrap();
        let tree = builder.write().unwrap();
        let tree_obj = repo.find_tree(tree).unwrap();
        let sig = Signature::new("Fixture", "fixture@example.test", &Time::new(0, 0)).unwrap();
        let commit = repo
            .commit(None, &sig, &sig, "fixture", &tree_obj, &[])
            .unwrap();
        if symbolic {
            repo.reference("refs/heads/candidate", commit, true, "fixture")
                .unwrap();
            repo.set_head("refs/heads/candidate").unwrap();
        } else {
            repo.set_head_detached(commit).unwrap();
        }
        let index = runtime.join("index");
        let mut staged = Index::open(&index).unwrap();
        staged.read_tree(&tree_obj).unwrap();
        staged.write().unwrap();
        for path in [&root, &runtime, &git] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        for path in [&index, &git.join("HEAD")] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        Self {
            _tmp: tmp,
            root,
            git,
            index,
            tree: tree.to_string(),
            base: EMPTY_TREE.into(),
        }
    }

    fn open(&self) -> Result<(Source, Candidate, ToolBinding)> {
        Source::tool_context_paths(
            &self.root,
            &self.base,
            &self.tree,
            Some(self.git.clone()),
            Some(self.root.clone()),
            Some(self.index.clone()),
        )
    }
}

#[test]
fn detached_and_symbolic_private_heads_are_bound() {
    for symbolic in [false, true] {
        let f = Fixture::new(symbolic);
        let (source, candidate, binding) = f.open().unwrap();
        assert_eq!(candidate.kind, Kind::Index);
        assert_eq!(candidate.tree_oid, f.tree);
        assert_eq!(candidate.changes.len(), 1);
        binding.verify(&source).unwrap();
    }
}

#[test]
fn missing_variables_wrong_tree_and_invalid_base_refuse() {
    let mut f = Fixture::new(false);
    assert!(paths(&f.root, None, Some(f.root.clone()), Some(f.index.clone())).is_err());
    assert!(paths(&f.root, Some(f.git.clone()), None, Some(f.index.clone())).is_err());
    assert!(paths(&f.root, Some(f.git.clone()), Some(f.root.clone()), None).is_err());
    f.base = "HEAD".into();
    assert!(f.open().is_err());
    f.base = EMPTY_TREE.into();
    f.tree = EMPTY_TREE.into();
    assert!(f.open().is_err());
}

#[test]
fn full_commit_and_tree_bases_are_supported() {
    let mut f = Fixture::new(false);
    let repo = Repository::open(&f.git).unwrap();
    f.base = repo.head().unwrap().target().unwrap().to_string();
    let (_, candidate, _) = f.open().unwrap();
    assert_eq!(candidate.base_commit_oid.as_deref(), Some(f.base.as_str()));
    assert!(candidate.changes.is_empty());
    f.base = f.tree.clone();
    let (_, candidate, _) = f.open().unwrap();
    assert!(candidate.base_commit_oid.is_none());
    assert!(candidate.changes.is_empty());
}

#[test]
fn symlink_git_directory_refuses() {
    let f = Fixture::new(false);
    let alias = f.git.with_file_name("alias");
    symlink(&f.git, &alias).unwrap();
    assert!(Source::tool_context_paths(
        &f.root,
        &f.base,
        &f.tree,
        Some(alias),
        Some(f.root.clone()),
        Some(f.index.clone())
    )
    .is_err());
}

#[test]
fn index_tree_mismatch_and_index_drift_refuse() {
    let f = Fixture::new(false);
    let (source, _, binding) = f.open().unwrap();
    let repo = Repository::open(&f.git).unwrap();
    let mut index = Index::open(&f.index).unwrap();
    let empty = repo.treebuilder(None).unwrap().write().unwrap();
    index.read_tree(&repo.find_tree(empty).unwrap()).unwrap();
    index.write().unwrap();
    assert!(binding.verify(&source).is_err());
    assert!(f.open().is_err());
}

#[test]
fn symlink_index_and_snapshot_replacement_refuse() {
    let f = Fixture::new(false);
    let (source, _, binding) = f.open().unwrap();
    let real = f.index.with_file_name("real-index");
    fs::rename(&f.index, &real).unwrap();
    symlink(&real, &f.index).unwrap();
    assert!(binding.verify(&source).is_err());
    assert!(f.open().is_err());
    fs::remove_file(&f.index).unwrap();
    fs::rename(real, &f.index).unwrap();
    fs::rename(&f.root, f.root.with_file_name("old-candidate")).unwrap();
    fs::create_dir(&f.root).unwrap();
    assert!(binding.verify(&source).is_err());
}

#[test]
fn symbolic_head_reference_drift_refuses() {
    let f = Fixture::new(true);
    let (source, _, binding) = f.open().unwrap();
    let repo = Repository::open(&f.git).unwrap();
    let tree = repo.find_tree(oid(&f.tree).unwrap()).unwrap();
    let sig = Signature::new("Other", "other@example.test", &Time::new(0, 0)).unwrap();
    let changed = repo.commit(None, &sig, &sig, "other", &tree, &[]).unwrap();
    repo.reference("refs/heads/candidate", changed, true, "drift")
        .unwrap();
    assert!(binding.verify(&source).is_err());
}

#[test]
fn altered_index_mode_is_rejected() {
    let f = Fixture::new(false);
    let repo = Repository::open(&f.git).unwrap();
    let mut index = Index::open(&f.index).unwrap();
    let mut entry = index.get_path(Path::new("source.c"), 0).unwrap();
    entry.mode = 0o100755;
    index.add(&entry).unwrap();
    index.write().unwrap();
    drop(repo);
    assert!(f.open().is_err());
}
