use super::*;
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt};
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn save(path: &Path, body: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(body).unwrap();
    fs::write(path, &bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    hash(&bytes)
}
#[test]
fn exact_git_binding_and_owned_profile_are_required() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("source");
    let operator = tmp.path().join("operator");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&operator).unwrap();
    fs::set_permissions(&operator, fs::Permissions::from_mode(0o700)).unwrap();
    let path = operator.join("git-profile.json");
    let git = "a".repeat(64);
    let doc = serde_json::json!({"schema_version":1,"git_sha256":git,"system_attributes":{"mode":"path","path":"/etc/gitattributes"}});
    let sha = save(&path, &doc);
    let held = Profile::open(&path, &sha, &git, &root).unwrap();
    assert!(held.verify_for(&git).is_ok());
    assert!(held.verify_for(&"b".repeat(64)).is_err());
    assert!(Profile::open(&path, &"0".repeat(64), &git, &root).is_err());
    assert!(Profile::open(&operator.join("missing.json"), &sha, &git, &root).is_err());
    fs::write(&path, b"{}").unwrap();
    assert!(held.verify().is_err());
}
#[test]
fn rejects_unpinned_or_ambiguous_system_attribute_declarations() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("source");
    let operator = tmp.path().join("operator");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&operator).unwrap();
    fs::set_permissions(&operator, fs::Permissions::from_mode(0o700)).unwrap();
    let path = operator.join("profile.json");
    let git = "a".repeat(64);
    for attrs in [
        serde_json::json!({"mode":"path","path":"../etc/gitattributes"}),
        serde_json::json!({"mode":"none","path":"/etc/gitattributes"}),
        serde_json::json!({"mode":"guessed"}),
        serde_json::Value::Null,
    ] {
        let doc =
            serde_json::json!({"schema_version":1,"git_sha256":git,"system_attributes":attrs});
        let sha = save(&path, &doc);
        assert!(
            Profile::open(&path, &sha, &git, &root).is_err(),
            "accepted invalid attributes: {attrs}"
        );
    }
    let doc = serde_json::json!({"schema_version":1,"git_sha256":git,"system_attributes":{"mode":"none"}});
    let sha = save(&path, &doc);
    let profile = Profile::open(&path, &sha, &git, &root).unwrap();
    assert!(profile.system_path().is_none());
    assert!(profile.provenance()["authority"]
        .as_str()
        .unwrap()
        .contains("operator-asserted"));
}
