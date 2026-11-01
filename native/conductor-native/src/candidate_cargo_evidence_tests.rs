use super::*;
use serde_json::json;
use std::os::unix::fs::{symlink, PermissionsExt};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(crate::native_id::new("cargo-evidence-test").unwrap());
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn bundle(&self, captured: u64) -> (EvidenceSpec, Vec<u8>) {
        let text = "```toml\n[advisory]\nid = \"RUSTSEC-2021-0001\"\npackage = \"example\"\ndate = \"2021-01-01\"\n[versions]\npatched = [\">= 2.0.0\"]\n```\n# Example\n\nDescription.\n";
        let bytes = serde_json::to_vec(&json!({"schema_version":1,"captured_at":captured,"advisory_commit":"a".repeat(40),"advisories":[{"path":"crates/example/RUSTSEC-2021-0001.md","text":text}],"registry":[]})).unwrap();
        let path = self.0.join("bundle.json");
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        (
            EvidenceSpec {
                path,
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                max_age_seconds: 60,
            },
            bytes,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn digest_and_age_boundaries_are_fail_closed() {
    let f = Fixture::new();
    let (mut spec, _) = f.bundle(1000);
    let verified = load(&spec, &[], 1060).unwrap();
    assert_eq!(verified.checked_at(), 1060);
    assert_eq!(verified.digest(), spec.sha256);
    assert!(load(&spec, &[], 1061).is_err());
    assert!(load(&spec, &[], 999).is_err());
    spec.max_age_seconds = 0;
    assert!(load(&spec, &[], 1000).is_err());
    spec.max_age_seconds = 31 * 86400 + 1;
    assert!(load(&spec, &[], 1000).is_err());
    spec.max_age_seconds = 31 * 86400;
    assert!(load(&spec, &[], 1000 + 31 * 86400).is_ok());
    spec.sha256 = "f".repeat(64);
    assert!(load(&spec, &[], 1000).is_err());
    spec.sha256 = "A".repeat(64);
    assert!(load(&spec, &[], 1000).is_err());
}

#[test]
fn rejects_links_permissions_and_source_containment() {
    let f = Fixture::new();
    let (spec, _) = f.bundle(1000);
    assert!(load(&spec, &[&f.0], 1000).is_err());
    fs::set_permissions(&spec.path, fs::Permissions::from_mode(0o620)).unwrap();
    assert!(load(&spec, &[], 1000).is_err());
    fs::set_permissions(&spec.path, fs::Permissions::from_mode(0o600)).unwrap();
    let hard = f.0.join("hard");
    fs::hard_link(&spec.path, &hard).unwrap();
    assert!(load(&spec, &[], 1000).is_err());
    fs::remove_file(hard).unwrap();
    let link = f.0.join("link");
    symlink(&spec.path, &link).unwrap();
    let mut modified = spec.clone();
    modified.path = link;
    assert!(load(&modified, &[], 1000).is_err());
    let dirlink = f.0.join("ancestor");
    symlink(&f.0, &dirlink).unwrap();
    modified.path = dirlink.join("bundle.json");
    assert!(load(&modified, &[], 1000).is_err());
}

#[test]
fn strict_spec_and_bundle_integrity() {
    let f = Fixture::new();
    let (spec, bytes) = f.bundle(1000);
    let path = f.0.join("spec.json");
    fs::write(&path, serde_json::to_vec(&spec).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(read_spec(&path, &[]).is_ok());
    fs::write(
        &path,
        br#"{"path":"/x","sha256":"x","max_age_seconds":1,"unexpected":true}"#,
    )
    .unwrap();
    assert!(read_spec(&path, &[]).is_err());
    fs::write(&spec.path, &bytes[..bytes.len() - 1]).unwrap();
    assert!(load(&spec, &[], 1000).is_err());
}

#[test]
fn rejects_foreign_owned_file_when_system_fixture_is_available() {
    let path = Path::new("/etc/passwd");
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.is_file() && meta.uid() != unsafe { libc::geteuid() } {
            assert!(read_owned(path, &[], 1024 * 1024).is_err());
        }
    }
}
