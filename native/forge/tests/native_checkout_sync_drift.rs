//! Read-only drift fixtures. These tests author disposable repositories only.
use std::path::Path;
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn reports_even_behind_tracked_overlap_and_untracked_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.name", "Fixture"]);
    git(root, &["config", "user.email", "fixture@example.invalid"]);
    std::fs::write(root.join("tracked"), "base\n").unwrap();
    git(root, &["add", "tracked"]);
    git(root, &["commit", "-m", "base"]);
    let base = git(root, &["rev-parse", "HEAD"]);
    git(root, &["update-ref", "refs/remotes/origin/main", &base]);
    git(
        root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let even = forge::checkout_drift::report(root).unwrap();
    assert_eq!(even["behind"], 0);
    assert_eq!(even["ahead"], 0);
    assert_eq!(even["fast_forwardable"], false);
    let repo = git2::Repository::open(root).unwrap();
    let parent = repo
        .find_commit(git2::Oid::from_str(&base).unwrap())
        .unwrap();
    let base_tree = parent.tree().unwrap();
    let mut builder = repo.treebuilder(Some(&base_tree)).unwrap();
    let blob = repo.blob(b"upstream\n").unwrap();
    builder.insert("tracked", blob, 0o100644).unwrap();
    let tree_id = builder.write().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let signature = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
    repo.commit(
        Some("refs/remotes/origin/main"),
        &signature,
        &signature,
        "incoming",
        &tree,
        &[&parent],
    )
    .unwrap();
    std::fs::write(root.join("untracked"), "local\n").unwrap();
    let behind = forge::checkout_drift::report(root).unwrap();
    assert_eq!(behind["behind"], 1);
    assert_eq!(behind["fast_forwardable"], true);
    assert_eq!(behind["blocked_by"], serde_json::json!([]));
    std::fs::write(root.join("tracked"), "local\n").unwrap();
    let status = git(root, &["status", "--porcelain"]);
    let blocked = forge::checkout_drift::report(root).unwrap();
    assert_eq!(blocked["blocked_by"], serde_json::json!(["tracked"]));
    assert_eq!(blocked["fast_forwardable"], false);
    assert_eq!(git(root, &["status", "--porcelain"]), status);
    assert_eq!(git(root, &["rev-parse", "HEAD"]), base);
    assert_eq!(std::fs::read(root.join("tracked")).unwrap(), b"local\n");
}
