//! Missing or unsuitable executables must never select a stale fallback.
#[allow(dead_code)]
mod support;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{symlink, PermissionsExt};
use support::forge_binary;

#[test]
fn handoff_requires_explicit_absolute_utf8_path() {
    assert!(forge_binary::resolve(None)
        .unwrap_err()
        .contains("required"));
    assert!(forge_binary::resolve(Some("relative/forge".into()))
        .unwrap_err()
        .contains("absolute"));
    assert!(
        forge_binary::resolve(Some(OsString::from_vec(vec![b'/', 0xff])))
            .unwrap_err()
            .contains("UTF-8")
    );
}

#[test]
fn handoff_refuses_missing_directory_symlink_and_nonexecutable() {
    let host = support::Host::new();
    let binary = host.0.join("forge");
    let resolve = |path: &std::path::Path| forge_binary::resolve(Some(path.into()));
    assert!(resolve(&binary).is_err());
    assert!(resolve(&host.0).is_err());
    std::fs::write(&binary, b"fixture").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(resolve(&binary).is_err());
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(resolve(&binary).unwrap(), binary.to_str().unwrap());
    let link = host.0.join("linked-forge");
    symlink(&binary, &link).unwrap();
    assert!(resolve(&link).is_err());
}
