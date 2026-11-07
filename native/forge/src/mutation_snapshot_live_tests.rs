use super::*;
use crate::mutation_snapshot::Snapshot;
use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
};

struct Fixture {
    root: tempfile::TempDir,
    repo: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let repo = root.path().join("repo");
        let repository = Repository::init(&repo).unwrap();
        fs::write(repo.join("tracked.py"), b"value = 1\n").unwrap();
        fs::write(repo.join(".gitignore"), b"ignored/\n*.bin\n").unwrap();
        let mut index = repository.index().unwrap();
        for p in ["tracked.py", ".gitignore"] {
            index.add_path(Path::new(p)).unwrap();
        }
        index.write().unwrap();
        let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = Signature::now("fixture", "fixture@example.invalid").unwrap();
        repository
            .commit(Some("HEAD"), &sig, &sig, "authored", &tree, &[])
            .unwrap();
        Self { root, repo }
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.repo.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    fn capture(&self, roots: &[&str]) -> (Snapshot, LiveBinding) {
        let mut snapshot = Snapshot::allocate(self.root.path()).unwrap();
        let roots: Vec<_> = roots.iter().map(|s| (*s).to_owned()).collect();
        let binding = snapshot.populate_live(&self.repo, &roots).unwrap();
        snapshot.verify_live(&self.repo, &binding).unwrap();
        (snapshot, binding)
    }
}
#[test]
fn live_includes_tracked_index_ignored_data_and_mandatory_config_without_host_objects() {
    let f = Fixture::new();
    f.write("tracked.py", b"dirty = 2\n");
    f.write("staged.bin", &[0, 255, 1]);
    let r = Repository::open(&f.repo).unwrap();
    let mut index = r.index().unwrap();
    index.add_path(Path::new("staged.bin")).unwrap();
    index.write().unwrap();
    f.write("ignored/data.bin", &[3, 4, 5]);
    f.write(
        "pyproject.toml",
        b"[tool.forge.equivalence_probe]\njobs=1\n",
    );
    f.write("omitted.bin", b"outside selection");
    let index_before = fs::read(f.repo.join(".git/index")).unwrap();
    let objects_before = object_names(&f.repo.join(".git/objects"));
    let (snapshot, binding) = f.capture(&["ignored"]);
    for p in [
        "tracked.py",
        "staged.bin",
        "ignored/data.bin",
        "pyproject.toml",
    ] {
        assert_eq!(
            fs::read(snapshot.worktree.join(p)).unwrap(),
            fs::read(f.repo.join(p)).unwrap()
        );
    }
    assert!(!snapshot.worktree.join("omitted.bin").exists());
    assert_eq!(index_before, fs::read(f.repo.join(".git/index")).unwrap());
    assert_eq!(objects_before, object_names(&f.repo.join(".git/objects")));
    assert_eq!(
        Source::open(&snapshot.worktree)
            .unwrap()
            .commit_tree(binding.commit())
            .unwrap(),
        binding.tree()
    );
    let root = snapshot.root.clone();
    snapshot.remove().unwrap();
    assert!(matches!(fs::symlink_metadata(root),Err(e) if e.kind()==std::io::ErrorKind::NotFound));
}
fn object_names(root: &Path) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for item in fs::read_dir(dir).unwrap() {
            let p = item.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                paths.insert(p);
            }
        }
    }
    paths
}
#[test]
fn live_host_bytes_presence_links_modes_index_and_controls_are_held() {
    for change in [
        "bytes", "added", "deleted", "mode", "link", "index", "ignore", "config",
    ] {
        let f = Fixture::new();
        f.write("data/leaf", b"one");
        let (snapshot, binding) = f.capture(&["data"]);
        match change {
            "bytes" => f.write("data/leaf", b"two"),
            "added" => f.write("data/new.bin", b"new"),
            "deleted" => fs::remove_file(f.repo.join("data/leaf")).unwrap(),
            "mode" => {
                fs::set_permissions(f.repo.join("data/leaf"), fs::Permissions::from_mode(0o755))
                    .unwrap()
            }
            "link" => {
                fs::remove_file(f.repo.join("data/leaf")).unwrap();
                symlink("../tracked.py", f.repo.join("data/leaf")).unwrap();
            }
            "index" => {
                let r = Repository::open(&f.repo).unwrap();
                let mut i = r.index().unwrap();
                i.remove_path(Path::new("tracked.py")).unwrap();
                i.write().unwrap();
            }
            "ignore" => f.write(".gitignore", b"different\n"),
            "config" => f.write("pyproject.toml", b"new=true\n"),
            _ => unreachable!(),
        }
        assert!(snapshot.verify_live(&f.repo, &binding).is_err(), "{change}");
    }
}
#[test]
fn live_snapshot_extra_empty_directory_content_and_git_drift_are_refused() {
    for change in ["empty", "extra", "bytes", "head", "hardlink", "write-mode"] {
        let f = Fixture::new();
        let (snapshot, binding) = f.capture(&["tracked.py"]);
        match change {
            "empty" => fs::create_dir(snapshot.worktree.join("extra")).unwrap(),
            "extra" => fs::write(snapshot.worktree.join("extra"), b"new").unwrap(),
            "bytes" => fs::write(snapshot.worktree.join("tracked.py"), b"drift").unwrap(),
            "head" => fs::write(
                snapshot.worktree.join(".git/HEAD"),
                b"ref: refs/heads/missing\n",
            )
            .unwrap(),
            "hardlink" => fs::hard_link(
                snapshot.worktree.join("tracked.py"),
                f.root.path().join("alias"),
            )
            .unwrap(),
            "write-mode" => fs::set_permissions(
                snapshot.worktree.join("tracked.py"),
                fs::Permissions::from_mode(0o664),
            )
            .unwrap(),
            _ => unreachable!(),
        }
        assert!(snapshot.verify_live(&f.repo, &binding).is_err(), "{change}");
    }
}
#[test]
fn explicit_empty_roots_nested_empty_roots_aliases_and_missing_roots_refuse() {
    for roots in [
        vec![],
        vec!["empty"],
        vec!["data"],
        vec!["missing"],
        vec!["alias"],
        vec!["data", "data/leaf"],
        vec!["../escape"],
        vec![".git"],
    ] {
        let f = Fixture::new();
        fs::create_dir(f.repo.join("empty")).unwrap();
        f.write("data/leaf", b"ok");
        fs::create_dir(f.repo.join("data/empty")).unwrap();
        symlink("data", f.repo.join("alias")).unwrap();
        let mut snapshot = Snapshot::allocate(f.root.path()).unwrap();
        assert!(
            snapshot
                .populate_live(
                    &f.repo,
                    &roots.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()
                )
                .is_err(),
            "{roots:?}"
        );
    }
}
#[test]
fn live_outputs_must_not_overlap_inputs_git_or_owned_snapshot() {
    let f = Fixture::new();
    f.write("data/leaf", b"ok");
    let (snapshot, binding) = f.capture(&["data"]);
    for p in [
        f.repo.join("data"),
        f.repo.join("data/result"),
        f.repo.join("tracked.py"),
        f.repo.join(".git"),
        snapshot.root.clone(),
    ] {
        assert!(binding.reject_output(&p).is_err(), "{}", p.display());
    }
    binding
        .reject_output(&f.root.path().join("output.json"))
        .unwrap();
}
#[test]
fn allocation_cleanup_refuses_replaced_root_and_never_deletes_replacement() {
    let f = Fixture::new();
    let snapshot = Snapshot::allocate(f.root.path()).unwrap();
    let path = snapshot.root.clone();
    fs::rename(&path, f.root.path().join("retained-original")).unwrap();
    fs::create_dir(&path).unwrap();
    fs::write(path.join("sentinel"), b"keep").unwrap();
    let error = snapshot.remove().unwrap_err();
    assert!(error.contains("identity changed"), "{error}");
    assert!(!error.contains("survived removal"), "{error}");
    assert_eq!(fs::read(path.join("sentinel")).unwrap(), b"keep");
    let snapshot = Snapshot::allocate(f.root.path()).unwrap();
    let path = snapshot.root.clone();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(snapshot.remove().is_err());
    assert!(path.is_dir());
}

