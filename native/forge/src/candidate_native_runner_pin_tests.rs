use super::*;
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;

fn stamp() -> BuildStamp {
    BuildStamp {
        source_sha256: "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        git_commit: "cccccccccccccccccccccccccccccccccccccccc",
        source_matches_git: "1",
    }
}
fn fixture(bytes: &[u8]) -> (tempfile::TempDir, PathBuf, PathBuf, String) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let host = dir.path().join("host");
    std::fs::create_dir(&host).unwrap();
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("pin.json");
    std::fs::write(&path, bytes).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let hash = format!("{:x}", Sha256::digest(bytes));
    (dir, path, host, hash)
}
fn rejects(result: Result<NativeRunnerPin>, expected: &str) {
    let error = result.err().expect("invalid pin was accepted");
    assert!(format!("{error:#}").contains(expected), "{error:#}");
}
#[test]
fn clean_description_is_measured_and_dirty_or_unknown_build_is_refused() {
    let pin = describe(&stamp()).unwrap();
    assert_eq!(
        pin.executable_sha256,
        engine::running_executable().unwrap().sha256
    );
    for cleanliness in ["0", "unknown", ""] {
        assert!(describe(&BuildStamp {
            source_matches_git: cleanliness,
            ..stamp()
        })
        .is_err());
    }
    assert!(describe(&BuildStamp {
        git_commit: "unknown",
        ..stamp()
    })
    .is_err());
}
#[test]
fn pin_is_held_and_drift_is_rejected() {
    let bytes = serde_json::to_vec(&describe(&stamp()).unwrap()).unwrap();
    let (_dir, path, host, hash) = fixture(&bytes);
    let held = NativeRunnerPin::capture(&path, &hash, &[host], &stamp()).unwrap();
    held.verify().unwrap();
    std::fs::write(&path, b"changed").unwrap();
    assert!(held.verify().is_err());
}
#[test]
fn wrong_pin_fields_unknown_fields_and_excluded_host_fail() {
    let mut value = serde_json::to_value(describe(&stamp()).unwrap()).unwrap();
    value["executable_sha256"] = "a".repeat(64).into();
    let bytes = serde_json::to_vec(&value).unwrap();
    let (_dir, path, host, hash) = fixture(&bytes);
    rejects(
        NativeRunnerPin::capture(&path, &hash, std::slice::from_ref(&host), &stamp()),
        "executable differs",
    );
    value = serde_json::to_value(describe(&stamp()).unwrap()).unwrap();
    value["unknown"] = true.into();
    let bytes = serde_json::to_vec(&value).unwrap();
    let (dir, path, _host, hash) = fixture(&bytes);
    rejects(
        NativeRunnerPin::capture(&path, &hash, &[dir.path().to_path_buf()], &stamp()),
        "excluded",
    );
    rejects(
        NativeRunnerPin::capture(&path, &hash, &[dir.path().join("host")], &stamp()),
        "unknown field",
    );
}

#[test]
fn pin_rejects_source_commit_schema_and_digest_mismatches() {
    let original = serde_json::to_value(describe(&stamp()).unwrap()).unwrap();
    for (field, replacement, reason) in [
        (
            "compiled_source_sha256",
            "d".repeat(64),
            "source stamp differs",
        ),
        (
            "compiled_git_commit",
            "d".repeat(40),
            "Git revision differs",
        ),
        ("schema_version", "unknown".into(), "schema is unsupported"),
    ] {
        let mut value = original.clone();
        value[field] = replacement.into();
        let bytes = serde_json::to_vec(&value).unwrap();
        let (_dir, path, host, hash) = fixture(&bytes);
        rejects(
            NativeRunnerPin::capture(&path, &hash, &[host], &stamp()),
            reason,
        );
    }
    let bytes = serde_json::to_vec(&original).unwrap();
    let (_dir, path, host, _) = fixture(&bytes);
    assert!(NativeRunnerPin::capture(&path, &"0".repeat(64), &[host], &stamp()).is_err());
}
