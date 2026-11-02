use super::*;
#[test]
fn full_review_requires_real_execution_and_exact_candidate_authority() {
    let expected = ["tree", "base", "policy", "grants", "full"];
    let mut review = json!({"decision":"pass","executed":false,"review_complete":false,"execution_verified":false,
        "execution_scope":"complete_active_profile","binding":{"candidate_tree_oid":"tree","base_tree_oid":"base",
        "policy_sha256":"policy","grants_sha256":"grants","profile":"full","surface":"ci"}});
    assert_eq!(review_checks(&review, expected), (true, false));
    review["executed"] = json!(true);
    review["review_complete"] = json!(true);
    review["execution_verified"] = json!(true);
    assert_eq!(review_checks(&review, expected), (true, true));
    for key in [
        "candidate_tree_oid",
        "base_tree_oid",
        "policy_sha256",
        "grants_sha256",
        "profile",
        "surface",
    ] {
        let original = review["binding"][key].clone();
        review["binding"][key] = json!("wrong");
        assert_eq!(review_checks(&review, expected), (false, true), "{key}");
        review["binding"][key] = original;
    }
    review["execution_scope"] = json!("partial");
    assert_eq!(review_checks(&review, expected), (true, false));
}
const POLICY:&str = "schema_version=1\nblock_at='high'\nmax_workers=2\ncache_ttl_days=7\nclaim_max_age_hours=24\nmax_file_bytes=100000\nmax_binary_bytes=200000\ncoverage_threshold=80.0\nhigh_risk_coverage_threshold=90.0\nbaseline_expires=2099-11-30\n[classes]\npython=['**/*.py']\ntest=['tests/**']\nnative=['**/*.rs']\n[risk]\nhigh=[]\n[paths]\nprotected_deletes=[]\nhot=[]\ngenerated=[]\n[checks.mutation-evidence]\nkind='builtin'\nprofiles=['fast']\nalways=true\n";
#[test]
fn exported_policy_is_candidate_bound_and_untracked_files_are_excluded() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    let repo = git2::Repository::init(&root).unwrap();
    std::fs::write(root.join("policy.toml"), POLICY).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("policy.toml")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("fixture", "test@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "candidate", &tree, &[])
        .unwrap();
    std::fs::write(root.join("policy.toml"), "broken = [").unwrap();
    std::fs::write(root.join("untracked.py"), "bad = True\n").unwrap();
    let source = crate::candidate_source::Source::open(&root).unwrap();
    let candidate = source.resolve(Kind::Commit, "HEAD", None).unwrap();
    let snapshot = Snapshot::create(
        &source,
        &candidate.tree_oid,
        &tmp.path().join("snapshot"),
        Limits::default(),
    )
    .unwrap();
    let policy =
        crate::candidate_policy::load(&snapshot.path(), Some("policy.toml"), "2026-11-02").unwrap();
    assert_eq!(policy.document["schema_version"], 1);
    assert!(!snapshot.path().join("untracked.py").exists());
    assert!(!snapshot.path().join(".git").exists());
    assert!(source
        .resolve(Kind::Commit, "refs/heads/no-such-ref", None)
        .is_err());
}

#[test]
fn report_statuses_distinguish_pass_failure_and_refusal() {
    assert_eq!(label(0), "PASS");
    assert_eq!(label(1), "FAIL");
    assert_eq!(label(2), "REFUSED");
    assert_eq!(verdict(&[]), 0);
    assert_eq!(verdict(&[Phase::new("failed", false, "", json!({}))]), 1);
}
#[test]
fn malformed_candidate_policy_is_rejected_even_when_worktree_is_clean() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("repo");
    let repo = git2::Repository::init(&root).unwrap();
    std::fs::write(root.join("policy.toml"), "broken = [").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("policy.toml")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("fixture", "test@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "bad candidate", &tree, &[])
        .unwrap();
    std::fs::write(root.join("policy.toml"), POLICY).unwrap();
    let source = crate::candidate_source::Source::open(&root).unwrap();
    let candidate = source.resolve(Kind::Commit, "HEAD", None).unwrap();
    let snapshot = Snapshot::create(
        &source,
        &candidate.tree_oid,
        &tmp.path().join("snapshot"),
        Limits::default(),
    )
    .unwrap();
    assert!(
        crate::candidate_policy::load(&snapshot.path(), Some("policy.toml"), "2026-11-02").is_err()
    );
}

#[test]
fn review_setup_mismatch_and_incomplete_refuse_while_completed_fail_is_a_failure() {
    let expected = ["tree", "base", "policy", "grants", "full"];
    let mut review = json!({"decision":"fail","executed":true,"review_complete":true,
        "execution_verified":true,"execution_scope":"complete_active_profile",
        "binding":{"candidate_tree_oid":"tree","base_tree_oid":"base",
        "policy_sha256":"policy","grants_sha256":"grants","profile":"full","surface":"ci"}});
    assert_eq!(
        verdict(&[review_result(Ok(review.clone()), expected).unwrap()]),
        1
    );
    assert!(review_result(Err(anyhow::anyhow!("lock unavailable")), expected).is_err());
    review["binding"]["candidate_tree_oid"] = json!("other");
    assert!(review_result(Ok(review.clone()), expected).is_err());
    review["binding"]["candidate_tree_oid"] = json!("tree");
    review["review_complete"] = json!(false);
    assert!(review_result(Ok(review.clone()), expected).is_err());
    review["review_complete"] = json!(true);
    review["decision"] = json!("unknown");
    assert!(review_result(Ok(review), expected).is_err());
}
