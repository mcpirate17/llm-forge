use super::*;
#[test]
fn selected_targets_are_exact_validated_and_duplicate_free() {
    assert_eq!(
        targets(&["python_contracts_b".into(), "python_contracts_a".into()]).unwrap(),
        ["python_contracts_a", "python_contracts_b"]
    );
    assert!(targets(&[]).is_err());
    assert!(targets(&["python_contracts_a".into(), "python_contracts_a".into()]).is_err());
    for bad in ["--help", "python_contracts_../x", "python_contracts_Ä"] {
        assert!(targets(&[bad.into()]).is_err());
    }
}
#[test]
fn cargo_artifact_inventory_is_confined_and_unambiguous() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let binary = root.join("forge");
    fs::write(&binary, b"fixture").unwrap();
    let row = json!({"reason":"compiler-artifact","target":{"name":"forge"},"executable":binary});
    assert_eq!(
        artifact_from_json(format!("{row}\n").as_bytes(), root, "forge", true).unwrap(),
        binary.canonicalize().unwrap()
    );
    let other = tempfile::tempdir().unwrap();
    let external = other.path().join("forge");
    fs::write(&external, b"outside").unwrap();
    let row = json!({"reason":"compiler-artifact","target":{"name":"forge"},"executable":external});
    assert!(artifact_from_json(format!("{row}\n").as_bytes(), root, "forge", true).is_err());
    assert!(artifact_from_json(b"{}\n", root, "forge", true).is_err());
}
#[test]
fn artifact_copy_is_exclusive_and_never_follows_source_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let source = root.join("source");
    let output = root.join("output");
    fs::write(&source, b"candidate").unwrap();
    let artifact = copy_artifact(&source, &output, "test").unwrap();
    assert_eq!(artifact.bytes, 9);
    assert_eq!(artifact.sha256.len(), 64);
    assert!(copy_artifact(&source, &output, "test").is_err());
    assert_eq!(fs::read(&output).unwrap(), b"candidate");
    let link = root.join("link");
    std::os::unix::fs::symlink(&source, &link).unwrap();
    assert!(copy_artifact(&link, &root.join("other"), "test").is_err());
}
#[test]
fn optional_slop_uses_legacy_mapping_when_candidate_source_is_absent() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for path in [
        "native/conductor-native/Cargo.toml",
        "native/forge/Cargo.toml",
    ] {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, b"fixture").unwrap();
    }
    let runtime = root.join("runtime");
    let python = Path::new("/usr/bin/false");
    assert!(!slop_needed(root, &runtime, python, &["python_contracts_a".into()]).unwrap());
    assert!(slop_needed(
        root,
        &runtime,
        python,
        &["python_contracts_equivalence_probe".into()]
    )
    .is_err());
    let slop = root.join("native/slop-core/Cargo.toml");
    fs::create_dir_all(slop.parent().unwrap()).unwrap();
    fs::write(slop, b"fixture").unwrap();
    assert!(slop_needed(root, &runtime, python, &["python_contracts_a".into()]).unwrap());
}
#[test]
fn slop_manifest_parent_symlink_is_refused_without_following() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = dir.path();
    for path in [
        "native/conductor-native/Cargo.toml",
        "native/forge/Cargo.toml",
    ] {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, b"fixture").unwrap();
    }
    fs::write(outside.path().join("Cargo.toml"), b"sentinel").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("native/slop-core")).unwrap();
    assert!(slop_needed(
        root,
        &root.join("runtime"),
        Path::new("/usr/bin/false"),
        &["python_contracts_a".into()]
    )
    .is_err());
    assert_eq!(
        fs::read(outside.path().join("Cargo.toml")).unwrap(),
        b"sentinel"
    );
}
