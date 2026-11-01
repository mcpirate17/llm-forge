use super::*;
struct Fixture {
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(crate::native_id::new("cargo-toolchain-test").unwrap());
        fs::create_dir(&path).unwrap();
        Self { path }
    }
    fn rustup(&self, home: &str) -> PathBuf {
        let root = self.path.join(home).join(".rustup");
        fs::create_dir_all(root.join("toolchains/stable")).unwrap();
        fs::write(
            root.join("settings.toml"),
            b"default_toolchain = 'stable'\n",
        )
        .unwrap();
        root
    }
    fn cargo(&self, home: &str) -> PathBuf {
        let root = self.path.join(home).join(".cargo");
        fs::create_dir_all(root.join("bin")).unwrap();
        root
    }
    fn env(&self, home: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "HOME".into(),
                self.path.join(home).to_str().unwrap().to_owned(),
            ),
            ("UNRELATED".into(), "kept".into()),
        ])
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("remove owned fixture");
    }
}

#[test]
fn explicit_nonempty_values_are_preserved_without_validating_their_targets() {
    let f = Fixture::new();
    f.rustup("home");
    f.cargo("home");
    let mut environment = f.env("home");
    environment.insert("RUSTUP_HOME".into(), "/not-present-rustup".into());
    environment.insert("CARGO_HOME".into(), "/not-present-cargo".into());
    assert_eq!(
        resolve(&environment, Some(&f.path.join("missing"))).unwrap(),
        environment
    );
    assert_eq!(resolve_current(&environment).unwrap(), environment);
}

#[test]
fn home_precedes_login_and_each_home_is_resolved_independently() {
    let f = Fixture::new();
    let rustup = f.rustup("home");
    let cargo = f.cargo("login");
    let login = f.path.join("login");
    f.rustup("login");
    let result = resolve(&f.env("home"), Some(&login)).unwrap();
    assert_eq!(result["RUSTUP_HOME"], rustup.to_str().unwrap());
    assert_eq!(result["CARGO_HOME"], cargo.to_str().unwrap());
    assert_eq!(result["UNRELATED"], "kept");
    let local = f.cargo("home");
    assert_eq!(
        resolve(&f.env("home"), Some(&login)).unwrap()["CARGO_HOME"],
        local.to_str().unwrap()
    );
}

#[test]
fn absent_blank_home_and_shell_rustup_fall_back_without_overwriting_empty_unresolved_values() {
    let f = Fixture::new();
    let expected = f.rustup("login");
    let login = f.path.join("login");
    let shell = f.path.join("home/.rustup");
    fs::create_dir_all(shell.join("toolchains")).unwrap();
    fs::write(shell.join("settings.toml"), b"version=12\n[overrides]\n").unwrap();
    let mut env = f.env("home");
    env.insert("RUSTUP_HOME".into(), String::new());
    assert_eq!(
        resolve(&env, Some(&login)).unwrap()["RUSTUP_HOME"],
        expected.to_str().unwrap()
    );
    env.remove("HOME");
    assert_eq!(
        resolve(&env, Some(&login)).unwrap()["RUSTUP_HOME"],
        expected.to_str().unwrap()
    );
    env.insert("HOME".into(), String::new());
    assert_eq!(
        resolve(&env, Some(&login)).unwrap()["RUSTUP_HOME"],
        expected.to_str().unwrap()
    );
    assert_eq!(resolve(&env, None).unwrap(), env);
}

#[test]
fn named_default_without_any_toolchain_is_unusable_but_literal_marker_is_legacy_contract() {
    let f = Fixture::new();
    let home = f.path.join("home/.rustup");
    fs::create_dir_all(home.join("toolchains")).unwrap();
    fs::write(home.join("settings.toml"), b"default_toolchain='missing'\n").unwrap();
    assert!(!usable_rustup(&home).unwrap());
    fs::write(home.join("toolchains/arbitrary-marker"), b"not a toolchain").unwrap();
    assert!(
        usable_rustup(&home).unwrap(),
        "legacy predicate tests any entry, not default installation"
    );
    assert!(!usable_cargo(&f.path.join("home/.cargo")).unwrap());
}

