use super::*;
use crate::candidate_cargo_evidence::{self, EvidenceSpec};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    root: PathBuf,
    snapshot: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(crate::native_id::new("cargo-run-test").unwrap());
        let snapshot = root.join("snapshot");
        fs::create_dir_all(&snapshot).unwrap();
        Self { root, snapshot }
    }
    fn evidence(&self) -> VerifiedEvidence {
        let advisory = "```toml\n[advisory]\nid = \"RUSTSEC-2021-0001\"\npackage = \"example\"\ndate = \"2021-01-01\"\n[versions]\npatched = [\">= 2.0.0\"]\n```\n# Example\n\nDescription.\n";
        let bytes = serde_json::to_vec(&json!({"schema_version":1,"captured_at":1000,"advisory_commit":"a".repeat(40),
            "advisories":[{"path":"crates/example/RUSTSEC-2021-0001.md","text":advisory}],
            "registry":[{"name":"example","version":"1.0.0","checksum":"1".repeat(64),"yanked":true},{"name":"example","version":"2.0.0","checksum":"2".repeat(64),"yanked":false}]})).unwrap();
        let path = self.root.join("evidence.json");
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let spec = EvidenceSpec {
            path,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            max_age_seconds: 60,
        };
        candidate_cargo_evidence::load(&spec, &[&self.snapshot], 1000).unwrap()
    }
    fn add(&self, dir: &str, version: &str, checksum: &str) {
        let directory = self.snapshot.join(dir);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("source.rs"), b"fn example() {}\n").unwrap();
        fs::write(directory.join("Cargo.lock"), format!("version = 3\n[[package]]\nname = \"example\"\nversion = \"{version}\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{checksum}\"\n")).unwrap();
    }
    fn dir(&self) -> Dir {
        Dir::open_ambient_dir(&self.snapshot, cap_std::ambient_authority()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn sorted_owners_vulnerable_clean_and_evidence_metadata() {
    let f = Fixture::new();
    f.add("a", "1.0.0", &"1".repeat(64));
    f.add("b", "2.0.0", &"2".repeat(64));
    let evidence = f.evidence();
    let result = evaluate(&f.dir(), &["b/source.rs", "a/source.rs"], &evidence, || {
        Ok(())
    })
    .unwrap();
    assert_eq!(
        result["metrics"]["owner_plan"]["lockfiles"],
        json!(["a/Cargo.lock", "b/Cargo.lock"])
    );
    assert_eq!(result["metrics"]["evidence_sha256"], evidence.digest());
    assert_eq!(result["metrics"]["captured_at"], 1000);
    let findings = result["findings"].as_array().unwrap();
    assert!(findings
        .iter()
        .any(|f| f["rule_id"] == "vulnerable-dependency" && f["severity"] == "critical"));
    assert!(findings
        .iter()
        .any(|f| f["rule_id"] == "yanked-dependency" && f["severity"] == "medium"));
    assert!(findings.iter().all(|f| f["path"] == "a/Cargo.lock"));
}

#[test]
fn missing_registry_and_cancellation_never_return_partial_success() {
    let f = Fixture::new();
    f.add("a", "3.0.0", &"3".repeat(64));
    let evidence = f.evidence();
    assert!(evaluate(&f.dir(), &["a/source.rs"], &evidence, || Ok(())).is_err());
    let mut calls = 0;
    assert!(evaluate(&f.dir(), &["a/source.rs"], &evidence, || {
        calls += 1;
        if calls == 2 {
            Err("cancelled".into())
        } else {
            Ok(())
        }
    })
    .is_err());
    assert_eq!(calls, 2);
}
