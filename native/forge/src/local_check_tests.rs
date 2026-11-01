use super::*;

const POLICY: &str = r#"
base_ref = "origin/main"
allowed_untracked_prefixes = ["research/"]

[[setup]]
name = "sync"
run = "true"
timeout_s = 2

[[check]]
name = "python"
run = "true"
paths = ['^src/']
timeout_s = 2
"#;

#[test]
fn policy_rejects_invalid_regex_duplicate_and_unbounded_steps() {
    assert!(Policy::parse(POLICY).is_ok());
    assert!(Policy::parse(&POLICY.replace("'^src/'", "'('")).is_err());
    assert!(Policy::parse(&POLICY.replace("timeout_s = 2", "timeout_s = 0")).is_err());
    assert!(Policy::parse(&POLICY.replace("name = \"python\"", "name = \"sync\"")).is_err());
    assert!(Policy::parse(&POLICY.replace("research/", "../")).is_err());
}

#[test]
fn shipped_native_policy_checks_dependents_and_isolated_installed_artifact() {
    let policy = Policy::parse(include_str!("../../../.forge/local-check.toml")).unwrap();
    for crate_name in ["conductor-native", "slop-core", "forge"] {
        let changed = vec![format!("native/{crate_name}/src/lib.rs")];
        for name in ["rust-forge-clippy", "rust-forge-tests", "installed-runtime"] {
            let check = policy.checks.iter().find(|step| step.name == name).unwrap();
            assert!(
                selected(check, &changed),
                "{crate_name} omits dependent {name}"
            );
        }
        assert!(policy.setup.iter().all(|step| selected(step, &changed)));
    }
    assert_eq!(policy.env["PYO3_PYTHON"], "/nonexistent/forbidden-python");
    let land = crate::land::Config::parse(include_str!("../../../.forge/land.toml")).unwrap();
    crate::sweep::SweepConfig::parse(include_str!("../../../.forge/sweep.toml"), &land).unwrap();
}

#[test]
fn infrastructure_changes_force_full_selection() {
    assert!(!effective_all(false, &["docs/readme.md".into()]));
    assert!(effective_all(
        false,
        &["native/forge/src/local_check.rs".into()]
    ));
    assert!(effective_all(false, &[".forge/local-check.toml".into()]));
    assert!(effective_all(false, &["forge.toml".into()]));
    assert!(effective_all(true, &[]));
}

#[test]
fn a_second_check_cannot_take_the_same_lock() {
    let dir = std::env::temp_dir().join(format!("forge-check-lock-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let first = acquire_lock(&dir, true).unwrap();
    assert!(acquire_lock(&dir, true).is_err());
    assert!(acquire_lock(&dir, false).is_err());
    drop(first);
    // A concurrent fork briefly shares the open file description until its
    // CLOEXEC descriptor closes. Verify release with a bounded deadline.
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if acquire_lock(&dir, false).is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "check lock was not released");
        std::thread::sleep(Duration::from_millis(1));
    }
    fs::remove_dir_all(dir).unwrap();
}

const LAUNCHER_KEYS: &[&str] = &[
    "CARGO_MANIFEST_DIR",
    "CARGO_MANIFEST_PATH",
    "OUT_DIR",
    "CARGO_PKG_NAME",
    "CARGO_PKG_VERSION",
    "CARGO_PKG_VERSION_MAJOR",
    "CARGO_PKG_VERSION_MINOR",
    "CARGO_PKG_VERSION_PATCH",
    "CARGO_PKG_VERSION_PRE",
    "CARGO_PKG_AUTHORS",
    "CARGO_PKG_LICENSE",
    "CARGO_PKG_FUTURE_METADATA",
    "PYO3_CONFIG_FILE",
    "GIT_DIR",
];
const BUILD_SETTINGS: &[(&str, &str)] = &[
    ("CARGO_HOME", "/synthetic/cargo-home"),
    ("CARGO_TARGET_DIR", "/synthetic/target"),
    ("CARGO_BUILD_JOBS", "2"),
    ("CARGO_ENCODED_RUSTFLAGS", "--cfg\u{1f}synthetic_probe"),
    ("RUSTFLAGS", "--cfg synthetic_probe"),
    ("RUSTUP_TOOLCHAIN", "1.98.0"),
    ("PYO3_PYTHON", "/nonexistent/forbidden-python"),
];

