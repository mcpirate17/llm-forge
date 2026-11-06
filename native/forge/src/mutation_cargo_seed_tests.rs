use super::*;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
struct Fixture {
    _root: tempfile::TempDir,
    cache: PathBuf,
    lock: PathBuf,
    seed: PathBuf,
}
fn directory(path: &Path) {
    if path.exists() {
        return;
    }
    if let Some(parent) = path.parent() {
        directory(parent);
    }
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn write(path: &Path, bytes: &[u8]) {
    directory(path.parent().unwrap());
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn public_write(path: &Path, bytes: &[u8]) {
    write(path, bytes);
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
}
fn source_body(i: usize) -> Vec<u8> {
    format!("pub fn value() -> usize {{ {i} }}\n").into_bytes()
}
fn crate_archive(name: &str, body: &[u8]) -> Vec<u8> {
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut archive = tar::Builder::new(encoder);
    let manifest = format!("[package]\nname={name:?}\nversion=\"0.1.0\"\nedition=\"2021\"\n");
    for (path, bytes) in [("Cargo.toml", manifest.as_bytes()), ("src/lib.rs", body)] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, format!("{name}-0.1.0/{path}"), bytes)
            .unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap()
}
fn materialize(home: &Path, name: &str, body: &[u8]) {
    let package = home.join(format!("registry/src/crates-io/{name}-0.1.0"));
    write(
        &package.join("Cargo.toml"),
        format!("[package]\nname={name:?}\nversion=\"0.1.0\"\nedition=\"2021\"\n").as_bytes(),
    );
    write(&package.join("src/lib.rs"), body);
    write(&package.join(".cargo-ok"), b"{\"v\":1}");
}
impl Fixture {
    fn new(count: usize) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let cache = root.path().join("cache");
        directory(&cache);
        let lock = root.path().join("Cargo.lock");
        let seed = root.path().join("seed");
        let mut text = "version=4\n".to_string();
        for i in 0..count {
            let bytes = crate_archive(&format!("p{i:03}"), &source_body(i));
            let name = format!("p{i:03}");
            public_write(
                &cache.join(format!("registry/cache/crates-io/{name}-0.1.0.crate")),
                &bytes,
            );
            public_write(
                &cache.join(format!(
                    "registry/index/crates-io/.cache/{}",
                    index_path(&name)
                )),
                b"opaque index bytes\0\x01",
            );
            text.push_str(&format!("[[package]]\nname={name:?}\nversion=\"0.1.0\"\nsource={REGISTRY:?}\nchecksum={:?}\n",sha(&bytes)));
        }
        write(&lock, text.as_bytes());
        public_write(
            &cache.join("registry/index/crates-io/config.json"),
            b"{\"dl\":\"https://example.invalid/crates\"}",
        );

        Self {
            _root: root,
            cache,
            lock,
            seed,
        }
    }
    fn provision(&self) -> Result<Provisioned> {
        provision(&ProvisionRequest {
            cache_root: &self.cache,
            lock_path: &self.lock,
            expected_lock_sha256: &sha(&fs::read(&self.lock)?),
            registry_directory: "crates-io",
            cargo_sha256: &"c".repeat(64),
            destination: &self.seed,
        })
    }
    fn held(&self, provisioned: &Provisioned) -> HeldSeed {
        HeldSeed::capture(
            &provisioned.config,
            &fs::read(&self.lock).unwrap(),
            &"c".repeat(64),
        )
        .unwrap()
    }
}
#[test]
fn provisions_all_331_locked_archives_and_stages_independent_bytes() {
    let fixture = Fixture::new(331);
    let provisioned = fixture.provision().unwrap();
    assert_eq!(provisioned.identity.files, 663); // 331 archives, 331 index entries, config.
    assert_eq!(provisioned.identity.expanded_files, 662);
    let held = fixture.held(&provisioned);
    assert_eq!(held.identity(), provisioned.identity);
    let destination = fixture._root.path().join("private-home");
    let staged = held.stage(&destination).unwrap();
    assert_eq!(staged.identity(), held.identity());
    staged.verify().unwrap();
    let original = fixture
        .cache
        .join("registry/cache/crates-io/p000-0.1.0.crate");
    let copied = destination.join("registry/cache/crates-io/p000-0.1.0.crate");
    assert_ne!(
        fs::metadata(original).unwrap().ino(),
        fs::metadata(&copied).unwrap().ino()
    );
    assert_eq!(
        fs::read(destination.join("registry/src/crates-io/p000-0.1.0/src/lib.rs")).unwrap(),
        source_body(0)
    );
    write(&destination.join(".package-cache"), b"");
    staged.verify().unwrap();
    write(&copied, b"changed admitted cache");
    assert!(staged.verify().is_err());
}
#[test]
fn archive_checksum_missing_and_unsupported_sources_refuse_without_passing_manifest() {
    for change in ["checksum", "missing", "git"] {
        let f = Fixture::new(1);
        let archive = f.cache.join("registry/cache/crates-io/p000-0.1.0.crate");
        match change {
            "checksum" => public_write(&archive, b"corrupt archive"),
            "missing" => fs::remove_file(archive).unwrap(),
            _ => {
                let old = fs::read_to_string(&f.lock).unwrap();
                write(
                    &f.lock,
                    old.replace(REGISTRY, "git+https://example.invalid/repo")
                        .as_bytes(),
                );
            }
        }
        assert!(f.provision().is_err(), "{change}");
        assert!(!f.seed.join(MANIFEST).exists());
    }
}
#[test]
fn capture_binds_manifest_lock_and_exact_cargo_without_defaults() {
    let f = Fixture::new(1);
    let provisioned = f.provision().unwrap();
    let bytes = fs::read(&f.lock).unwrap();
    assert!(HeldSeed::capture(&provisioned.config, &bytes, &"d".repeat(64)).is_err());
    assert!(HeldSeed::capture(&provisioned.config, b"version=4\n", &"c".repeat(64)).is_err());
    let mut config = provisioned.config.clone();
    config.expected_sha256 = "f".repeat(64);
    assert!(HeldSeed::capture(&config, &bytes, &"c".repeat(64)).is_err());
    config = provisioned.config;
    config.manifest_path = f.lock.clone();
    assert!(HeldSeed::capture(&config, &bytes, &"c".repeat(64)).is_err());
}
#[test]
fn index_drift_and_added_input_entries_refuse_at_capture_and_final_recheck() {
    for kind in ["index", "extra_archive", "extra_index", "empty_dir"] {
        let f = Fixture::new(1);
        let provisioned = f.provision().unwrap();
        let held = f.held(&provisioned);
        let destination = f._root.path().join("home");
        let staged = held.stage(&destination).unwrap();
        let path = match kind {
            "index" => "registry/index/crates-io/config.json",
            "extra_index" => "registry/index/crates-io/.cache/extra",
            "empty_dir" => "registry/index/crates-io/.cache/new-dir",
            _ => "registry/cache/crates-io/not-locked-0.1.0.crate",
        };
        if kind == "empty_dir" {
            directory(&destination.join(path));
            directory(&f.seed.join(path));
        } else {
            write(&destination.join(path), b"drift");
            write(&f.seed.join(path), b"drift");
        }
        assert!(staged.verify().is_err(), "{kind}");
        assert!(held.verify().is_err(), "{kind}");
        assert!(HeldSeed::capture(
            &provisioned.config,
            &fs::read(&f.lock).unwrap(),
            &"c".repeat(64)
        )
        .is_err());
    }
}
#[test]
fn no_ambient_config_credentials_git_or_extracted_sources_are_provisioned() {
    let f = Fixture::new(1);
    for path in [
        "config.toml",
        "credentials.toml",
        "bin/tool",
        "git/db/secret",
        "registry/src/host/source.rs",
    ] {
        write(&f.cache.join(path), b"must never copy");
    }
    let provisioned = f.provision().unwrap();
    f.held(&provisioned).verify().unwrap();
    for path in [
        "config.toml",
        "credentials.toml",
        "bin",
        "git",
        "registry/src",
    ] {
        assert!(!f.seed.join(path).exists());
    }
    let staged = f
        .held(&provisioned)
        .stage(&f._root.path().join("home"))
        .unwrap();
    write(&staged.root.join("config.toml"), b"forbidden override");
    assert!(staged.verify().is_err());
}
#[test]
fn aliases_symlinks_and_hardlinks_fail_closed() {
    for kind in ["symlink", "hardlink"] {
        let f = Fixture::new(1);
        let archive = f.cache.join("registry/cache/crates-io/p000-0.1.0.crate");
        match kind {
            "symlink" => {
                let other = f._root.path().join("archive");
                fs::rename(&archive, &other).unwrap();
                symlink(other, &archive).unwrap();
            }
            "hardlink" => fs::hard_link(&archive, f._root.path().join("alias")).unwrap(),
            _ => unreachable!(),
        }
        assert!(f.provision().is_err(), "{kind}");
    }
    let f = Fixture::new(1);
    let p = f.provision().unwrap();
    let held = f.held(&p);
    assert!(held.stage(&f.seed).is_err());
    assert!(held.stage(&f.seed.join("nested")).is_err());
    let alias = f._root.path().join("aliased-cache");
    symlink(&f.cache, &alias).unwrap();
    assert!(provision(&ProvisionRequest {
        cache_root: &alias,
        lock_path: &f.lock,
        expected_lock_sha256: &sha(&fs::read(&f.lock).unwrap()),
        registry_directory: "crates-io",
        cargo_sha256: &"c".repeat(64),
        destination: &f._root.path().join("other")
    })
    .is_err());
}
#[test]
fn seed_budget_checks_use_small_files_and_explicit_bounds() {
    assert!(namespace("../cache").is_err());
    assert!(namespace("").is_err());
    assert!(io::relative("registry/../credentials").is_err());
    assert!(io::relative("registry/.git/config").is_err());
    let f = Fixture::new(1);
    let p = f.provision().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&p.config.manifest_path).unwrap()).unwrap();
    value["entries"][0]["bytes"] = serde_json::json!(io::MAX_TOTAL + 1);
    let manifest: Manifest = serde_json::from_value(value).unwrap();
    assert!(validate_manifest(&manifest, &fs::read(&f.lock).unwrap(), &"c".repeat(64)).is_err());
}

