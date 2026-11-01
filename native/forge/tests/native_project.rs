#[allow(dead_code)]
mod support;
use serde_json::Value;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::process::Output;

fn host() -> support::Host {
    let host = support::Host::new();
    fs::set_permissions(&host.0, fs::Permissions::from_mode(0o700)).unwrap();
    host
}

fn command(host: &support::Host, args: &[&str]) -> Output {
    host.command()
        .env("PATH", "")
        .env("CONDUCTOR_PYTHON", "/nonexistent/interpreter")
        .env_remove("CONDUCTOR_CANDIDATE_POLICY")
        .env_remove("CONDUCTOR_INTEGRATION_BRANCH")
        .args(["project", "--host"])
        .arg(&host.0)
        .args(args)
        .output()
        .unwrap()
}

fn parsed(output: Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn fail(output: Output, fragment: &str) {
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(fragment), "{stderr}");
}

fn write(path: &std::path::Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn dry_run_has_no_side_effects_and_apply_rollback_are_idempotent_without_interpreter() {
    let host = host();
    let plan = parsed(command(&host, &["init"]));
    assert_eq!(plan["applied"], false);
    assert_eq!(plan["changed"], true);
    assert_eq!(plan["creates_services"], false);
    assert_eq!(fs::read_dir(&host.0).unwrap().count(), 1);
    let applied = parsed(command(&host, &["init", "--apply"]));
    assert_eq!(applied["after"], plan["after"]);
    let path = host.0.join("forge.toml");
    let text = fs::read(&path).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        parsed(command(&host, &["init", "--apply"]))["changed"],
        false
    );
    assert_eq!(parsed(command(&host, &["show"]))["namespace"], "conductor");
    assert_eq!(parsed(command(&host, &["rollback"]))["changed"], true);
    assert_eq!(fs::read(&path).unwrap(), text);
    parsed(command(&host, &["rollback", "--apply"]));
    assert!(!path.exists());
    assert_eq!(
        parsed(command(&host, &["rollback", "--apply"]))["changed"],
        false
    );
    assert!(!host.0.join(".agents").exists());
    assert!(!host.0.join(".mcp.json").exists());
}

