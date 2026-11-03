use super::*;
#[test]
fn named_snapshot_journals_exact_oid_before_visible_reference() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let tree_id = repo.treebuilder(None).unwrap().write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = Signature::now("test", "test@example.invalid").unwrap();
    repo.commit(Some("refs/heads/main"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let source = Source::open(dir.path()).unwrap();
    let reference = "refs/snapshots/forge/sync-test";
    let mut intended = None;
    let result = create_named(&source, Limits::default(), reference, |name, oid| {
        assert_eq!(name, reference);
        assert!(repo.find_reference(name).is_err());
        intended = Some(oid);
        Ok(())
    })
    .unwrap();
    assert_eq!(result.reference, reference);
    assert_eq!(repo.find_reference(reference).unwrap().target(), intended);
}
#[test]
fn rejected_snapshot_wal_prevents_reference_publication() {
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let tree_id = repo.treebuilder(None).unwrap().write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let sig = Signature::now("test", "test@example.invalid").unwrap();
    repo.commit(Some("refs/heads/main"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    repo.set_head("refs/heads/main").unwrap();
    let source = Source::open(dir.path()).unwrap();
    let name = "refs/snapshots/forge/refuse";
    assert!(
        create_named(&source, Limits::default(), name, |_, _| anyhow::bail!(
            "WAL failure"
        ))
        .is_err()
    );
    assert!(repo.find_reference(name).is_err());
}
