//! Unit fixtures exercise admission/provenance; they are not mutation evidence.
use super::*;

#[test]
fn coordinator_holds_campaign_without_a_snapshot_copy_and_rejects_drift() {
    let dir = crate::mutation_run_test_support::repository(&["src/a.rs"]);
    fs::write(dir.path().join(".gitignore"), "/campaigns/\n").unwrap();
    fs::create_dir(dir.path().join("campaigns")).unwrap();
    let first = dir.path().join("campaigns/generated.json");
    let bytes = br#"{"generated_value":{"schema_version":1}}"#;
    fs::write(&first, bytes).unwrap();
    let snapshot = crate::mutation_snapshot::Snapshot::create(dir.path()).unwrap();
    assert!(!snapshot.worktree.join("campaigns/generated.json").exists());
    let (held, payload) = capture_manifest(&first, &sha256_hex(bytes)).unwrap();
    assert_eq!(payload["generated_value"]["schema_version"], 1);
    assert!(capture_manifest(&first, &"0".repeat(64)).is_err());
    fs::write(&first, b"{}").unwrap();
    assert!(capture_manifest(&first, &sha256_hex(bytes)).is_err());
    assert!(held.verify().is_err());
    fs::remove_file(&first).unwrap();
    fs::write(&first, bytes).unwrap();
    assert!(held.verify().is_err());
}
#[test]
fn clean_environment_contains_only_fixed_toolchain_and_owned_paths() {
    let dir = tempfile::tempdir().unwrap();
    let env = clean_environment(
        dir.path(),
        "/toolchain/cargo",
        "/toolchain/rustc",
        "/toolchain/rustdoc",
    )
    .unwrap();
    assert_eq!(env["CARGO"], "/toolchain/cargo");
    assert_eq!(env["RUSTC"], "/toolchain/rustc");
    assert_eq!(env["RUSTDOC"], "/toolchain/rustdoc");
    assert_eq!(env["PATH"], dir.path().join("bin").to_string_lossy());
    for key in ["CARGO_HOME", "CARGO_TARGET_DIR", "PYTHONPYCACHEPREFIX"] {
        assert!(Path::new(&env[key]).starts_with(dir.path()));
    }
    for key in [
        "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN",
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "PYTHONPATH",
        "CONDUCTOR_SNAPSHOT_PYTHON",
    ] {
        assert!(!env.contains_key(key), "unexpected inherited key {key}");
    }
    assert_eq!(env["CARGO_NET_OFFLINE"], "true");
}
#[test]
fn ancestor_admission_refuses_configs_even_when_empty_or_symlinked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("package");
    fs::create_dir(&root).unwrap();
    // Test only owned ancestor paths; do not depend on machine-global files.
    let config = dir.path().join(".cargo");
    fs::create_dir(&config).unwrap();
    let path = config.join("config.toml");
    fs::write(&path, "").unwrap();
    assert!(ancestors(&root).unwrap_err().contains("configuration"));
    fs::remove_file(&path).unwrap();
    symlink("absent", &path).unwrap();
    assert!(ancestors(&root).is_err());
    fs::remove_file(path).unwrap();
    fs::write(dir.path().join("Cargo.toml"), "").unwrap();
    assert!(ancestors(&root).is_err());
}
#[test]
fn supplied_log_paths_cannot_escape_or_follow_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let logdir = dir.path().join("mutants.out/log");
    fs::create_dir_all(&logdir).unwrap();
    let secret = dir.path().join("outside");
    fs::write(&secret, "not a scenario").unwrap();
    symlink(&secret, logdir.join("linked.log")).unwrap();
    let report = json!({"outcomes":[{"log_path":"log/linked.log"}]});
    assert!(confined_logs(dir.path(), &report).is_err());
    for path in [
        "../outside",
        "/tmp/outside",
        "log/../../outside",
        "log//a.log",
    ] {
        let report = json!({"outcomes":[{"log_path":path}]});
        assert!(
            confined_logs(dir.path(), &report).is_err(),
            "accepted {path}"
        );
    }
}
#[test]
fn original_scenarios_reconcile_through_existing_row_ids_including_tied_locations() {
    let make = |replacement: &str| json!({"scenario":{"Mutant":{"name":"generated","package":"demo","file":"src/lib.rs","function":{"function_name":"f","return_type":"bool"},"span":{"start":{"line":1,"column":3},"end":{"line":1,"column":7}},"replacement":replacement,"genre":"FnValue"}},"summary":"CaughtMutant","phase_results":[]});
    let report = json!({"outcomes":[make("true"),make("false")]});
    let rows = rows(&report, "").unwrap();
    let mapped = expected(&report, &rows).unwrap();
    assert_ne!(mapped[0].id, mapped[1].id);
    for (index, row) in mapped.iter().enumerate() {
        assert_eq!(row.id, rows[index]["id"]);
        assert_eq!(row.scenario, report["outcomes"][index]["scenario"]);
    }
    assert!(expected(&report, &rows[..1]).is_err());
}
#[test]
fn failed_or_dropped_runtime_keeps_its_owned_directory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let runtime = Runtime {
        directory: Some(dir),
        tools: vec![],
        environment: BTreeMap::new(),
        cargo: String::new(),
        engine: String::new(),
        rustc: String::new(),
        rustdoc: String::new(),
        links: BTreeMap::new(),
        sandbox: None,
        reactor: tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
        cgroup_parent: PathBuf::from("/not-used-in-pure-owner-unit"),
        cleanup_complete: Cell::new(true),
        signal: Cell::new(None),
        limits: group_limits(None).unwrap(),
        operator_home: PathBuf::from("/unit/home"),
        rustup_home: PathBuf::from("/unit/home/.rustup"),
    };
    assert!(!runtime.finish(false).unwrap());
    assert!(path.is_dir());
    fs::remove_dir(path).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let runtime = Runtime {
        directory: Some(dir),
        tools: vec![],
        environment: BTreeMap::new(),
        cargo: String::new(),
        engine: String::new(),
        rustc: String::new(),
        rustdoc: String::new(),
        links: BTreeMap::new(),
        sandbox: None,
        reactor: tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
        cgroup_parent: PathBuf::from("/not-used-in-pure-owner-unit"),
        cleanup_complete: Cell::new(true),
        signal: Cell::new(None),
        limits: group_limits(None).unwrap(),
        operator_home: PathBuf::from("/unit/home"),
        rustup_home: PathBuf::from("/unit/home/.rustup"),
    };
    drop(runtime);
    assert!(path.is_dir());
    fs::remove_dir(path).unwrap();
}
#[test]
fn verified_success_removes_runtime_and_tool_replacement_fails_identity() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("native-tool");
    fs::write(&binary, "native bytes").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    let tool = Tool::capture(&binary).unwrap();
    tool.verify().unwrap();
    fs::remove_file(&binary).unwrap();
    fs::write(&binary, "native bytes").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(tool.verify().is_err());
    let owned = tempfile::tempdir().unwrap();
    let path = owned.path().to_path_buf();
    let runtime = Runtime {
        directory: Some(owned),
        tools: vec![],
        environment: BTreeMap::new(),
        cargo: String::new(),
        engine: String::new(),
        rustc: String::new(),
        rustdoc: String::new(),
        links: BTreeMap::new(),
        sandbox: None,
        reactor: tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
        cgroup_parent: PathBuf::from("/not-used-in-pure-owner-unit"),
        cleanup_complete: Cell::new(true),
        signal: Cell::new(None),
        limits: group_limits(None).unwrap(),
        operator_home: PathBuf::from("/unit/home"),
        rustup_home: PathBuf::from("/unit/home/.rustup"),
    };
    assert!(runtime.finish(true).unwrap());
    assert!(!path.exists());
}

