use super::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::os::unix::fs::{symlink, PermissionsExt};

#[test]
fn confined_memory_reports_aggregate_and_refuses_explicit_per_process_limit() {
    let resources = cgroup::Limits {
        cpu_millis: 1000,
        memory_mib: 1024,
        processes: 32,
    };
    let legacy = crate::mutation_process::MemoryCap {
        limit: 8 * 1024 * 1024 * 1024,
        hard: 8 * 1024 * 1024 * 1024,
    };
    let report = memory_enforcement(&resources, Some(legacy), false).unwrap();
    assert_eq!(report["mode"], "aggregate_cgroup");
    assert_eq!(report["bytes"], 1024_u64 * 1024 * 1024);
    assert_eq!(
        report["legacy_requested_per_process_data_bytes"],
        legacy.limit
    );
    assert!(report["scope"]
        .as_str()
        .unwrap()
        .contains("not per-process"));
    assert!(memory_enforcement(&resources, Some(legacy), true)
        .unwrap_err()
        .to_string()
        .contains("MUTATION_MEMORY_CAP_GIB"));
    let disabled = memory_enforcement(&resources, None, true).unwrap();
    assert!(disabled["legacy_requested_per_process_data_bytes"].is_null());
    assert_eq!(disabled["bytes"], report["bytes"]);
}