#[test]
fn generated_archive_paths_cannot_silently_merge_distinct_lock_packages() {
    let packages = vec![
        Package {
            name: "a-b".into(),
            version: "c".into(),
            checksum: "a".repeat(64),
        },
        Package {
            name: "a".into(),
            version: "b-c".into(),
            checksum: "a".repeat(64),
        },
    ];
    assert!(archive_paths(&packages, "crates-io")
        .unwrap_err()
        .to_string()
        .contains("collide"));
    let lock = format!(
        "version=4\n{}",
        packages
            .iter()
            .map(|p| format!(
                "[[package]]\nname={:?}\nversion={:?}\nsource={REGISTRY:?}\nchecksum={:?}\n",
                p.name, p.version, p.checksum
            ))
            .collect::<String>()
    );
    assert!(super::packages(lock.as_bytes()).is_err());
}
#[test]
fn every_locked_crate_requires_a_regular_canonical_index_metadata_file() {
    for (name, expected) in [
        ("A", "1/a"),
        ("AB", "2/ab"),
        ("AbC", "3/a/abc"),
        ("AbCdE", "ab/cd/abcde"),
    ] {
        assert_eq!(index_path(name), expected);
    }
    let f = Fixture::new(2);
    fs::remove_file(f.cache.join(format!(
        "registry/index/crates-io/.cache/{}",
        index_path("p001")
    )))
    .unwrap();
    let error = format!("{:#}", f.provision().unwrap_err());
    assert!(error.contains("required index metadata input missing/unsafe"));
    assert!(error.contains(&index_path("p001")));
    assert!(!f.seed.join(MANIFEST).exists());
}