#[test]
fn legacy_host_settings_are_copied_without_python_package_metadata_or_rewrites() {
    let host = host();
    let legacy = "[project]\nname='unrelated'\n[tool.conductor]\ncandidate_policy='policy/custom.toml'\nintegration_branch='trunk'\n[tool.conductor.session]\npreamble=['keep literal $HOME']\nstanding_mandates=['keep this']\n";
    write(&host.0.join("pyproject.toml"), legacy);
    parsed(command(&host, &["init", "--apply"]));
    let native: toml::Value = fs::read_to_string(host.0.join("forge.toml"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(native["schema_version"].as_integer(), Some(1));
    assert!(native.get("project").is_none());
    assert_eq!(
        native["conductor"]["session"]["preamble"][0].as_str(),
        Some("keep literal $HOME")
    );
    let shown = parsed(command(&host, &["show"]));
    assert_eq!(shown["integration_branch"], "trunk");
    assert_eq!(shown["paths"][0][0], "policy/custom.toml");
    assert!(!shown.to_string().contains("keep literal"));
    assert_eq!(
        fs::read_to_string(host.0.join("pyproject.toml")).unwrap(),
        legacy
    );
}

#[test]
fn existing_native_manifest_keeps_bytes_mode_and_does_not_read_legacy() {
    let host = host();
    let path = host.0.join("forge.toml");
    let text = "# Keep my comments\nschema_version = 1\n[conductor]\nintegration_branch='main'\n";
    fs::write(&path, text).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    std::os::unix::fs::symlink("missing", host.0.join("pyproject.toml")).unwrap();
    assert_eq!(
        parsed(command(&host, &["init", "--apply"]))["changed"],
        false
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert!(!host.0.join("forge.toml.forge-native.bak.json").exists());
    assert!(!host.0.join(".forge-project.lock").exists());
}

#[test]
fn rollback_refuses_user_edits_and_untrusted_backup_without_changing_either() {
    let host = host();
    parsed(command(&host, &["init", "--apply"]));
    let path = host.0.join("forge.toml");
    let original = fs::read(&path).unwrap();
    let backup = host.0.join("forge.toml.forge-native.bak.json");
    let saved = fs::read(&backup).unwrap();
    let edited = "schema_version=1\n[conductor]\nintegration_branch='user'\n";
    fs::write(&path, edited).unwrap();
    fail(command(&host, &["rollback", "--apply"]), "changed after");
    assert_eq!(fs::read_to_string(&path).unwrap(), edited);
    assert_eq!(fs::read(&backup).unwrap(), saved);
    fs::write(&path, &original).unwrap();
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o644)).unwrap();
    fail(
        command(&host, &["rollback", "--apply"]),
        "backup must be private",
    );
    assert_eq!(fs::read(&path).unwrap(), original);
}

#[test]
fn invalid_or_oversized_manifests_fail_without_publication() {
    for (relative, text, error) in [
        (
            "forge.toml",
            "schema_version=2".to_owned(),
            "schema_version",
        ),
        (
            "forge.toml",
            "schema_version=1\nconductor=[]".to_owned(),
            "not a table",
        ),
        (
            "forge.toml",
            "schema_version=1\nschema_version=1".to_owned(),
            "invalid TOML",
        ),
        (
            "pyproject.toml",
            "[tool]\nconductor='bad'".to_owned(),
            "not a table",
        ),
        ("pyproject.toml", " ".repeat(65537), "exceeds 64 KiB"),
    ] {
        let host = host();
        let path = host.0.join(relative);
        write(&path, &text);
        fail(command(&host, &["init", "--apply"]), error);
        assert_eq!(fs::read_to_string(path).unwrap(), text);
        assert!(!host.0.join("forge.toml.forge-native.bak.json").exists());
        assert!(!host.0.join(".forge-project.lock").exists());
    }
}

#[test]
fn invalid_declared_settings_cannot_be_persisted_even_with_an_environment_override() {
    for text in [
        "[tool.conductor]\ncandidate_policy='../escape'",
        "[tool.conductor]\nnotes_root=12",
        "[tool.conductor]\nintegration_branch=''",
        "[tool.conductor]\nworktree_patterns=[1]",
        "[tool.conductor.session]\npreamble=['x']",
        "[tool.conductor.session]\npreamble=[1]\nstanding_mandates=[]",
    ] {
        let host = host();
        write(&host.0.join("pyproject.toml"), text);
        let output = host
            .command()
            .env("PATH", "")
            .env("CONDUCTOR_CANDIDATE_POLICY", "valid.toml")
            .args(["project", "init", "--apply"])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
        assert!(!host.0.join("forge.toml").exists());
        assert!(!host.0.join("forge.toml.forge-native.bak.json").exists());
    }
}

#[test]
fn symlinks_fifos_and_insecure_host_are_rejected_without_following_or_blocking() {
    let host = host();
    let path = host.0.join("forge.toml");
    std::os::unix::fs::symlink("missing", &path).unwrap();
    fail(command(&host, &["init", "--apply"]), "read");
    fs::remove_file(&path).unwrap();
    let cpath = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    fail(command(&host, &["init", "--apply"]), "regular");
    fs::remove_file(&path).unwrap();
    fs::set_permissions(&host.0, fs::Permissions::from_mode(0o777)).unwrap();
    fail(
        command(&host, &["init", "--apply"]),
        "not writable by others",
    );
    assert!(!path.exists());
}

#[test]
fn competing_writer_lock_fails_before_creating_manifest_or_backup() {
    let host = host();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(host.0.join(".forge-project.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    fail(command(&host, &["init", "--apply"]), "busy");
    assert!(!host.0.join("forge.toml").exists());
    assert!(!host.0.join("forge.toml.forge-native.bak.json").exists());
    drop(lock);
    parsed(command(&host, &["init", "--apply"]));
}
