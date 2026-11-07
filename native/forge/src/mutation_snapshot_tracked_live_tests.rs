//! Tracked-only selection controls; all sources are real authored Git fixtures.
use super::*;
use std::{ffi::CString, os::unix::ffi::OsStrExt};

fn stage(f: &Fixture, paths: &[&str]) {
    let repository = Repository::open(&f.repo).unwrap();
    let mut index = repository.index().unwrap();
    for path in paths {
        index.add_path(Path::new(path)).unwrap();
    }
    index.write().unwrap();
}
fn commit_index(f: &Fixture, message: &str) -> String {
    let repository = Repository::open(&f.repo).unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let tree = repository
        .find_tree(repository.index().unwrap().write_tree().unwrap())
        .unwrap();
    let signature = Signature::now("fixture", "fixture@example.invalid").unwrap();
    repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &[&parent],
        )
        .unwrap()
        .to_string()
}
fn capture(f: &Fixture) -> (Snapshot, LiveBinding) {
    let mut snapshot = Snapshot::allocate(f.root.path()).unwrap();
    let binding = snapshot.populate_live_tracked(&f.repo).unwrap();
    snapshot.verify_live(&f.repo, &binding).unwrap();
    let evidence = binding.evidence();
    assert_eq!(evidence["profile"], "tracked-live-inputs-v1");
    assert_eq!(evidence["roots"], json!({}));
    assert!(evidence["controls_held"].as_bool().unwrap());
    (snapshot, binding)
}
fn remove(snapshot: Snapshot) {
    let root = snapshot.root.clone();
    snapshot.remove().unwrap();
    assert!(
        matches!(fs::symlink_metadata(root), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    );
}
fn captured_paths(snapshot: &Snapshot, binding: &LiveBinding) -> BTreeSet<String> {
    Source::open(&snapshot.worktree)
        .unwrap()
        .list_tree(binding.tree())
        .unwrap()
        .into_iter()
        .map(|entry| entry.path)
        .collect()
}

#[test]
fn tracked_live_uses_disk_bytes_for_head_index_union_and_never_resurrects_deletions() {
    let f = Fixture::new();
    f.write("gone", b"HEAD bytes");
    f.write("index-deleted-present", b"HEAD original");
    stage(&f, &["gone", "index-deleted-present"]);
    let head = commit_index(&f, "tracked union fixture");
    f.write("tracked.py", b"dirty = 2\n");
    f.write("staged.bin", b"index original");
    f.write("staged-and-gone", b"index only");
    stage(&f, &["staged.bin", "staged-and-gone"]);
    let repository = Repository::open(&f.repo).unwrap();
    let mut index = repository.index().unwrap();
    index
        .remove_path(Path::new("index-deleted-present"))
        .unwrap();
    index.write().unwrap();
    f.write("index-deleted-present", b"working survivor");
    f.write("staged.bin", &[0, 255, 9]);
    fs::remove_file(f.repo.join("gone")).unwrap();
    fs::remove_file(f.repo.join("staged-and-gone")).unwrap();
    f.write(
        "pyproject.toml",
        b"[tool.forge.equivalence_probe]\njobs=1\n",
    );
    f.write("untracked.py", b"must not be selected\n");
    let index_before = fs::read(f.repo.join(".git/index")).unwrap();
    let objects_before = object_names(&f.repo.join(".git/objects"));
    let (snapshot, binding) = capture(&f);
    let expected: BTreeSet<_> = [
        ".gitignore",
        "tracked.py",
        "staged.bin",
        "index-deleted-present",
        "pyproject.toml",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    assert_eq!(captured_paths(&snapshot, &binding), expected);
    for path in &expected {
        assert_eq!(
            fs::read(snapshot.worktree.join(path)).unwrap(),
            fs::read(f.repo.join(path)).unwrap()
        );
    }
    for path in ["gone", "staged-and-gone", "untracked.py"] {
        assert!(!snapshot.worktree.join(path).exists());
    }
    assert_eq!(binding.evidence()["inputs"]["files"], 5);
    assert_eq!(binding.evidence()["host_head"], head);
    assert_eq!(snapshot.commit, binding.commit());
    assert_eq!(index_before, fs::read(f.repo.join(".git/index")).unwrap());
    assert_eq!(objects_before, object_names(&f.repo.join(".git/objects")));
    remove(snapshot);
}

#[test]
fn tracked_live_omits_untracked_and_ignored_unsupported_or_symlink_caches_without_traversal() {
    let f = Fixture::new();
    for cache in ["ignored", "untracked-cache"] {
        f.write(
            &format!("{cache}/.gitattributes"),
            b"unsupported cache attributes\n",
        );
        f.write(
            &format!("{cache}/.git/sentinel"),
            b"nested metadata must not be visited",
        );
        let fifo = CString::new(f.repo.join(cache).join("pipe").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    }
    let outside = f.root.path().join("outside-cache");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join(".gitattributes"), b"not a selected source\n").unwrap();
    fs::write(outside.join("sentinel"), b"outside preserved").unwrap();
    symlink(&outside, f.repo.join("cache-link")).unwrap();
    f.write("unrelated.bin", &[0, 255]);
    let (snapshot, binding) = capture(&f);
    assert_eq!(
        captured_paths(&snapshot, &binding),
        BTreeSet::from([".gitignore".into(), "tracked.py".into()])
    );
    for omitted in ["ignored", "untracked-cache", "cache-link", "unrelated.bin"] {
        assert!(fs::symlink_metadata(snapshot.worktree.join(omitted)).is_err());
    }
    // Changing omitted cache membership/content must not broaden the held selection.
    f.write("ignored/new", b"new ignored cache data");
    f.write("untracked-cache/new", b"new untracked cache data");
    fs::write(outside.join("sentinel"), b"outside changed independently").unwrap();
    snapshot.verify_live(&f.repo, &binding).unwrap();
    remove(snapshot);
    assert_eq!(
        fs::read(outside.join("sentinel")).unwrap(),
        b"outside changed independently"
    );
}

#[test]
fn tracked_live_holds_bytes_modes_absence_index_head_config_controls_and_materialized_inputs() {
    for change in [
        "bytes",
        "mode",
        "removed",
        "reappeared",
        "index",
        "head",
        "config",
        "controls",
        "snapshot",
    ] {
        let f = Fixture::new();
        if change == "reappeared" {
            fs::remove_file(f.repo.join("tracked.py")).unwrap();
        }
        let (snapshot, binding) = capture(&f);
        match change {
            "bytes" => f.write("tracked.py", b"value = 2\n"),
            "mode" => {
                fs::set_permissions(f.repo.join("tracked.py"), fs::Permissions::from_mode(0o755))
                    .unwrap()
            }
            "removed" => fs::remove_file(f.repo.join("tracked.py")).unwrap(),
            "reappeared" => f.write("tracked.py", b"value = 1\n"),
            "index" => {
                f.write("new.py", b"new = True\n");
                stage(&f, &["new.py"]);
            }
            "head" => {
                commit_index(&f, "new HEAD with same tree");
            }
            "config" => f.write(
                "pyproject.toml",
                b"[tool.forge.equivalence_probe]\njobs=1\n",
            ),
            "controls" => {
                fs::write(f.repo.join(".git/info/exclude"), b"changed control\n").unwrap()
            }
            "snapshot" => fs::write(
                snapshot.worktree.join("tracked.py"),
                b"changed private bytes\n",
            )
            .unwrap(),
            _ => unreachable!(),
        }
        assert!(
            snapshot.verify_live(&f.repo, &binding).is_err(),
            "accepted {change} drift"
        );
        remove(snapshot);
    }
}

#[test]
fn empty_tracked_tree_is_admitted_and_root_config_is_the_only_mandatory_exception() {
    let f = Fixture::new();
    let repository = Repository::open(&f.repo).unwrap();
    let mut index = repository.index().unwrap();
    index.clear().unwrap();
    index.write().unwrap();
    commit_index(&f, "empty tracked tree");
    let (first, absent_config) = capture(&f);
    assert!(captured_paths(&first, &absent_config).is_empty());
    assert_eq!(absent_config.evidence()["inputs"]["files"], 0);
    assert_eq!(
        absent_config.tree(),
        "4b825dc642cb6eb9a060e54bf8d69288fbee4904"
    );
    f.write(
        "pyproject.toml",
        b"[project]\nname='mandatory-live-config'\n",
    );
    assert!(first.verify_live(&f.repo, &absent_config).is_err());
    remove(first);
    let (second, configured) = capture(&f);
    assert_eq!(
        captured_paths(&second, &configured),
        BTreeSet::from(["pyproject.toml".into()])
    );
    assert_eq!(
        fs::read(second.worktree.join("pyproject.toml")).unwrap(),
        fs::read(f.repo.join("pyproject.toml")).unwrap()
    );
    second.verify_live(&f.repo, &configured).unwrap();
    remove(second);
}

#[test]
fn existing_explicit_live_api_still_refuses_empty_roots_and_keeps_its_profile() {
    let f = Fixture::new();
    let mut refused = Snapshot::allocate(f.root.path()).unwrap();
    let error = refused
        .populate_live(&f.repo, &[])
        .err()
        .expect("empty explicit roots accepted");
    assert!(
        error.contains("live input roots must contain 1..256 paths"),
        "{error}"
    );
    remove(refused);
    let (explicit, binding) = f.capture(&["tracked.py"]);
    assert_eq!(binding.evidence()["profile"], "explicit-live-inputs-v1");
    assert_eq!(binding.evidence()["roots"], json!({"tracked.py":false}));
    remove(explicit);
}

#[test]
fn tracked_live_keeps_unsafe_path_refusal_output_protection_and_failure_cleanup() {
    let f = Fixture::new();
    let (snapshot, binding) = capture(&f);
    assert!(binding.reject_output(&f.repo.join("tracked.py")).is_err());
    assert!(binding
        .reject_output(&f.repo.join("pyproject.toml"))
        .is_ok());
    assert!(binding.reject_output(&f.repo.join(".git/output")).is_err());
    assert!(binding
        .reject_output(&snapshot.worktree.join("output"))
        .is_err());
    remove(snapshot);
    fs::remove_file(f.repo.join("tracked.py")).unwrap();
    let outside = f.root.path().join("outside-sentinel");
    fs::write(&outside, b"outside unchanged").unwrap();
    symlink(&outside, f.repo.join("tracked.py")).unwrap();
    let mut failed = Snapshot::allocate(f.root.path()).unwrap();
    assert!(failed.populate_live_tracked(&f.repo).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"outside unchanged");
    remove(failed);
}