#[test]
fn child_build_removes_launcher_metadata_but_preserves_build_settings() {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("FORGE_CHECK_ENV_FIXTURE", "launcher")
        .args([
            "--exact",
            "local_check::tests::launcher_metadata_child",
            "--ignored",
            "--nocapture",
        ]);
    for key in LAUNCHER_KEYS {
        command.env(key, "synthetic-launcher-value");
    }
    for (key, value) in BUILD_SETTINGS {
        command.env(key, value);
    }
    let output =
        forge::process_probe::run(&mut command, Duration::from_secs(10), 64 * 1024).unwrap();
    // Never print inherited environment or child output on assertion failure.
    assert!(
        output.status.success(),
        "native launcher/observer fixture failed"
    );
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("clean-build-environment-observed"));
}

struct CheckEnvScratch(PathBuf);
impl Drop for CheckEnvScratch {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("check environment fixture cleanup: {error}");
        }
    }
}

/// Re-executed native test fixture; never changes the test process's environment.
/// The middle process exercises the same measured shell path as gate jobs.
#[test]
#[ignore = "subprocess fixture for child_build_removes_launcher_metadata_but_preserves_build_settings"]
fn launcher_metadata_child() {
    match std::env::var("FORGE_CHECK_ENV_FIXTURE").as_deref() {
        Ok("observer") => {
            for key in LAUNCHER_KEYS {
                assert!(
                    std::env::var_os(key).is_none(),
                    "launcher metadata survived: {key}"
                );
            }
            assert!(std::env::var_os("CARGO_PKG_POLICY_ONLY_METADATA").is_none());
            for (key, expected) in BUILD_SETTINGS {
                assert!(
                    std::env::var(key).is_ok_and(|actual| actual == *expected),
                    "build setting changed: {key}"
                );
            }
            println!("clean-build-environment-observed");
        }
        Ok("launcher") => launch_clean_observer(),
        Err(std::env::VarError::NotPresent) => {}
        _ => panic!("invalid native environment fixture mode"),
    }
}

fn launch_clean_observer() {
    let root = std::env::temp_dir().join(forge::resource_id("forge-check-env").unwrap());
    fs::create_dir(&root).unwrap();
    let scratch = CheckEnvScratch(root);
    let log = scratch.0.join("child.log");
    let executable = std::env::current_exe().unwrap();
    let quoted = format!(
        "'{}'",
        executable.to_str().unwrap().replace('\'', "'\"'\"'")
    );
    let command = format!(
        "exec {quoted} --exact local_check::tests::launcher_metadata_child --ignored --nocapture"
    );
    let child_env = vec![
        ("FORGE_CHECK_ENV_FIXTURE".into(), "observer".into()),
        (
            "CARGO_PKG_POLICY_ONLY_METADATA".into(),
            "synthetic-policy-metadata".into(),
        ),
    ];
    let cleaned = clean_command(&command, &child_env);
    let measurement = run_measured(
        &ShellRun {
            command: &cleaned,
            cwd: &scratch.0,
            env: &child_env,
            log: &log,
            timeout: Duration::from_secs(5),
        },
        64 * 1024,
    )
    .unwrap();
    assert_eq!(
        measurement.outcome,
        Outcome::Passed,
        "cleaned native observer failed"
    );
    let observed = fs::read_to_string(log).unwrap();
    assert!(observed.contains("clean-build-environment-observed"));
    println!("clean-build-environment-observed");
}
