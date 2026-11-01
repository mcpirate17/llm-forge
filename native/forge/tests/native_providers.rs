#[allow(dead_code)]
mod support;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

fn host() -> support::Host {
    let host = support::Host::new();
    fs::set_permissions(&host.0, fs::Permissions::from_mode(0o700)).unwrap();
    host
}

fn command(host: &support::Host, provider: &str, args: &[&str]) -> Output {
    host.command()
        .env("PATH", "")
        .args(["providers", "--provider", provider])
        .args(args)
        .output()
        .unwrap()
}
fn parsed(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn fail(output: Output, fragment: &str) {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains(fragment), "{message}");
}
fn write(path: &Path, text: &str) {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path.parent().unwrap())
        .unwrap();
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();
}
fn backup(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.forge-native.bak.json", path.display()))
}

#[test]
fn all_provider_plans_install_and_check_without_python_or_service_startup() {
    for (provider, relative, count, event, timeout) in [
        ("claude", ".claude/settings.json", 5, "SessionStart", 20),
        ("codex", ".codex/hooks.json", 5, "SessionStart", 20),
        ("qwen", ".qwen/settings.json", 1, "SessionStart", 15000),
        (
            "grok",
            ".grok/hooks/workspace.json",
            1,
            "UserPromptSubmit",
            15,
        ),
    ] {
        let host = host();
        let path = host.0.join(relative);
        let plan = parsed(command(&host, provider, &["install"]));
        assert_eq!(plan["applied"], false);
        assert_eq!(plan["changed"], true);
        assert!(!path.parent().unwrap().exists());
        let applied = parsed(command(&host, provider, &["install", "--apply"]));
        assert_eq!(applied["after"], plan["after"]);
        let config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["hooks"].as_object().unwrap().len(), count);
        assert_eq!(config["hooks"][event][0]["hooks"][0]["timeout"], timeout);
        let cmd = config["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(cmd.contains(host.0.to_str().unwrap()));
        assert!(!cmd.contains("python"));
        assert_eq!(
            parsed(command(&host, provider, &["check"]))["configured"],
            true
        );
        assert_eq!(
            parsed(command(&host, provider, &["install", "--apply"]))["changed"],
            false
        );
        assert!(!host.0.join(".agents").exists());
        parsed(command(&host, provider, &["rollback", "--apply"]));
        assert!(!path.exists());
        assert_eq!(
            parsed(command(&host, provider, &["rollback", "--apply"]))["changed"],
            false
        );
    }
}

