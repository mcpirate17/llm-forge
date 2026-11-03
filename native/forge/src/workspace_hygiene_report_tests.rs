use super::*;
fn empty() -> Report {
    Report {
        live_ref: "main".into(),
        redundant_branches: vec![],
        stale_worktrees: vec![],
        dirty_worktrees: vec![],
        landed_worktrees: vec![],
        landed_ref: "origin/main".into(),
        expired_claims: vec![],
        idle_claims: vec![],
        unpushed_work: vec![],
        orphan_manifests: vec![],
        dangling_manifests: vec![],
        untracked_imports: vec![],
        stale_feature_branches: vec![],
        gh_available: true,
        local_only_commit_exposure: vec![],
        stale_dirty_files: vec![],
        worktree_leases: vec![],
        checkout_drift: json!({"remote":"origin/main","behind":0,"ahead":0,"fast_forwardable":false,"blocked_by":[]}),
        gh_errors: vec![],
        reap_preview: vec![],
    }
}
#[test]
fn exposure_exit_predicate_does_not_conflate_cleanup_or_checkout_drift() {
    let mut report = empty();
    assert!(report.is_clean());
    report.idle_claims.push(json!({"claim_id":"idle"}));
    assert!(!report.is_clean());
    assert!(!report.has_exposure());
    report.idle_claims.clear();
    report.checkout_drift["behind"] = json!(2);
    assert!(!report.is_clean());
    assert!(!report.has_exposure());
    report.stale_dirty_files.push(json!({"path":"old.py"}));
    assert!(report.has_exposure());
}
#[test]
fn every_legacy_json_field_is_retained_and_optional_gh_errors_are_visible() {
    let mut report = empty();
    report.gh_errors.push(json!("offline"));
    let value = serde_json::to_value(&report).unwrap();
    for field in [
        "live_ref",
        "redundant_branches",
        "stale_worktrees",
        "dirty_worktrees",
        "landed_worktrees",
        "landed_ref",
        "expired_claims",
        "idle_claims",
        "unpushed_work",
        "orphan_manifests",
        "dangling_manifests",
        "untracked_imports",
        "stale_feature_branches",
        "gh_available",
        "local_only_commit_exposure",
        "stale_dirty_files",
        "worktree_leases",
        "checkout_drift",
    ] {
        assert!(value.get(field).is_some(), "missing {field}");
    }
    assert_eq!(value["gh_errors"], json!(["offline"]));
    assert!(value.get("reap_preview").is_none());
    assert!(!report.is_clean());
}
#[test]
fn human_report_keeps_drift_lease_claim_import_and_exposure_context() {
    let mut report = empty();
    report.checkout_drift = json!({"remote":"origin/main","behind":2,"ahead":1,"fast_forwardable":false,"blocked_by":["local file.py"]});
    report
        .worktree_leases
        .push(json!({"worktree":"/owned/linked","status":"unleased"}));
    report
        .untracked_imports
        .push(json!({"module":"helper.py","importers":["a.py"],"tracked_importers":["a.py"]}));
    let output = render(&report).unwrap();
    for expected in [
        "CHECKOUT BEHIND -- 2 commit(s) behind origin/main",
        "blocked by local edit: local file.py",
        "1 commit(s) ahead",
        "/owned/linked -- no lease",
        "helper.py  <- 1 importer(s), 1 tracked",
        "SAFE TO DELETE",
        "DO NOT REMOVE",
        "IDLE CLAIMS",
        "EXPIRED CLAIMS",
    ] {
        assert!(output.contains(expected), "missing {expected}: {output}");
    }
}
#[test]
fn redundant_local_branch_with_dirty_work_is_blocked_and_never_mutated() {
    use git2::{Repository, Signature};
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let repo = Repository::init(root).unwrap();
    std::fs::write(root.join("a.py"), "x=1\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("a.py")).unwrap();
    let oid = index.write_tree().unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(oid).unwrap();
    let signature = Signature::now("fixture", "fixture@example.invalid").unwrap();
    let id = repo
        .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .unwrap();
    let commit = repo.find_commit(id).unwrap();
    repo.branch("finished", &commit, false).unwrap();
    let live = repo.head().unwrap().shorthand().unwrap().to_owned();
    let rows = facts::redundant(
        root,
        &live,
        &live,
        &[json!({"branch":"refs/heads/finished"})],
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["blocked_by"], "worktree holds uncommitted work");
    assert!(repo
        .find_branch("finished", git2::BranchType::Local)
        .is_ok());
    assert_eq!(std::fs::read_to_string(root.join("a.py")).unwrap(), "x=1\n");
}

#[test]
fn primary_git_directory_layout_is_never_a_landed_removal_row() {
    use git2::{Repository, Signature};
    let directory = tempfile::tempdir().unwrap();
    let repo = Repository::init(directory.path()).unwrap();
    let mut index = repo.index().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let author = Signature::now("fixture", "fixture@example.invalid").unwrap();
    let oid = repo
        .commit(Some("HEAD"), &author, &author, "empty", &tree, &[])
        .unwrap();
    let another_primary = tempfile::tempdir().unwrap();
    Repository::init(another_primary.path()).unwrap();
    let trees = [
        json!({"worktree":another_primary.path(),"HEAD":oid.to_string(),"branch":"refs/heads/main"}),
    ];
    assert!(facts::landed(directory.path(), "HEAD", &trees)
        .unwrap()
        .is_empty());
}

#[test]
fn tracked_only_drift_and_unverified_remote_retirement_do_not_claim_safe_mutation() {
    let mut report = empty();
    report.checkout_drift = json!({"remote":"origin/main","behind":2,"ahead":0,"fast_forwardable":true,"blocked_by":[]});
    report.landed_worktrees.push(
        json!({"worktree":"/owned/linked","branch":"feature","head":"12345678",
        "reason":"remote branch is gone; merge is unverified","requires_review":true}),
    );
    let output = render(&report).unwrap();
    assert!(output.contains("tracked changes alone do not block"));
    assert!(output.contains("untracked/ignored collisions"));
    assert!(output.contains("merge is unverified (review only)"));
    assert!(!output.contains("SAFE TO REMOVE"));
    assert!(!output.contains("git worktree remove"));
    assert!(!output.contains("a fast-forward makes it even"));
}
