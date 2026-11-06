use super::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::os::unix::fs::{symlink, PermissionsExt};

struct Fixture {
    root: tempfile::TempDir,
    document: Value,
    entries: Vec<InventoryEntry>,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn directory(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn write(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}
fn entry(path: &str, bytes: Option<&[u8]>, executable: bool) -> InventoryEntry {
    InventoryEntry {
        path: path.into(),
        directory: bytes.is_none(),
        bytes: bytes.map_or(0, |b| b.len() as u64),
        sha256: bytes.map(hash),
        executable,
    }
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        for path in [
            "repo",
            "repo/.git",
            "scratch",
            "cgroup",
            "guest",
            "guest/bin",
        ] {
            directory(&root.path().join(path));
        }
        write(
            &root.path().join("guest/bin/python"),
            b"python fixture; never run",
            0o700,
        );
        write(
            &root.path().join("guest/bin/fest"),
            b"fest fixture; never run",
            0o700,
        );
        write(
            &root.path().join("loader"),
            b"external loader fixture",
            0o600,
        );
        let entries = vec![
            entry("bin", None, false),
            entry("bin/fest", Some(b"fest fixture; never run"), true),
            entry("bin/python", Some(b"python fixture; never run"), true),
        ];
        let guest = root.path().join("guest");
        let loader = root.path().join("loader");
        let document = json!({"version":1,"source_root":root.path().join("repo"),
            "scratch_parent":root.path().join("scratch"),"cgroup_parent":root.path().join("cgroup"),
            "read_only":[guest,loader],"programs":{
                "python":{"path":guest.join("bin/python"),"sha256":hash(b"python fixture; never run")},
                "fest":{"path":guest.join("bin/fest"),"sha256":hash(b"fest fixture; never run")}},
            "resources":{"cpu_millis":1000,"memory_mib":128,"processes":32},"max_wall_seconds":30,
            "python_guest":{"root":guest,"manifest_path":root.path().join("manifest.json"),
                "manifest_sha256":"", "external_files":{(loader.to_str().unwrap()):hash(b"external loader fixture")}}});
        let mut result = Self {
            root,
            document,
            entries,
        };
        result.manifest();
        result
    }
    fn path(&self, path: &str) -> PathBuf {
        self.root.path().join(path)
    }
    fn manifest(&mut self) {
        let bytes =
            serde_json::to_vec(&json!({"schema_version":1,"entries":self.entries})).unwrap();
        write(&self.path("manifest.json"), &bytes, 0o600);
        self.document["python_guest"]["manifest_sha256"] = json!(hash(&bytes));
    }
    fn load(&self) -> Result<Grants> {
        let bytes = serde_json::to_vec(&self.document).unwrap();
        write(&self.path("grants.json"), &bytes, 0o600);
        Grants::load(
            &self.path("grants.json"),
            &hash(&bytes),
            &self.path("repo"),
            &self.path("repo/.git"),
            &self.path("repo/.git"),
        )
    }
    fn rejects(&self, reason: &str) {
        let error = self.load().err().expect("guest must refuse");
        assert!(format!("{error:#}").contains(reason), "{error:#}");
    }
}

#[test]
fn guest_admission_binds_exact_tree_control_and_external_file_contents() {
    for changed in ["guest/bin/python", "loader", "manifest.json"] {
        let f = Fixture::new();
        let grants = f.load().unwrap();
        grants.fest_guest().unwrap().verify().unwrap();
        write(&f.path(changed), b"changed", 0o700);
        assert!(grants.verify().is_err(), "{changed}");
    }
    let f = Fixture::new();
    let grants = f.load().unwrap();
    write(&f.path("guest/unexpected"), b"extra", 0o600);
    assert!(grants.fest_guest().unwrap().verify().is_err());
    fs::remove_file(f.path("guest/unexpected")).unwrap();
    fs::remove_file(f.path("guest/bin/fest")).unwrap();
    assert!(grants.fest_guest().unwrap().verify().is_err());
}

#[test]
fn guest_requires_exact_external_pin_set_and_matching_manifest_pin() {
    let mut f = Fixture::new();
    f.document["python_guest"]["external_files"] = json!({});
    f.rejects("pins must match read grants exactly");
    let mut f = Fixture::new();
    let extra = f.path("not-granted");
    f.document["python_guest"]["external_files"][extra.to_str().unwrap()] = json!("a".repeat(64));
    f.rejects("pins must match read grants exactly");
    let mut f = Fixture::new();
    let loader = f.path("loader");
    f.document["python_guest"]["external_files"][loader.to_str().unwrap()] = json!("a".repeat(64));
    assert!(f.load().is_err());
    let mut f = Fixture::new();
    f.document["python_guest"]["manifest_sha256"] = json!("a".repeat(64));
    assert!(f.load().is_err());
    let mut f = Fixture::new();
    f.entries[1].sha256 = Some("a".repeat(64));
    f.manifest();
    f.rejects("guest inventory changed");
}

#[test]
fn guest_rejects_declared_startup_hooks_and_old_bytecode() {
    for name in [
        "injected.pth",
        "sitecustomize.py",
        "usercustomize.py",
        "pyvenv.cfg",
        "old.pyc",
        "old.pyo",
        "seed-manifest.json",
    ] {
        let mut f = Fixture::new();
        write(
            &f.path(&format!("guest/{name}")),
            b"host startup input",
            0o600,
        );
        f.entries
            .push(entry(name, Some(b"host startup input"), false));
        f.entries.sort_by(|a, b| a.path.cmp(&b.path));
        f.manifest();
        f.rejects("startup hook or bytecode cache");
    }
    let mut f = Fixture::new();
    directory(&f.path("guest/__pycache__"));
    f.entries.insert(0, entry("__pycache__", None, false));
    f.manifest();
    f.rejects("startup hook or bytecode cache");
}

#[test]
fn guest_tree_refuses_aliases_unsafe_modes_and_unmanifested_reserved_input() {
    let f = Fixture::new();
    symlink("bin/python", f.path("guest/alias")).unwrap();
    assert!(f.load().is_err());
    let f = Fixture::new();
    fs::hard_link(f.path("guest/bin/python"), f.path("guest/alias")).unwrap();
    assert!(f.load().is_err());
    let f = Fixture::new();
    fs::set_permissions(f.path("guest"), fs::Permissions::from_mode(0o755)).unwrap();
    f.rejects("owned private directory");
    let f = Fixture::new();
    fs::set_permissions(
        f.path("guest/bin/python"),
        fs::Permissions::from_mode(0o720),
    )
    .unwrap();
    assert!(f.load().is_err());
    let f = Fixture::new();
    write(&f.path("guest/seed-manifest.json"), b"{}", 0o600);
    f.rejects("reserved seed manifest path");
}

#[test]
fn guest_manifest_refuses_unsorted_duplicate_and_oversize_declarations() {
    let mut f = Fixture::new();
    f.entries.swap(1, 2);
    f.manifest();
    f.rejects("uniquely sorted");
    let mut f = Fixture::new();
    f.entries.push(f.entries[2].clone());
    f.manifest();
    f.rejects("uniquely sorted");
    let mut f = Fixture::new();
    f.entries[1].bytes = 32 * 1024 * 1024 + 1;
    f.manifest();
    f.rejects("file exceeds bound");
    let entries: Vec<_> = (0..9)
        .map(|n| InventoryEntry {
            bytes: 32 * 1024 * 1024,
            ..entry(&format!("file{n}"), Some(b""), false)
        })
        .collect();
    assert!(validate_entries(&entries)
        .unwrap_err()
        .to_string()
        .contains("total byte bound"));
    let too_many: Vec<_> = (0..10_001)
        .map(|n| entry(&format!("f{n:05}"), Some(b""), false))
        .collect();
    assert!(validate_entries(&too_many)
        .unwrap_err()
        .to_string()
        .contains("entry count"));
}