#[test]
fn extracted_sources_require_archive_bytes_complete_inventory_and_exact_marker() {
    for change in [
        "content",
        "extra",
        "missing",
        "marker",
        "symlink",
        "hardlink",
        "type",
        "mode",
        "unknown",
        "package",
        "namespace",
        "marker_missing",
    ] {
        let f = Fixture::new(1);
        let provisioned = f.provision().unwrap();
        let held = f.held(&provisioned);
        let home = f._root.path().join("home");
        let staged = held.stage(&home).unwrap();
        assert_eq!(
            fs::read(home.join("registry/src/crates-io/p000-0.1.0/src/lib.rs")).unwrap(),
            source_body(0)
        );
        staged.verify().unwrap();
        let package = home.join("registry/src/crates-io/p000-0.1.0");
        let file = package.join("src/lib.rs");
        match change {
            "package" => fs::remove_dir_all(&package).unwrap(),
            "namespace" => fs::remove_dir_all(home.join("registry/src")).unwrap(),
            "marker_missing" => fs::remove_file(package.join(".cargo-ok")).unwrap(),
            "content" => write(&file, b"poisoned persistent source"),
            "extra" => write(&package.join("extra.rs"), b"extra"),
            "missing" => fs::remove_file(&file).unwrap(),
            "marker" => write(&package.join(".cargo-ok"), b"{\"v\":2}"),
            "symlink" => {
                let other = f._root.path().join("same-source.rs");
                write(&other, &source_body(0));
                fs::remove_file(&file).unwrap();
                symlink(&other, &file).unwrap();
            }
            "hardlink" => {
                fs::hard_link(&file, f._root.path().join("same-source.rs")).unwrap();
            }
            "type" => {
                fs::remove_file(&file).unwrap();
                directory(&file);
            }
            "mode" => fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap(),
            _ => materialize(&home, "unlocked", b"unknown"),
        }
        assert!(staged.verify().is_err(), "{change}");
    }
}
#[test]
fn runtime_private_writable_bits_are_not_ambient_cache_grants() {
    let f = Fixture::new(1);
    let provisioned = f.provision().unwrap();
    let held = f.held(&provisioned);
    let home = f._root.path().join("home");
    let staged = held.stage(&home).unwrap();
    let package = home.join("registry/src/crates-io/p000-0.1.0");
    fs::set_permissions(&package, fs::Permissions::from_mode(0o775)).unwrap();
    fs::set_permissions(
        package.join("src/lib.rs"),
        fs::Permissions::from_mode(0o664),
    )
    .unwrap();
    staged.verify().unwrap();
}
#[test]
fn operator_pinned_manifest_cannot_redeclare_poisoned_extracted_expectations() {
    let f = Fixture::new(1);
    let mut provisioned = f.provision().unwrap();
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&provisioned.config.manifest_path).unwrap()).unwrap();
    value["expanded"][0]["entries"][0]["sha256"] = serde_json::json!("f".repeat(64));
    let bytes = serde_json::to_vec(&value).unwrap();
    write(&provisioned.config.manifest_path, &bytes);
    provisioned.config.expected_sha256 = sha(&bytes);
    assert!(HeldSeed::capture(
        &provisioned.config,
        &fs::read(&f.lock).unwrap(),
        &"c".repeat(64)
    )
    .is_err());
}