#[test]
fn live_special_alias_config_and_root_count_limits_fail_closed() {
    for case in [
        "hardlink",
        "fifo",
        "config-link",
        "config-directory",
        "root-count",
    ] {
        let f = Fixture::new();
        f.write("data/leaf", b"ok");
        let mut roots = vec!["data".to_owned()];
        match case {
            "hardlink" => {
                fs::hard_link(f.repo.join("data/leaf"), f.root.path().join("alias")).unwrap()
            }
            "fifo" => {
                let path =
                    std::ffi::CString::new(f.repo.join("data/pipe").as_os_str().as_encoded_bytes())
                        .unwrap();
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            }
            "config-link" => symlink("tracked.py", f.repo.join("pyproject.toml")).unwrap(),
            "config-directory" => fs::create_dir(f.repo.join("pyproject.toml")).unwrap(),
            "root-count" => roots = vec!["data".to_owned(); 257],
            _ => unreachable!(),
        }
        let mut snapshot = Snapshot::allocate(f.root.path()).unwrap();
        assert!(snapshot.populate_live(&f.repo, &roots).is_err(), "{case}");
    }
}
#[test]
fn deleted_head_file_absence_and_explicit_file_scope_are_reverified() {
    let f = Fixture::new();
    fs::remove_file(f.repo.join("tracked.py")).unwrap();
    f.write("input.bin", b"selected");
    f.write("other.bin", b"unselected");
    let (snapshot, binding) = f.capture(&["input.bin"]);
    assert!(!snapshot.worktree.join("tracked.py").exists());
    assert!(!snapshot.worktree.join("other.bin").exists());
    f.write("tracked.py", b"first reappearance");
    assert!(snapshot.verify_live(&f.repo, &binding).is_err());
}

#[test]
fn control_identity_changes_without_source_selection_changes_and_original_hold_rejects() {
    let f = Fixture::new();
    let (first, old) = f.capture(&["tracked.py"]);
    fs::write(
        f.repo.join(".git/info/exclude"),
        b"ignored-but-not-selected\n",
    )
    .unwrap();
    let (second, new) = f.capture(&["tracked.py"]);
    let before = old.evidence();
    let after = new.evidence();
    assert_eq!(before["inputs"], after["inputs"]);
    assert_ne!(before["controls"]["digest"], after["controls"]["digest"]);
    assert!(first.verify_live(&f.repo, &old).is_err());
    second.verify_live(&f.repo, &new).unwrap();
}

#[path = "mutation_snapshot_tracked_live_tests.rs"]
mod tracked_live_tests;
