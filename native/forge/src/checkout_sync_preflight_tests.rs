use super::*;
use git2::{Repository, Signature};
#[test]
fn preflight_binds_branch_target_index_and_parent() {
    let dir = tempfile::tempdir().unwrap();
    let repo = Repository::init(dir.path()).unwrap();
    let sig = Signature::now("test", "test@example.invalid").unwrap();
    let tree_id = repo.treebuilder(None).unwrap().write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let old = repo
        .commit(Some("refs/heads/main"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    let parent = repo.find_commit(old).unwrap();
    let new = repo
        .commit(None, &sig, &sig, "next", &tree, &[&parent])
        .unwrap();
    repo.set_head("refs/heads/main").unwrap();
    repo.reference("refs/remotes/origin/main", new, false, "test")
        .unwrap();
    let bound = Preflight::open(dir.path(), "refs/remotes/origin/main").unwrap();
    assert_eq!(bound.behind, 1);
    bound.verify().unwrap();
    repo.reference("refs/remotes/origin/main", old, true, "intervening writer")
        .unwrap();
    assert!(bound.verify().is_err());
}