fn source_group_modes(path: &Path) {
    let meta = fs::symlink_metadata(path).unwrap();
    if meta.is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o775)).unwrap();
        for entry in fs::read_dir(path).unwrap() {
            source_group_modes(&entry.unwrap().path());
        }
    } else {
        fs::set_permissions(path, fs::Permissions::from_mode(0o664)).unwrap();
    }
}
#[test]
fn explicit_public_cache_import_accepts_same_uid_group_write_and_stages_strictly() {
    let f = Fixture::new(2);
    source_group_modes(&f.cache);
    let result = f.provision().unwrap();
    assert_eq!(result.identity.index_provenance, INDEX_PROVENANCE);
    let held = f.held(&result);
    held.verify().unwrap();
    let home = f._root.path().join("home");
    held.stage(&home).unwrap().verify().unwrap();
    assert_eq!(fs::metadata(&f.seed).unwrap().mode() & 0o777, 0o700);
    assert_eq!(
        fs::metadata(f.seed.join("registry/cache/crates-io/p000-0.1.0.crate"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(f.cache.join("registry/cache/crates-io/p000-0.1.0.crate"))
            .unwrap()
            .mode()
            & 0o777,
        0o664
    );
}
#[test]
fn public_import_never_accepts_other_write_or_private_files_even_with_valid_bytes() {
    for (path, mode) in [
        ("", 0o777),
        ("registry/index/crates-io/.cache", 0o777),
        ("registry/cache/crates-io/p000-0.1.0.crate", 0o666),
        ("registry/index/crates-io/config.json", 0o600),
    ] {
        let f = Fixture::new(1);
        source_group_modes(&f.cache);
        fs::set_permissions(f.cache.join(path), fs::Permissions::from_mode(mode)).unwrap();
        let error = f.provision().unwrap_err();
        let expected = match mode {
            0o600 => "world-readable",
            0o666 => "seed file must be bounded",
            _ => "ownership/mode",
        };
        assert!(
            format!("{error:#}").contains(expected),
            "unexpected precondition failure: {error:#}"
        );
        assert!(!f.seed.join(MANIFEST).exists());
    }
}
#[test]
fn public_import_policy_does_not_relax_captured_or_staged_seed_permissions() {
    let f = Fixture::new(1);
    source_group_modes(&f.cache);
    let result = f.provision().unwrap();
    let held = f.held(&result);
    let home = f._root.path().join("home");
    let staged = held.stage(&home).unwrap();
    fs::set_permissions(
        home.join("registry/index/crates-io/config.json"),
        fs::Permissions::from_mode(0o664),
    )
    .unwrap();
    assert!(staged.verify().is_err());
    fs::set_permissions(&f.seed, fs::Permissions::from_mode(0o775)).unwrap();
    assert!(held.verify().is_err());
    assert!(
        HeldSeed::capture(&result.config, &fs::read(&f.lock).unwrap(), &"c".repeat(64)).is_err()
    );
}
#[test]
fn public_import_keeps_link_and_persistent_named_root_drift_refusals() {
    let f = Fixture::new(1);
    source_group_modes(&f.cache);
    let captured = io::PublicCache::open(&f.cache).unwrap();
    let moved = f._root.path().join("moved-cache");
    fs::rename(&f.cache, &moved).unwrap();
    fs::create_dir(&f.cache).unwrap();
    fs::set_permissions(&f.cache, fs::Permissions::from_mode(0o775)).unwrap();
    assert!(captured.verify().is_err());
    for hard in [false, true] {
        let f = Fixture::new(1);
        source_group_modes(&f.cache);
        let original = f.cache.join("registry/cache/crates-io/p000-0.1.0.crate");
        let alias = f._root.path().join("archive");
        if hard {
            fs::hard_link(&original, &alias).unwrap();
        } else {
            fs::rename(&original, &alias).unwrap();
            symlink(&alias, &original).unwrap();
        }
        assert!(f.provision().is_err());
    }
}

#[test]
fn public_import_rejects_replaceable_other_writable_ancestor_and_parent_alias() {
    let f = Fixture::new(1);
    source_group_modes(&f.cache);
    fs::set_permissions(f._root.path(), fs::Permissions::from_mode(0o777)).unwrap();
    let error = io::PublicCache::open(&f.cache)
        .err()
        .expect("unsafe source ancestor refused");
    assert!(format!("{error:#}").contains("ownership/mode"), "{error:#}");
    fs::set_permissions(f._root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let alias = f._root.path().join("alias");
    symlink(&f.cache, &alias).unwrap();
    assert!(io::PublicCache::open(&alias).is_err());
}

#[test]
fn index_import_ignores_unselected_credentials_configuration_and_special_entries() {
    let f = Fixture::new(1);
    for name in [
        "credentials.toml",
        "config.toml",
        ".cache/credentials.toml",
        ".cache/not-locked",
    ] {
        let path = f.cache.join("registry/index/crates-io").join(name);
        public_write(&path, b"unrelated content must not be imported");
        // Neither invalid permissions nor unrelated bytes should be inspected.
        fs::set_permissions(path, fs::Permissions::from_mode(0o666)).unwrap();
    }
    let unreadable = f.cache.join("registry/index/crates-io/.cache/private");
    write(&unreadable, b"private-unrelated");
    fs::set_permissions(unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    let link = f
        .cache
        .join("registry/index/crates-io/.cache/unrelated-link");
    symlink("/definitely-not-a-cache-record", &link).unwrap();
    directory(&f.cache.join("registry/index/crates-io/.git"));
    let socket = f
        .cache
        .join("registry/index/crates-io/.cache/unrelated-socket");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let result = f.provision().unwrap();
    f.held(&result).verify().unwrap();
    assert_eq!(result.identity.files, 3); // exactly archive, selected record, config.json.
    for name in [
        "credentials.toml",
        "config.toml",
        ".cache/credentials.toml",
        ".cache/not-locked",
        ".cache/private",
        ".cache/unrelated-link",
        ".cache/unrelated-socket",
        ".git",
    ] {
        assert!(!f.seed.join("registry/index/crates-io").join(name).exists());
    }
}
#[test]
fn index_selection_deduplicates_versions_without_importing_unrelated_names() {
    let packages = vec![
        Package {
            name: "same".into(),
            version: "1.0.0".into(),
            checksum: "a".repeat(64),
        },
        Package {
            name: "same".into(),
            version: "2.0.0".into(),
            checksum: "b".repeat(64),
        },
    ];
    assert_eq!(
        index_paths(&packages, "ns"),
        BTreeSet::from([
            "registry/index/ns/config.json".into(),
            "registry/index/ns/.cache/sa/me/same".into()
        ])
    );
}