struct Fixture {
    root: tempfile::TempDir,
    document: Value,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["repo", "repo/.git", "tools", "scratch", "cgroup"] {
            fs::create_dir(root.path().join(name)).unwrap();
            fs::set_permissions(root.path().join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let tool = root.path().join("tools/cargo");
        fs::write(&tool, b"fixture executable; never launched").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let hash = format!("{:x}", Sha256::digest(fs::read(&tool).unwrap()));
        let document = json!({"version":1,"source_root":root.path().join("repo"),
            "scratch_parent":root.path().join("scratch"),"cgroup_parent":root.path().join("cgroup"),
            "read_only":[root.path().join("tools")],"programs":{"cargo":{"path":tool,"sha256":hash}},
            "resources":{"cpu_millis":1000,"memory_mib":128,"processes":32},"max_wall_seconds":30});
        Self { root, document }
    }
    fn path(&self, path: &str) -> PathBuf {
        self.root.path().join(path)
    }
    fn write(&self, bytes: &[u8]) -> String {
        fs::write(self.path("grants.json"), bytes).unwrap();
        fs::set_permissions(self.path("grants.json"), fs::Permissions::from_mode(0o600)).unwrap();
        format!("{:x}", Sha256::digest(bytes))
    }
    fn load(&self) -> Result<Grants> {
        let bytes = serde_json::to_vec(&self.document).unwrap();
        let pin = self.write(&bytes);
        self.load_pin(&pin)
    }
    fn load_pin(&self, pin: &str) -> Result<Grants> {
        Grants::load(
            &self.path("grants.json"),
            pin,
            &self.path("repo"),
            &self.path("repo/.git"),
            &self.path("repo/.git"),
        )
    }
}
#[test]
fn exact_alias_resolution_and_reverification() {
    let f = Fixture::new();
    let grants = f.load().unwrap();
    assert_eq!(grants.program("cargo").unwrap(), f.path("tools/cargo"));
    assert!(grants.program("/bin/sh").is_err());
    assert!(grants.program("Cargo").is_err());
    assert_eq!(grants.programs().len(), 1);
    assert_eq!(grants.digest().len(), 64);
    assert!(grants.cargo_workspace.is_none());
    assert_eq!(
        grants.program_sha256("cargo").unwrap(),
        f.document["programs"]["cargo"]["sha256"]
    );
    grants.verify().unwrap();
    fs::write(f.path("tools/cargo"), b"different executable").unwrap();
    assert!(grants.verify().is_err());
}
#[test]
fn seed_descriptor_selects_only_a_repository_lockfile_and_keeps_existing_grants() {
    let mut f = Fixture::new();
    f.document["cargo_workspace"] = json!({"lockfile":"native/forge/Cargo.lock",
        "seed":{"root":f.path("seed"),"manifest_path":f.path("seed/seed-manifest.json"),
            "expected_sha256":"a".repeat(64)}});
    let grants = f.load().unwrap();
    assert_eq!(
        grants.cargo_workspace.as_ref().unwrap().lockfile,
        "native/forge/Cargo.lock"
    );
    // Actual seed bytes are admitted separately against this held control and lockfile.
    assert_eq!(grants.programs().len(), 1);
    assert_eq!(grants.read_only, vec![f.path("tools")]);
    assert!(grants.program("git").is_err());
    for path in [
        "../Cargo.lock",
        "/Cargo.lock",
        ".git/Cargo.lock",
        "native/Cargo.toml",
        "a//Cargo.lock",
    ] {
        f.document["cargo_workspace"]["lockfile"] = json!(path);
        assert!(f.load().is_err(), "accepted {path}");
    }
}
#[test]
fn control_drift_wrong_pin_and_replacement_refuse() {
    let f = Fixture::new();
    let grants = f.load().unwrap();
    assert!(f.load_pin(&"0".repeat(64)).is_err());
    fs::write(f.path("grants.json"), b"{}").unwrap();
    assert!(grants.verify().is_err());
    let f = Fixture::new();
    let grants = f.load().unwrap();
    fs::rename(f.path("tools/cargo"), f.path("tools/old")).unwrap();
    fs::copy(f.path("tools/old"), f.path("tools/cargo")).unwrap();
    assert!(grants.verify().is_err());
}
#[test]
fn strict_schema_rejects_unknown_duplicate_and_invalid_budgets() {
    let mut f = Fixture::new();
    f.document["environment"] = json!({});
    assert!(f.load().is_err());
    f.document.as_object_mut().unwrap().remove("environment");
    f.document["resources"]["processes"] = json!(0);
    assert!(f.load().is_err());
    f.document["resources"]["processes"] = json!(32);
    let original = serde_json::to_string(&f.document).unwrap();
    let duplicate = original.replacen("{", "{\"version\":1,", 1);
    let pin = f.write(duplicate.as_bytes());
    assert!(f.load_pin(&pin).is_err());
}
#[test]
fn program_requires_granted_root_safe_mode_and_unique_inode() {
    let mut f = Fixture::new();
    fs::create_dir(f.path("other-tools")).unwrap();
    fs::set_permissions(f.path("other-tools"), fs::Permissions::from_mode(0o700)).unwrap();
    f.document["read_only"] = json!([f.path("other-tools")]);
    assert!(f
        .load()
        .err()
        .unwrap()
        .to_string()
        .contains("program lacks explicit read grant"));
    f.document["read_only"] = json!([f.path("tools")]);
    fs::set_permissions(f.path("tools/cargo"), fs::Permissions::from_mode(0o775)).unwrap();
    assert!(f.load().is_err());
    fs::set_permissions(f.path("tools/cargo"), fs::Permissions::from_mode(0o755)).unwrap();
    f.document["programs"]["other"] = f.document["programs"]["cargo"].clone();
    assert!(f.load().is_err());
    f.document["programs"]
        .as_object_mut()
        .unwrap()
        .remove("other");
    fs::hard_link(f.path("tools/cargo"), f.path("tools/alias")).unwrap();
    assert!(f.load().is_err());
}
#[test]
fn symlinked_program_control_and_directory_refuse() {
    let mut f = Fixture::new();
    symlink(f.path("tools/cargo"), f.path("tools/link")).unwrap();
    f.document["programs"]["cargo"]["path"] = json!(f.path("tools/link"));
    assert!(f.load().is_err());
    let f = Fixture::new();
    let bytes = serde_json::to_vec(&f.document).unwrap();
    let pin = f.write(&bytes);
    fs::rename(f.path("grants.json"), f.path("actual.json")).unwrap();
    symlink(f.path("actual.json"), f.path("grants.json")).unwrap();
    assert!(f.load_pin(&pin).is_err());
    let mut f = Fixture::new();
    symlink(f.path("tools"), f.path("linked-tools")).unwrap();
    f.document["read_only"] = json!([f.path("linked-tools")]);
    assert!(f.load().is_err());
}
#[test]
fn broad_host_git_scratch_and_credential_read_grants_refuse() {
    let mut f = Fixture::new();
    for root in [
        f.path("repo"),
        f.path("repo/.git"),
        f.path("scratch"),
        f.path("cgroup"),
        f.root.path().to_path_buf(),
        PathBuf::from("/"),
        PathBuf::from(std::env::var_os("HOME").unwrap()),
    ] {
        f.document["read_only"] = json!([root]);
        assert!(f.load().is_err(), "accepted {}", root.display());
    }
    let (home, protected) = protected_roots().unwrap();
    assert!(protected.contains(&home.join(".cargo")));
    assert!(protected.contains(&home.join(".ssh")));
}
#[test]
fn root_validation_requires_private_child_and_rechecks_grants() {
    let f = Fixture::new();
    let grants = f.load().unwrap();
    let child = f.path("scratch/private");
    fs::create_dir(&child).unwrap();
    fs::set_permissions(&child, fs::Permissions::from_mode(0o700)).unwrap();
    grants.validate_private_root(&child).unwrap();
    assert!(grants.validate_private_root(&f.path("scratch")).is_err());
    assert!(grants.validate_private_root(&f.path("repo")).is_err());
    fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(grants.validate_private_root(&child).is_err());
    fs::set_permissions(&child, fs::Permissions::from_mode(0o700)).unwrap();
    fs::rename(f.path("tools"), f.path("old-tools")).unwrap();
    fs::create_dir(f.path("tools")).unwrap();
    assert!(grants.validate_private_root(&child).is_err());
}

#[test]
fn external_worktree_git_directory_is_excluded() {
    let mut f = Fixture::new();
    fs::create_dir(f.path("external-git")).unwrap();
    fs::set_permissions(f.path("external-git"), fs::Permissions::from_mode(0o700)).unwrap();
    f.document["read_only"] = json!([f.path("external-git")]);
    let bytes = serde_json::to_vec(&f.document).unwrap();
    let pin = f.write(&bytes);
    let result = Grants::load(
        &f.path("grants.json"),
        &pin,
        &f.path("repo"),
        &f.path("repo/.git"),
        &f.path("external-git"),
    );
    assert!(result.err().unwrap().to_string().contains("paths overlap"));
}
#[test]
fn final_verification_rejects_changed_credential_selection() {
    let f = Fixture::new();
    let mut grants = f.load().unwrap();
    grants.environment.1 = Some("different-operator-cargo-home".into());
    assert!(grants
        .verify()
        .err()
        .unwrap()
        .to_string()
        .contains("selection changed"));
    grants.environment = environment_selection();
    grants.protected.push(f.path("tools"));
    assert!(grants
        .verify()
        .err()
        .unwrap()
        .to_string()
        .contains("credential-root selection changed"));
}

#[test]
fn exact_regular_file_grants_are_held_without_granting_their_parent() {
    let mut f = Fixture::new();
    fs::create_dir(f.path("system")).unwrap();
    fs::set_permissions(f.path("system"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(f.path("system/ld.so.cache"), b"fixture linker cache").unwrap();
    fs::set_permissions(
        f.path("system/ld.so.cache"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    f.document["read_only"] = json!([f.path("tools"), f.path("system/ld.so.cache")]);
    let grants = f.load().unwrap();
    assert_eq!(grants.read_files.len(), 1);
    assert!(!grants.read_directories.contains(&f.path("system")));
    assert!(grants.allows_program(&f.path("system/ld.so.cache")));
    assert!(!grants.allows_program(&f.path("system/ld.so.cache/child")));
    assert!(!grants.allows_program(&f.path("system/sibling")));
    grants.verify().unwrap();
    fs::write(f.path("system/ld.so.cache"), b"changed linker cache").unwrap();
    assert!(grants.verify().is_err());
}
#[test]
fn read_file_symlink_hardlink_unsafe_mode_and_oversize_refuse() {
    let mut f = Fixture::new();
    fs::write(f.path("cache"), b"cache").unwrap();
    symlink(f.path("cache"), f.path("cache-link")).unwrap();
    f.document["read_only"] = json!([f.path("tools"), f.path("cache-link")]);
    assert!(f.load().is_err());
    f.document["read_only"] = json!([f.path("tools"), f.path("cache")]);
    fs::set_permissions(f.path("cache"), fs::Permissions::from_mode(0o664)).unwrap();
    assert!(f.load().is_err());
    fs::set_permissions(f.path("cache"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::hard_link(f.path("cache"), f.path("cache-hardlink")).unwrap();
    assert!(f.load().is_err());
    fs::remove_file(f.path("cache-hardlink")).unwrap();
    File::options()
        .write(true)
        .open(f.path("cache"))
        .unwrap()
        .set_len(MAX_READ_FILE_BYTES as u64 + 1)
        .unwrap();
    assert!(f.load().is_err());
}
#[test]
fn a_file_grant_can_bind_a_single_executable_and_detect_replacement() {
    let mut f = Fixture::new();
    f.document["read_only"] = json!([f.path("tools/cargo")]);
    let grants = f.load().unwrap();
    assert!(grants.read_directories.is_empty());
    assert!(grants.allows_program(&f.path("tools/cargo")));
    assert!(!grants.allows_program(&f.path("tools/cargo/child")));
    fs::rename(f.path("tools/cargo"), f.path("tools/old")).unwrap();
    fs::copy(f.path("tools/old"), f.path("tools/cargo")).unwrap();
    assert!(grants.verify().is_err());
}

#[test]
fn report_binding_avoids_git_config_parsing_and_refuses_linked_metadata() {
    let f = Fixture::new();
    fs::write(f.path("repo/.git/config"), b"invalid [ git configuration").unwrap();
    let reads = vec![f.path("tools")];
    let hash = f.document["programs"]["cargo"]["sha256"].as_str().unwrap();
    let held = OutboundBindings::capture_report(
        &f.path("tools/cargo"),
        hash,
        &reads,
        &f.path("repo"),
        &f.path("scratch"),
        &reads,
    )
    .unwrap();
    held.verify().unwrap();
    fs::write(f.path("tools/cargo"), b"changed executable").unwrap();
    assert!(held.verify().is_err());
    fs::write(f.path("tools/cargo"), b"fixture executable; never launched").unwrap();
    let held = OutboundBindings::capture_report(
        &f.path("tools/cargo"),
        hash,
        &reads,
        &f.path("repo"),
        &f.path("scratch"),
        &reads,
    )
    .unwrap();
    fs::remove_dir_all(f.path("repo/.git")).unwrap();
    fs::write(f.path("repo/.git"), b"gitdir: ../elsewhere\n").unwrap();
    assert!(held.verify().is_err());
    assert!(OutboundBindings::capture_report(
        &f.path("tools/cargo"),
        hash,
        &reads,
        &f.path("repo"),
        &f.path("scratch"),
        &reads
    )
    .is_err());
    fs::remove_file(f.path("repo/.git")).unwrap();
    symlink(f.path("tools"), f.path("repo/.git")).unwrap();
    assert!(OutboundBindings::capture_report(
        &f.path("tools/cargo"),
        hash,
        &reads,
        &f.path("repo"),
        &f.path("scratch"),
        &reads
    )
    .is_err());
}

#[test]
fn report_data_directories_allow_group_write_but_parent_and_identity_remain_strict() {
    let f = Fixture::new();
    fs::write(f.path("repo/.git/config"), b"invalid [ git configuration").unwrap();
    for path in ["repo", "repo/.git"] {
        fs::set_permissions(f.path(path), fs::Permissions::from_mode(0o775)).unwrap();
    }
    let reads = vec![f.path("tools")];
    let hash = f.document["programs"]["cargo"]["sha256"].as_str().unwrap();
    let capture = || {
        OutboundBindings::capture_report(
            &f.path("tools/cargo"),
            hash,
            &reads,
            &f.path("repo"),
            &f.path("scratch"),
            &reads,
        )
    };
    let held = capture().unwrap();
    held.verify().unwrap();
    fs::set_permissions(f.path("repo/.git"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        held.verify().is_err(),
        "Git data directory mode drift must refuse"
    );
    let held = capture().unwrap();
    fs::set_permissions(f.root.path(), fs::Permissions::from_mode(0o775)).unwrap();
    assert!(
        capture().is_err(),
        "group-writable source parent must be refused"
    );
    assert!(
        held.verify().is_err(),
        "source ancestor drift must be refused"
    );
    fs::set_permissions(f.root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let held = capture().unwrap();
    fs::rename(f.path("repo"), f.path("old-repo")).unwrap();
    fs::create_dir(f.path("repo")).unwrap();
    fs::create_dir(f.path("repo/.git")).unwrap();
    assert!(
        held.verify().is_err(),
        "replacement source root must be refused"
    );
}