#[test]
fn legacy_replacement_is_explicit_preserves_mixed_groups_and_exact_rollback() {
    let host = host();
    let path = host.0.join(".codex/hooks.json");
    let original = "{\n \"model\":\"keep\",\"hooks\":{\"SessionStart\":[{\"custom\":7,\"hooks\":[{\"type\":\"command\",\"command\":\"python -m conductor.a2a_session_start --provider codex\"},{\"type\":\"command\",\"command\":\"echo unrelated\"}]}]}}\n";
    write(&path, original);
    fail(
        command(&host, "codex", &["install", "--apply"]),
        "--replace-legacy",
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert!(!backup(&path).exists());
    parsed(command(
        &host,
        "codex",
        &["install", "--replace-legacy", "--apply"],
    ));
    let config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(config["model"], "keep");
    assert_eq!(config["hooks"]["SessionStart"][0]["custom"], 7);
    assert_eq!(
        config["hooks"]["SessionStart"][0]["hooks"],
        json!([{"type":"command","command":"echo unrelated"}])
    );
    assert!(!fs::read_to_string(&path).unwrap().contains("python"));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(
        fs::metadata(backup(&path)).unwrap().permissions().mode() & 0o777,
        0o600
    );
    parsed(command(&host, "codex", &["rollback", "--apply"]));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn uninstall_retains_unrelated_commands_and_rollback_refuses_later_edits() {
    let host = host();
    let path = host.0.join(".qwen/settings.json");
    write(
        &path,
        r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo preserve"}]}]},"theme":"original"}"#,
    );
    parsed(command(&host, "qwen", &["install", "--apply"]));
    let installed = fs::read(&path).unwrap();
    parsed(command(&host, "qwen", &["uninstall", "--apply"]));
    let config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(config["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    assert_eq!(
        config["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        "echo preserve"
    );
    assert_eq!(config["theme"], "original");
    let saved_backup = fs::read(backup(&path)).unwrap();
    fs::write(&path, "{\"theme\":\"user edit\"}\n").unwrap();
    fail(
        command(&host, "qwen", &["rollback", "--apply"]),
        "changed after",
    );
    assert_eq!(fs::read(backup(&path)).unwrap(), saved_backup);
    let saved: Value = serde_json::from_slice(&saved_backup).unwrap();
    fs::write(&path, saved["after"]["text"].as_str().unwrap()).unwrap();
    parsed(command(&host, "qwen", &["rollback", "--apply"]));
    assert_eq!(fs::read(&path).unwrap(), installed);
}

#[test]
fn ownership_does_not_capture_shell_snippets_unrelated_modules_or_foreign_provider() {
    let host = host();
    let path = host.0.join(".codex/hooks.json");
    let unrelated = [
        "printf python -m conductor.a2a_session_start --provider codex",
        "python -m other -m conductor.a2a_session_start --provider codex",
        "python -m conductor.a2a_session_start --provider grok",
        "forge hook SessionStart; echo keep",
        "forge hook SessionStart --unknown x",
        "forge hook SessionStart --host '$HOME' && echo keep",
        "env -u PYTHONPATH forge hook SessionStart",
        "env CUSTOM=keep forge hook SessionStart",
        "env FORGE_MODE=warn FORGE_MODE=enforce forge hook SessionStart",
    ];
    let hooks: Vec<_> = unrelated
        .iter()
        .map(|command| json!({"type":"command","command":command}))
        .collect();
    write(
        &path,
        &json!({"hooks":{"SessionStart":[{"hooks":hooks.clone()}]}}).to_string(),
    );
    parsed(command(
        &host,
        "codex",
        &["install", "--apply", "--replace-legacy"],
    ));
    parsed(command(&host, "codex", &["uninstall", "--apply"]));
    let config: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(config["hooks"]["SessionStart"][0]["hooks"], json!(hooks));
}

#[test]
fn historical_project_init_env_launcher_is_replaced_without_duplicate_execution() {
    let host = host();
    let path = host.0.join(".claude/settings.json");
    let old = "env CONDUCTOR_PYTHON=/old/venv/bin/python /old/bin/forge hook SessionStart";
    let original = json!({"hooks":{"SessionStart":[{"hooks":[
        {"type":"command","command":old},
        {"type":"command","command":"echo custom"}
    ]}]}})
    .to_string();
    write(&path, &original);
    parsed(command(&host, "claude", &["install", "--apply"]));
    let config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let commands: Vec<_> = config["hooks"]["SessionStart"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["hooks"].as_array().unwrap())
        .map(|hook| hook["command"].as_str().unwrap())
        .collect();
    assert_eq!(commands.len(), 2);
    assert!(commands.contains(&"echo custom"));
    assert!(!commands.contains(&old));
    parsed(command(&host, "claude", &["rollback", "--apply"]));
    assert_eq!(fs::read_to_string(path).unwrap(), original);
}

#[test]
fn malformed_duplicate_oversized_and_unreadable_configs_are_never_reset() {
    for text in [
        "{".to_owned(),
        "[]".to_owned(),
        r#"{"hooks":{},"hooks":{}}"#.to_owned(),
        r#"{"hooks":{"SessionStart":{}}}"#.to_owned(),
        " ".repeat(1024 * 1024 + 1),
    ] {
        let host = host();
        let path = host.0.join(".claude/settings.json");
        write(&path, &text);
        let output = command(&host, "claude", &["install", "--apply"]);
        assert!(!output.status.success());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert!(!backup(&path).exists());
    }
}

#[test]
fn config_parents_targets_backups_and_locks_reject_unsafe_files() {
    for target in [
        "settings.json",
        "settings.json.forge-native.bak.json",
        ".forge-provider.lock",
    ] {
        let host = host();
        let path = host.0.join(".claude/settings.json");
        let sentinel = host.0.join("sentinel");
        write(&sentinel, "{}");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path.parent().unwrap())
            .unwrap();
        let unsafe_path = path.parent().unwrap().join(target);
        std::os::unix::fs::symlink(&sentinel, &unsafe_path).unwrap();
        assert!(!command(&host, "claude", &["install", "--apply"])
            .status
            .success());
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "{}");
        fs::remove_file(&unsafe_path).unwrap();
        fs::hard_link(&sentinel, &unsafe_path).unwrap();
        assert!(!command(&host, "claude", &["install", "--apply"])
            .status
            .success());
        assert_eq!(fs::read_to_string(&sentinel).unwrap(), "{}");
    }
    let host = host();
    fs::create_dir(host.0.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(host.0.join("elsewhere"), host.0.join(".claude")).unwrap();
    fail(
        command(&host, "claude", &["install", "--apply"]),
        "nonsymlink",
    );
    assert_eq!(fs::read_dir(host.0.join("elsewhere")).unwrap().count(), 0);
}

#[test]
fn busy_writer_and_fifo_targets_fail_without_blocking() {
    let host = host();
    let path = host.0.join(".claude/settings.json");
    write(&path, "{}");
    let lock = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path.parent().unwrap().join(".forge-provider.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    fail(command(&host, "claude", &["install", "--apply"]), "busy");
    assert_eq!(fs::read_to_string(&path).unwrap(), "{}");
    assert!(!backup(&path).exists());
    fs::remove_file(&path).unwrap();
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let start = std::time::Instant::now();
    fail(command(&host, "claude", &["install", "--apply"]), "regular");
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn generated_command_quotes_host_paths_and_executes_bound_native_guard() {
    let host = host();
    let root = host.0.join("with ' space;$HOME");
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    parsed(command(
        &host,
        "codex",
        &[
            "--host",
            root.to_str().unwrap(),
            "install",
            "--mode",
            "enforce",
            "--apply",
        ],
    ));
    let config: Value =
        serde_json::from_slice(&fs::read(root.join(".codex/hooks.json")).unwrap()).unwrap();
    let command_text = config["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    let mut child = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command_text)
        .env("PATH", "")
        .current_dir(&host.0)
        .env("CLAUDE_PROJECT_DIR", "/nonexistent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"tool_name":"Bash","tool_input":{"command":"git reset --hard"}}"#)
        .unwrap();
    let output = parsed(child.wait_with_output().unwrap());
    assert_eq!(output["hookSpecificOutput"]["permissionDecision"], "deny");
    parsed(command(
        &host,
        "codex",
        &[
            "--host",
            root.to_str().unwrap(),
            "check",
            "--mode",
            "enforce",
        ],
    ));
}

#[test]
fn check_detects_command_drift_and_binary_override_requires_native_executable() {
    let host = host();
    parsed(command(
        &host,
        "grok",
        &["install", "--identity", "fixture", "--apply"],
    ));
    fail(command(&host, "grok", &["check"]), "differ");
    parsed(command(&host, "grok", &["check", "--identity", "fixture"]));
    let fake = host.0.join("forge");
    write(&fake, "#!/bin/sh\nexit 0\n");
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    fail(
        command(
            &host,
            "grok",
            &["install", "--binary", fake.to_str().unwrap(), "--apply"],
        ),
        "native ELF",
    );
    assert!(!host.0.join(".agents").exists());
}
