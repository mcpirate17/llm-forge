use super::*;
use crate::mutation_run_test_support::{change, repository, write};

fn replacement_fixture() -> (
    tempfile::TempDir,
    serde_json::Value,
    std::collections::BTreeMap<String, String>,
) {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "subject.py", "value = 1\n");
    write(root.path(), "other.py", "other = 7\n");
    let row = serde_json::json!({"path":"subject.py","byte_offset":8,"byte_length":1,
        "original_text":"1","mutated_text":"2"});
    let pins = std::collections::BTreeMap::from([(
        "subject.py".into(),
        crate::mutation_campaign::sha256_hex(b"value = 1\n"),
    )]);
    (root, row, pins)
}

#[test]
fn attribution_identity_binds_exact_mutant_and_every_other_input() {
    let (root, row, pins) = replacement_fixture();
    let selection = SnapshotSelection::new(&[], &["subject.py".into()]).unwrap();
    let original = materialized_input_identity(root.path(), &selection).unwrap();
    let replacement = attribution_replacement(root.path(), &row, &pins).unwrap();
    write(root.path(), "subject.py", "value = 2\n");
    let observed =
        || materialized_input_identity_with_replacement(root.path(), &selection, &replacement);
    assert_eq!(observed().unwrap(), original);
    assert_ne!(
        materialized_input_identity(root.path(), &selection).unwrap(),
        original
    );
    write(root.path(), "other.py", "other = 8\n");
    assert_ne!(observed().unwrap(), original);
    write(root.path(), "other.py", "other = 7\n");
    write(root.path(), "added.py", "new = 1\n");
    assert_ne!(observed().unwrap(), original);
    fs::remove_file(root.path().join("added.py")).unwrap();
    write(root.path(), "subject.py", "value = 3\n");
    assert!(observed().is_err());
    write(root.path(), "subject.py", "value = 1\n");
    assert!(
        observed().is_err(),
        "restored source must leave the mutant-only scope"
    );
    assert_eq!(
        materialized_input_identity(root.path(), &selection).unwrap(),
        original
    );
}