#[test]
fn malformed_utf8_settings_are_unusable_but_oversized_or_special_settings_fail() {
    let f = Fixture::new();
    let home = f.rustup("home");
    fs::write(home.join("settings.toml"), [0xff]).unwrap();
    assert!(!usable_rustup(&home).unwrap());
    fs::write(home.join("settings.toml"), vec![b'a'; MAX_SETTINGS + 1]).unwrap();
    assert!(usable_rustup(&home).unwrap_err().contains("64 KiB"));
    fs::remove_file(home.join("settings.toml")).unwrap();
    let file =
        std::ffi::CString::new(home.join("settings.toml").as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(file.as_ptr(), 0o600) }, 0);
    assert!(usable_rustup(&home).unwrap_err().contains("regular file"));
}

#[test]
fn recovered_non_utf8_path_fails_explicitly_and_passwd_buffer_is_bounded() {
    let f = Fixture::new();
    let home = f.path.join(std::ffi::OsString::from_vec(vec![b'h', 0xff]));
    fs::create_dir_all(home.join(".cargo/bin")).unwrap();
    assert!(resolve(&BTreeMap::new(), Some(&home))
        .unwrap_err()
        .contains("UTF-8"));
    let bytes = b"ignored\0/home/fixture\0";
    let pointer = unsafe { bytes.as_ptr().add(8) }.cast();
    assert_eq!(
        passwd_path(bytes, pointer).unwrap(),
        Some(PathBuf::from("/home/fixture"))
    );
    assert!(passwd_path(bytes, std::ptr::null()).is_err());
    let unterminated = b"/home/no-terminator";
    assert!(passwd_path(unterminated, unterminated.as_ptr().cast()).is_err());
}

#[test]
fn environment_and_json_admission_refuse_bad_values_before_discovery() {
    for (key, value) in [("", "value"), ("bad=key", "x"), ("KEY", "bad\0value")] {
        let env = BTreeMap::from([(key.to_owned(), value.to_owned())]);
        assert!(resolve(&env, None).is_err());
    }
    let oversized = BTreeMap::from([("KEY".into(), "x".repeat(MAX_VALUE + 1))]);
    assert!(resolve(&oversized, None).is_err());
    let count: BTreeMap<String, String> = (0..MAX_FIELDS + 1)
        .map(|i| (format!("K{i}"), String::new()))
        .collect();
    assert!(resolve(&count, None).unwrap_err().contains("256 fields"));
    let bytes: BTreeMap<String, String> = (0..40)
        .map(|i| (format!("K{i}"), "x".repeat(MAX_VALUE)))
        .collect();
    assert!(resolve(&bytes, None).unwrap_err().contains("256 KiB"));
    for value in [
        serde_json::json!({"environment":{},"login_home":"forbidden"}),
        serde_json::json!({"environment":{"KEY":1}}),
        serde_json::json!({"environment":null}),
    ] {
        assert!(evaluate(&value).is_err());
    }
}

#[test]
fn json_operation_preserves_explicit_values_and_rejects_unbounded_paths() {
    let input = serde_json::json!({"environment":{"RUSTUP_HOME":"/explicit-rustup", "CARGO_HOME":"/explicit-cargo", "UNRELATED":"kept"}});
    assert_eq!(evaluate(&input).unwrap(), input["environment"]);
    let huge = BTreeMap::from([("HOME".into(), "a".repeat(MAX_PATH))]);
    assert!(resolve(&huge, None)
        .unwrap_err()
        .contains("path exceeds bounds"));
}

#[test]
fn settings_byte_boundary_and_permission_denied_fallback_are_explicit() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let home = f.rustup("home");
    let login = f.rustup("login");
    let mut bytes = b"default_toolchain".to_vec();
    bytes.resize(MAX_SETTINGS, b' ');
    fs::write(home.join("settings.toml"), &bytes).unwrap();
    assert!(usable_rustup(&home).unwrap());
    if unsafe { libc::geteuid() } != 0 {
        fs::set_permissions(home.join("settings.toml"), fs::Permissions::from_mode(0o0)).unwrap();
        let result = resolve(&f.env("home"), Some(&f.path.join("login")));
        fs::set_permissions(
            home.join("settings.toml"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert_eq!(result.unwrap()["RUSTUP_HOME"], login.to_str().unwrap());
    }
}