#[test]
fn cleanup_unknown_never_removes_owned_payload_even_with_a_nominal_success_request() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let runtime = Runtime {
        directory: Some(dir),
        tools: vec![],
        environment: BTreeMap::new(),
        cargo: String::new(),
        engine: String::new(),
        rustc: String::new(),
        rustdoc: String::new(),
        links: BTreeMap::new(),
        sandbox: None,
        reactor: tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap(),
        cgroup_parent: PathBuf::from("/not-used-in-pure-owner-unit"),
        cleanup_complete: Cell::new(false),
        signal: Cell::new(None),
        limits: group_limits(None).unwrap(),
        operator_home: PathBuf::from("/unit/home"),
        rustup_home: PathBuf::from("/unit/home/.rustup"),
    };
    assert!(!runtime.finish(true).unwrap());
    assert!(path.is_dir());
    fs::remove_dir(path).unwrap();
}

#[test]
fn explicit_smaller_memory_cap_is_not_silently_ignored_by_confined_admission() {
    assert_eq!(group_limits(None).unwrap().memory_mib, 8192);
    assert_eq!(
        group_limits(Some(MemoryCap {
            limit: 64 * 1024 * 1024,
            hard: u64::MAX
        }))
        .unwrap()
        .memory_mib,
        64
    );
    assert!(group_limits(Some(MemoryCap {
        limit: 1024 * 1024,
        hard: u64::MAX
    }))
    .is_err());
    assert_eq!(
        group_limits(Some(MemoryCap {
            limit: 64 * 1024 * 1024 * 1024,
            hard: u64::MAX
        }))
        .unwrap()
        .memory_mib,
        8192
    );
}

#[test]
fn arbitrary_executable_parents_never_become_home_or_repository_read_grants() {
    for path in [
        "/home/user/bin/cargo",
        "/usr/bin/cargo",
        "/bin/cargo",
        "/cargo",
    ] {
        assert!(
            narrow_toolchain_root(
                Path::new(path),
                Path::new("/home/user/.rustup"),
                Path::new("/home/user")
            )
            .is_err(),
            "accepted broad root for {path}"
        );
    }
    assert_eq!(
        narrow_toolchain_root(
            Path::new("/home/user/.rustup/toolchains/1.98/bin/cargo"),
            Path::new("/home/user/.rustup"),
            Path::new("/home/user")
        )
        .unwrap(),
        Path::new("/home/user/.rustup/toolchains/1.98")
    );
}

#[test]
fn forged_toolchains_names_and_symmetric_capability_overlaps_refuse() {
    let home = Path::new("/home/user");
    let rustup = Path::new("/home/user/.rustup");
    for cargo in [
        "/home/toolchains/user/bin/cargo",
        "/project/target/toolchains/name/bin/cargo",
    ] {
        assert!(narrow_toolchain_root(Path::new(cargo), rustup, home).is_err());
    }
    assert!(narrow_toolchain_root(
        Path::new("/x/toolchains/home/bin/cargo"),
        Path::new("/x"),
        Path::new("/x/toolchains/home")
    )
    .is_err());
    let repo = Path::new("/project");
    let snapshot = Path::new("/scratch/snapshot");
    let runtime = Path::new("/scratch/runtime");
    for read in [
        "/project/toolchains/name",
        "/project",
        "/scratch",
        "/scratch/runtime/tools",
    ] {
        assert!(validate_read_root(Path::new(read), repo, snapshot, runtime).is_err());
    }
    assert!(validate_read_root(
        Path::new("/usr"),
        Path::new("/usr/local/src/host"),
        snapshot,
        runtime
    )
    .is_err());
    validate_read_root(Path::new("/usr"), repo, snapshot, runtime).unwrap();
}
