//! Real-Git fixtures require a build profile established by the test operator.
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
pub(crate) fn profile(parent: &Path, git_sha256: &str) -> (PathBuf, String) {
    let directory = parent.join("git-build-profile");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let system=std::env::var("FORGE_TEST_GIT_SYSTEM_ATTRIBUTES")
        .expect("set FORGE_TEST_GIT_SYSTEM_ATTRIBUTES to the observed absolute path, or 'none' for a build without system attributes");
    let attributes = if system == "none" {
        serde_json::json!({"mode":"none"})
    } else {
        assert!(
            Path::new(&system).is_absolute(),
            "test Git system attributes path must be absolute"
        );
        serde_json::json!({"mode":"path","path":system})
    };
    let value = serde_json::json!({"schema_version":1,"git_sha256":git_sha256,"system_attributes":attributes});
    let data = serde_json::to_vec(&value).unwrap();
    let path = directory.join("profile.json");
    fs::write(&path, &data).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    (path, format!("{:x}", Sha256::digest(data)))
}
