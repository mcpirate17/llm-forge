use super::*;
use std::os::unix::fs::PermissionsExt;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    dir
}
fn put(path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> std::io::Result<()> {
    fs::write(path.as_ref(), bytes)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}
fn directory(path: impl AsRef<Path>) -> std::io::Result<()> {
    fs::create_dir(path.as_ref())?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}
fn sparse(path: &Path, bytes: u64) {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.set_len(bytes).unwrap();
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn programs(root: &Path) -> BTreeMap<String, PathBuf> {
    ALIASES
        .into_iter()
        .map(|a| (a.to_string(), root.join(a)))
        .collect()
}
fn setup() -> (
    tempfile::TempDir,
    Config,
    Vec<PathBuf>,
    BTreeMap<String, PathBuf>,
) {
    let dir = fixture();
    let plugin = dir.path().join("plugin");
    put(&plugin, b"plugin").unwrap();
    let programs = programs(dir.path());
    for p in programs.values() {
        put(p, b"tool").unwrap();
    }
    let gcc_install_dir = dir.path().join("gcc");
    directory(&gcc_install_dir).unwrap();
    let mut files = BTreeMap::from([(plugin.clone(), digest(b"plugin"))]);
    for name in ["crtbeginS.o", "crtendS.o", "libgcc.a"] {
        let path = gcc_install_dir.join(name);
        put(&path, b"gcc").unwrap();
        files.insert(path, digest(b"gcc"));
    }
    let config = Config {
        llvm_major: 18,
        plugin: plugin.clone(),
        gcc_install_dir,
        files,
        directories: BTreeMap::new(),
    };
    let reads = config
        .files
        .keys()
        .cloned()
        .chain(programs.values().cloned())
        .collect();
    (dir, config, reads, programs)
}
#[test]
fn exact_grants_and_pins_are_required() {
    let (dir, mut config, mut reads, programs) = setup();
    assert!(validate_paths(&config, &reads, &programs).is_ok());
    reads.push(dir.path().to_path_buf());
    assert!(validate_paths(&config, &reads, &programs).is_err());
    reads.pop();
    reads.retain(|p| p != &config.plugin);
    assert!(validate_paths(&config, &reads, &programs).is_err());
    config.llvm_major = 19;
    assert!(validate_paths(&config, &reads, &programs).is_err());
    assert!(serde_json::from_str::<Config>(
        r#"{"llvm_major":18,"plugin":"/x","gcc_install_dir":"/gcc","files":{},"directories":{},"extra":true}"#
    )
    .is_err());
}
#[test]
fn gcc_selection_requires_safe_canonical_directory_and_exact_file_pins() {
    let (dir, config, _, _) = setup();
    let held = gcc_binding(&config).unwrap();
    for name in ["crtbeginS.o", "crtendS.o", "libgcc.a"] {
        let mut missing = config.clone();
        missing.files.remove(&config.gcc_install_dir.join(name));
        assert!(gcc_binding(&missing).is_err(), "{name}");
    }
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&config.gcc_install_dir, &alias).unwrap();
    let mut aliased = config.clone();
    aliased.gcc_install_dir = alias.clone();
    for name in ["crtbeginS.o", "crtendS.o", "libgcc.a"] {
        aliased.files.insert(alias.join(name), digest(b"gcc"));
    }
    assert!(gcc_binding(&aliased).is_err());
    fs::set_permissions(&config.gcc_install_dir, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(gcc_binding(&config).is_err());
    fs::set_permissions(&config.gcc_install_dir, fs::Permissions::from_mode(0o700)).unwrap();
    fs::rename(&config.gcc_install_dir, dir.path().join("old-gcc")).unwrap();
    directory(&config.gcc_install_dir).unwrap();
    assert!(held.verify().is_err());
}
#[test]
fn file_pin_rejects_wrong_digest_mutation_and_replacement() {
    let (dir, config, _, _) = setup();
    assert!(HeldFile::capture(&config.plugin, &digest(b"wrong"), MAX_BYTES).is_err());
    let held = HeldFile::capture(&config.plugin, &digest(b"plugin"), MAX_BYTES).unwrap();
    put(&config.plugin, b"changed").unwrap();
    assert!(held.verify().is_err());
    put(&config.plugin, b"plugin").unwrap();
    let held = HeldFile::capture(&config.plugin, &digest(b"plugin"), MAX_BYTES).unwrap();
    let replacement = dir.path().join("replacement");
    put(&replacement, b"plugin").unwrap();
    fs::rename(replacement, &config.plugin).unwrap();
    assert!(held.verify().is_err());
}
#[test]
fn directory_pin_detects_added_files_and_wrong_pin() {
    let dir = fixture();
    let root = dir.path().join("inventory");
    directory(&root).unwrap();
    put(root.join("a"), b"a").unwrap();
    let pin = observe_directory(&root).unwrap();
    assert!(
        HeldDirectory::capture(&root, &digest(b"wrong"), &mut InventoryBudget::mull()).is_err()
    );
    let held = HeldDirectory::capture(&root, &pin, &mut InventoryBudget::mull()).unwrap();
    put(root.join("b"), b"b").unwrap();
    assert!(held.verify(&mut InventoryBudget::mull()).is_err());
}

#[test]
fn reserved_manifest_is_never_omitted_from_a_valid_inventory() {
    let dir = fixture();
    let root = dir.path().join("inventory");
    directory(&root).unwrap();
    let pin = observe_directory(&root).unwrap();
    let held = HeldDirectory::capture(&root, &pin, &mut InventoryBudget::mull()).unwrap();
    put(root.join("seed-manifest.json"), b"untracked").unwrap();
    assert!(observe_directory(&root).is_err());
    assert!(held.verify(&mut InventoryBudget::mull()).is_err());
    assert!(HeldDirectory::capture(&root, &pin, &mut InventoryBudget::mull()).is_err());
}

#[test]
fn replaced_inventory_root_is_rejected() {
    let dir = fixture();
    let root = dir.path().join("inventory");
    directory(&root).unwrap();
    let pin = observe_directory(&root).unwrap();
    let held = HeldDirectory::capture(&root, &pin, &mut InventoryBudget::mull()).unwrap();
    fs::rename(&root, dir.path().join("old")).unwrap();
    directory(&root).unwrap();
    assert!(held.verify(&mut InventoryBudget::mull()).is_err());
}
#[test]
fn pin_limits_and_program_aliases_are_checked_without_inode_assumptions() {
    let (dir, mut config, reads, mut programs) = setup();
    let duplicate = programs["clang"].clone();
    programs.insert("ld".into(), duplicate);
    assert!(validate_paths(&config, &reads, &programs).is_err()); // old ld grant is uncovered
    let mut adjusted = reads.clone();
    adjusted.retain(|p| p != &dir.path().join("ld"));
    assert!(validate_paths(&config, &adjusted, &programs).is_ok());
    programs.insert("extra".into(), dir.path().join("extra"));
    assert!(validate_paths(&config, &adjusted, &programs).is_err());
    for i in 0..=MAX_FILES {
        config
            .files
            .insert(dir.path().join(format!("p{i}")), digest(b"x"));
    }
    assert!(validate_paths(&config, &adjusted, &programs).is_err());
}
#[test]
fn large_file_is_rejected_before_hashing() {
    let dir = fixture();
    let path = dir.path().join("large");
    sparse(&path, MAX_FILE + 1);
    assert!(HeldFile::capture(&path, &digest(b"x"), MAX_BYTES).is_err());
}

#[test]
fn remaining_aggregate_budget_refuses_before_hashing() {
    let dir = fixture();
    let path = dir.path().join("sparse");
    sparse(&path, MAX_FILE);
    let error = HeldFile::capture(&path, &digest(b"wrong"), 1)
        .err()
        .unwrap();
    assert!(error.to_string().contains("before hashing"));
}

#[test]
fn directory_inventory_can_exceed_explicit_file_pin_limit() {
    let dir = fixture();
    let root = dir.path().join("inventory");
    directory(&root).unwrap();
    for i in 0..=MAX_FILES {
        put(root.join(format!("file-{i:04}")), b"x").unwrap();
    }
    let pin = observe_directory(&root).unwrap();
    let held = HeldDirectory::capture(&root, &pin, &mut InventoryBudget::mull()).unwrap();
    held.verify(&mut InventoryBudget::mull()).unwrap();
}
