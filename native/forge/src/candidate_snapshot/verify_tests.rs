use super::*;
use std::{fs, os::unix::fs::symlink};

fn sample(f: &Fixture) -> (Source, Snapshot) {
    let source = f.source();
    let tree = f.tree(&[
        ("dir/data", &[0, 255, 10], 0o100644),
        ("run", b"executable", 0o100755),
        ("link", b"dir/data", 0o120000),
        ("dangling", b"missing", 0o120000),
    ]);
    let snapshot = Snapshot::create(&source, &tree, &f.output(), Limits::default()).unwrap();
    snapshot.verify_materialized(&source).unwrap();
    (source, snapshot)
}
#[test]
fn committed_verifier_accepts_binary_executable_links_and_empty_tree() {
    let f = Fixture::new();
    let (source, snapshot) = sample(&f);
    snapshot.verify_materialized(&source).unwrap();
    snapshot.discard().unwrap();
    let tree = f.tree(&[]);
    let snapshot = Snapshot::create(&source, &tree, &f.output(), Limits::default()).unwrap();
    snapshot.verify_materialized(&source).unwrap();
}
#[test]
fn committed_verifier_rejects_content_missing_modes_and_unread_extra_paths() {
    for change in [
        "bytes", "missing", "execute", "write", "extra", "empty", "metadata",
    ] {
        let f = Fixture::new();
        let (source, snapshot) = sample(&f);
        let root = snapshot.path();
        match change {
            "bytes" => fs::write(root.join("dir/data"), [0, 254, 10]).unwrap(),
            "missing" => fs::remove_file(root.join("run")).unwrap(),
            "execute" => {
                fs::set_permissions(root.join("run"), fs::Permissions::from_mode(0o644)).unwrap()
            }
            "write" => {
                fs::set_permissions(root.join("dir/data"), fs::Permissions::from_mode(0o664))
                    .unwrap()
            }
            "extra" => fs::write(root.join("new"), b"extra").unwrap(),
            "empty" => fs::create_dir(root.join("empty")).unwrap(),
            "metadata" => fs::create_dir(root.join(".git")).unwrap(),
            _ => unreachable!(),
        }
        assert!(snapshot.verify_materialized(&source).is_err(), "{change}");
    }
}
#[test]
fn committed_verifier_rejects_changed_links_types_and_aliases() {
    for change in ["link", "parent", "hardlink", "fifo", "directory"] {
        let f = Fixture::new();
        let (source, snapshot) = sample(&f);
        let root = snapshot.path();
        match change {
            "link" => {
                fs::remove_file(root.join("link")).unwrap();
                symlink("run", root.join("link")).unwrap();
            }
            "parent" => {
                fs::rename(root.join("dir"), f.root.join("moved-dir")).unwrap();
                symlink(f.root.join("moved-dir"), root.join("dir")).unwrap();
            }
            "hardlink" => fs::hard_link(root.join("run"), f.root.join("alias")).unwrap(),
            "fifo" => {
                fs::remove_file(root.join("run")).unwrap();
                let name = CString::new(root.join("run").as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o755) }, 0);
            }
            "directory" => {
                fs::remove_file(root.join("run")).unwrap();
                fs::create_dir(root.join("run")).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(snapshot.verify_materialized(&source).is_err(), "{change}");
    }
}
#[test]
fn committed_verifier_rejects_replaced_root_and_preserves_admitted_limits() {
    let f = Fixture::new();
    let (source, snapshot) = sample(&f);
    let path = snapshot.path();
    fs::rename(&path, f.root.join("old-snapshot")).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(snapshot.verify_materialized(&source).is_err());
    drop(snapshot);
    let tree = f.tree(&[("small", b"ok", 0o100644)]);
    let limits = Limits {
        max_blob_bytes: 2,
        max_tree_bytes: 2,
    };
    let snapshot = Snapshot::create(&source, &tree, &f.output(), limits).unwrap();
    snapshot.verify_materialized(&source).unwrap();
    fs::write(snapshot.path().join("small"), b"big").unwrap();
    assert!(snapshot.verify_materialized(&source).is_err());
}
#[test]
fn committed_verifier_refuses_generic_materializer_skipped_gitlinks() {
    let f = Fixture::new();
    let tree = f.tree_objects(&[(
        "submodule",
        Oid::from_str(&"1".repeat(40)).unwrap(),
        0o160000,
    )]);
    let source = f.source();
    let snapshot =
        Snapshot::create(&source, &tree.to_string(), &f.output(), Limits::default()).unwrap();
    assert_eq!(snapshot.report().skipped_gitlinks, 1);
    assert!(snapshot
        .verify_materialized(&source)
        .unwrap_err()
        .to_string()
        .contains("gitlinks"));
}