#[test]
fn attribution_identity_rejects_target_aliases_modes_and_exclusions() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let (root, row, pins) = replacement_fixture();
    let replacement = attribution_replacement(root.path(), &row, &pins).unwrap();
    let selection = SnapshotSelection::new(&[], &[]).unwrap();
    let observed =
        || materialized_input_identity_with_replacement(root.path(), &selection, &replacement);
    write(root.path(), "subject.py", "value = 2\n");
    fs::set_permissions(
        root.path().join("subject.py"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(observed().is_err());
    fs::set_permissions(
        root.path().join("subject.py"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    fs::hard_link(root.path().join("subject.py"), root.path().join("alias")).unwrap();
    assert!(observed().is_err());
    fs::remove_file(root.path().join("alias")).unwrap();
    let excluded = SnapshotSelection::new(&["subject.py".into()], &[]).unwrap();
    assert!(
        materialized_input_identity_with_replacement(root.path(), &excluded, &replacement).is_err()
    );
    fs::remove_file(root.path().join("subject.py")).unwrap();
    assert!(observed().is_err());
    symlink("other.py", root.path().join("subject.py")).unwrap();
    assert!(observed().is_err());
}

#[test]
fn attribution_replacement_refuses_unpinned_or_inconsistent_engine_rows() {
    let (root, row, pins) = replacement_fixture();
    for (field, value) in [
        ("path", serde_json::json!("other.py")),
        ("path", serde_json::json!("../subject.py")),
        ("byte_offset", serde_json::json!(u64::MAX)),
        ("byte_length", serde_json::json!(2)),
        ("original_text", serde_json::json!("9")),
        ("mutated_text", serde_json::json!("1")),
    ] {
        let mut bad = row.clone();
        bad[field] = value;
        assert!(
            attribution_replacement(root.path(), &bad, &pins).is_err(),
            "{field}"
        );
    }
    write(root.path(), "subject.py", "value = 9\n");
    assert!(attribution_replacement(root.path(), &row, &pins).is_err());
}

#[test]
fn bound_snapshot_excludes_owned_outputs_and_rechecks_real_inputs() {
    let host = repository(&["src/lib.rs", "assets/data.bin", "campaigns/receipt.json"]);
    change(
        host.path(),
        &["src/lib.rs", "assets/data.bin", "campaigns/receipt.json"],
    );
    let selection = SnapshotSelection::new(
        &["campaigns/receipt.json".into()],
        &["src/lib.rs".into(), "assets/data.bin".into()],
    )
    .unwrap();
    let private = tempfile::tempdir().unwrap();
    let mut snapshot = Snapshot::allocate(private.path()).unwrap();
    let captured = snapshot.populate_bound(host.path(), &selection).unwrap();
    assert!(!snapshot.worktree.join("campaigns/receipt.json").exists());
    assert_eq!(
        captured,
        materialized_input_identity(&snapshot.worktree, &selection).unwrap()
    );
    assert_eq!(
        captured,
        live_input_identity(host.path(), &selection).unwrap()
    );
    write(
        host.path(),
        "campaigns/receipt.json",
        "changed owned output",
    );
    assert_eq!(
        captured,
        live_input_identity(host.path(), &selection).unwrap()
    );
    write(host.path(), "assets/data.bin", "changed asset bytes");
    assert_ne!(
        captured,
        live_input_identity(host.path(), &selection).unwrap()
    );
    write(&snapshot.worktree, "src/new.rs", "fn changed() {}\n");
    assert_ne!(
        captured,
        materialized_input_identity(&snapshot.worktree, &selection).unwrap()
    );
}

#[test]
fn committed_git_and_materialized_inputs_use_identical_framing() {
    let host = repository(&["src/lib.rs", "assets/data.bin"]);
    let selection = SnapshotSelection::new(&[], &["src/lib.rs".into()]).unwrap();
    let source = crate::candidate_source::Source::open(host.path()).unwrap();
    let tree = source
        .repository()
        .head()
        .unwrap()
        .peel_to_tree()
        .unwrap()
        .id()
        .to_string();
    let git = git_input_identity(&source, &tree, &selection).unwrap();
    let snapshot = Snapshot::create(host.path()).unwrap();
    assert_eq!(
        git,
        materialized_input_identity(&snapshot.worktree, &selection).unwrap()
    );
}

#[test]
fn tracked_mode_and_symlink_targets_are_bound() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let host = repository(&["src/lib.rs", "assets/a.txt", "assets/b.txt"]);
    symlink("a.txt", host.path().join("assets/link")).unwrap();
    crate::mutation_run_test_support::git(host.path(), &["add", "assets/link"]);
    crate::mutation_run_test_support::git(host.path(), &["commit", "-q", "-m", "link"]);
    let selection = SnapshotSelection::new(&[], &["src/lib.rs".into()]).unwrap();
    let baseline = live_input_identity(host.path(), &selection).unwrap();
    fs::set_permissions(
        host.path().join("src/lib.rs"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert_ne!(
        baseline,
        live_input_identity(host.path(), &selection).unwrap()
    );
    fs::set_permissions(
        host.path().join("src/lib.rs"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    fs::remove_file(host.path().join("assets/link")).unwrap();
    symlink("b.txt", host.path().join("assets/link")).unwrap();
    assert_ne!(
        baseline,
        live_input_identity(host.path(), &selection).unwrap()
    );
}

#[test]
fn excluded_output_aliases_are_refused() {
    use std::os::unix::fs::symlink;
    let host = repository(&["src/lib.rs", "campaigns/output.json"]);
    let selection =
        SnapshotSelection::new(&["campaigns/output.json".into()], &["src/lib.rs".into()]).unwrap();
    fs::remove_file(host.path().join("campaigns/output.json")).unwrap();
    fs::hard_link(
        host.path().join("src/lib.rs"),
        host.path().join("campaigns/output.json"),
    )
    .unwrap();
    assert!(live_input_identity(host.path(), &selection)
        .unwrap_err()
        .contains("hardlinked"));
    fs::remove_file(host.path().join("campaigns/output.json")).unwrap();
    symlink("../src/lib.rs", host.path().join("campaigns/output.json")).unwrap();
    assert!(live_input_identity(host.path(), &selection)
        .unwrap_err()
        .contains("symlink"));
}

#[test]
fn staged_additions_are_bound_and_match_the_staged_git_tree() {
    let host = repository(&["src/lib.rs", "asset.txt"]);
    write(host.path(), "src/new.rs", "fn original() {}\n");
    crate::mutation_run_test_support::git(host.path(), &["add", "src/new.rs"]);
    let selection =
        SnapshotSelection::new(&[], &["src/lib.rs".into(), "src/new.rs".into()]).unwrap();
    let captured = live_input_identity(host.path(), &selection).unwrap();
    let repository = git2::Repository::open(host.path()).unwrap();
    let tree = repository
        .index()
        .unwrap()
        .write_tree()
        .unwrap()
        .to_string();
    let source = crate::candidate_source::Source::open(host.path()).unwrap();
    assert_eq!(
        captured,
        git_input_identity(&source, &tree, &selection).unwrap()
    );
    let private = tempfile::tempdir().unwrap();
    let mut snapshot = Snapshot::allocate(private.path()).unwrap();
    assert_eq!(
        captured,
        snapshot.populate_bound(host.path(), &selection).unwrap()
    );
    assert!(snapshot.worktree.join("src/new.rs").is_file());
    write(host.path(), "src/new.rs", "fn changed_() {}\n");
    assert_ne!(
        captured,
        live_input_identity(host.path(), &selection).unwrap()
    );
    fs::remove_file(host.path().join("src/new.rs")).unwrap();
    assert_ne!(
        captured,
        live_input_identity(host.path(), &selection).unwrap()
    );
}

#[test]
fn git_live_and_materialized_refuse_output_links() {
    use std::os::unix::fs::symlink;
    let host = repository(&["src/lib.rs", "out/receipt.json"]);
    symlink("../out/receipt.json", host.path().join("src/link")).unwrap();
    crate::mutation_run_test_support::git(host.path(), &["add", "src/link"]);
    let repo = git2::Repository::open(host.path()).unwrap();
    let tree = repo.index().unwrap().write_tree().unwrap().to_string();
    let selection =
        SnapshotSelection::new(&["out/receipt.json".into()], &["src/lib.rs".into()]).unwrap();
    let source = crate::candidate_source::Source::open(host.path()).unwrap();
    assert!(git_input_identity(&source, &tree, &selection).is_err());
    assert!(live_input_identity(host.path(), &selection).is_err());
    let snapshot = Snapshot::create(host.path()).unwrap();
    symlink("../out/receipt.json", snapshot.worktree.join("src/link")).unwrap();
    assert!(materialized_input_identity(&snapshot.worktree, &selection).is_err());
}
