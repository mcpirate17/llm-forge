//! Synthetic admission fixtures, never executable campaigns or mutation evidence.
use super::*;
use serde_json::json;
use std::os::unix::fs::{symlink, PermissionsExt};

fn fixture(directory: &Path) -> Campaign {
    crate::mutation_run_test_support::campaign(
        directory,
        json!({
            "language":"rust","mutation_engine":"cargo-mutants",
            "generator":{"source":["src/lib.rs"],"jobs":1,"run_timeout_seconds":60,
                "options":{"manifest_path":"native/demo/Cargo.toml","package":"demo",
                    "package_root":"native/demo","execution_profile":"cargo-workspace-v2"}},
            "source_sha256":{"native/demo/src/lib.rs":"a".repeat(64)},
            "test_sha256":{"native/demo/tests/check.rs":"b".repeat(64)},
            "test_argv":["cargo","test","--manifest-path","native/demo/Cargo.toml","--package","demo"]
        }),
    )
}

#[test]
fn nested_literal_sources_match_repo_pins_without_scope_broadening() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = fixture(dir.path());
    assert_eq!(
        campaign(&c).unwrap(),
        ("native/demo/Cargo.toml".into(), "demo".into())
    );
    c.source.push("src/other.rs".into());
    assert!(campaign(&c).unwrap_err().to_string().contains("scope"));
    c.source = vec!["src/lib.rs".into(), "src/lib.rs".into()];
    assert!(campaign(&c).is_err());
    c.source = vec!["src/*.rs".into()];
    c.source_sha256 = [("native/demo/src/*.rs".into(), "a".repeat(64))].into();
    assert!(campaign(&c).unwrap_err().to_string().contains("glob"));
}

#[test]
fn unsupported_test_filters_environment_and_roots_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = fixture(dir.path());
    c.test_argv.push("--lib".into());
    assert!(campaign(&c).is_err());
    let mut c = fixture(dir.path());
    c.environment
        .insert("CARGO_TARGET_DIR".into(), "/tmp".into());
    assert!(campaign(&c).is_err());
    let mut c = fixture(dir.path());
    c.options.insert("package_root".into(), json!("different"));
    assert!(campaign(&c).is_err());
}

#[test]
fn active_config_is_refused_but_unrelated_nested_fixtures_are_not() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("native/demo")).unwrap();
    fs::create_dir_all(dir.path().join("tests/fixtures/.cargo")).unwrap();
    fs::write(
        dir.path().join("tests/fixtures/.cargo/config.toml"),
        "fixture",
    )
    .unwrap();
    no_cargo_redirection(dir.path(), "native/demo/Cargo.toml").unwrap();
    fs::create_dir_all(dir.path().join("native/.cargo")).unwrap();
    fs::write(dir.path().join("native/.cargo/config.toml"), "[env]").unwrap();
    assert!(no_cargo_redirection(dir.path(), "native/demo/Cargo.toml").is_err());
}

#[test]
fn unseeded_home_refuses_configuration_and_aliases_after_execution() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    verify_unseeded_home(dir.path()).unwrap();
    fs::write(dir.path().join(".package-cache"), "").unwrap();
    verify_unseeded_home(dir.path()).unwrap();
    fs::write(dir.path().join("config.toml"), "[env]").unwrap();
    assert!(verify_unseeded_home(dir.path()).is_err());
    fs::remove_file(dir.path().join("config.toml")).unwrap();
    fs::remove_file(dir.path().join(".package-cache")).unwrap();
    symlink("/dev/null", dir.path().join(".package-cache")).unwrap();
    assert!(verify_unseeded_home(dir.path()).is_err());
}

#[test]
fn actual_command_ancestors_refuse_scratch_configuration_and_aliases() {
    let root = tempfile::tempdir().unwrap();
    let scratch = root.path().join("scratch");
    let cwd = scratch.join("worktree/deep");
    fs::create_dir_all(&cwd).unwrap();
    verify_command_ancestors(&cwd).unwrap();
    fs::create_dir(scratch.join(".cargo")).unwrap();
    fs::write(scratch.join(".cargo/config.toml"), "[env]").unwrap();
    assert!(verify_command_ancestors(&cwd).is_err());
    fs::remove_file(scratch.join(".cargo/config.toml")).unwrap();
    fs::remove_dir(scratch.join(".cargo")).unwrap();
    fs::write(scratch.join("rust-toolchain.toml"), "[toolchain]").unwrap();
    assert!(verify_command_ancestors(&cwd).is_err());
    fs::remove_file(scratch.join("rust-toolchain.toml")).unwrap();
    symlink("missing", scratch.join(".cargo")).unwrap();
    assert!(verify_command_ancestors(&cwd).is_err());
    fs::remove_file(scratch.join(".cargo")).unwrap();
    symlink(&cwd, root.path().join("aliased-cwd")).unwrap();
    assert!(verify_command_ancestors(&root.path().join("aliased-cwd")).is_err());
    verify_command_ancestors(&cwd).unwrap();
}

#[test]
fn selected_workspace_admission_requires_exact_typed_profile_and_command() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = fixture(dir.path());
    let selector =
        json!({"schema_version":1,"target":{"kind":"test","name":"contract"},"filter":"checks::"});
    c.options
        .insert("workspace_selector".into(), selector.clone());
    assert!(
        campaign(&c).is_err(),
        "v2 must not silently accept a selector"
    );
    c.options
        .insert("execution_profile".into(), json!("cargo-workspace-v3"));
    assert!(
        campaign(&c).is_err(),
        "v3 must not retain a package-wide test command"
    );
    c.test_argv
        .extend(["--test=contract".into(), "--".into(), "checks::".into()]);
    assert!(campaign(&c).is_ok());
    c.test_argv.push("--ignored".into());
    assert!(campaign(&c).is_err());
    c.test_argv.pop();
    c.options.remove("workspace_selector");
    assert!(campaign(&c).is_err(), "v3 requires explicit selector");
}
