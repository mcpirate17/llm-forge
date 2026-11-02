#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use git2::Repository;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn private_dir(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
struct Worker {
    path: PathBuf,
    sha: String,
}
fn worker() -> &'static Worker {
    static WORKER: OnceLock<Worker> = OnceLock::new();
    WORKER.get_or_init(|| {
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/candidate_fix_worker.rs");
        let expected = include_bytes!("support/candidate_fix_worker.rs");
        assert_eq!(
            fs::read(&source).unwrap().as_slice(),
            expected.as_slice(),
            "fixture source changed after test compilation"
        );
        let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("candidate-fix-operator-{}", std::process::id()));
        private_dir(&directory);
        let path = directory.join("ruff-fixture");
        let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .args([
                "--edition=2021",
                "--crate-name=forge_candidate_fix_fixture",
                "-C",
                "debuginfo=0",
            ])
            .arg(&source)
            .arg("-o")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(hash(&fs::read(&source).unwrap()), hash(expected));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"\x7fELF"));
        Worker {
            path: path.canonicalize().unwrap(),
            sha: hash(&bytes),
        }
    })
}
struct Case {
    host: support::Host,
    repo: PathBuf,
    scratch: PathBuf,
    grants: Value,
    tool: PathBuf,
    sha: String,
}
impl Case {
    fn new() -> Self {
        let host = support::Host::new();
        let repo = host.0.join("repo");
        private_dir(&repo);
        Repository::init(&repo).unwrap();
        let operator = host.0.join("operator");
        private_dir(&operator);
        let tool = operator.join("ruff");
        fs::copy("/bin/true", &tool).unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let sha = hash(&fs::read(&tool).unwrap());
        let scratch = host.0.join("scratch");
        private_dir(&scratch);
        private_dir(&host.0.join("cgroup"));
        let mut read_only = vec![repo.clone(), operator];
        for path in ["/usr", "/lib", "/lib64", "/etc/ld.so.cache"] {
            if let Ok(path) = Path::new(path).canonicalize() {
                read_only.push(path);
            }
        }
        read_only.sort();
        read_only.dedup();
        let grants = json!({"version":1,"source_root":repo,"read_only":read_only,
            "programs":{"ruff":tool},"environment":{},"scratch_parent":scratch,
            "cgroup_parent":host.0.join("cgroup"),
            "resources":{"cpu_millis":1000,"memory_mib":128,"processes":16},
            "max_wall_seconds":10,"max_cpu_seconds":10,"max_address_space_mib":512});
        let case = Self {
            host,
            repo,
            scratch,
            grants,
            tool,
            sha,
        };
        case.write("first.py", b"import unused\r\nx=1\r\n");
        case.write("second.py", b"y=2\n");
        case.write("guard.txt", b"outside selected write scope\n");
        case
    }
    fn write(&self, relative: &str, bytes: &[u8]) {
        fs::write(self.repo.join(relative), bytes).unwrap();
    }
    fn read(&self, relative: &str) -> Vec<u8> {
        fs::read(self.repo.join(relative)).unwrap()
    }
    fn command(&self) -> Command {
        let grants = self.host.0.join("grants.json");
        fs::write(&grants, serde_json::to_vec(&self.grants).unwrap()).unwrap();
        fs::set_permissions(&grants, fs::Permissions::from_mode(0o600)).unwrap();
        let mut command = Command::new(support::forge_binary::path());
        command
            .args(["candidate", "fix", "--repo"])
            .arg(&self.repo)
            .arg("--grants")
            .arg(grants)
            .arg("--ruff-sha256")
            .arg(&self.sha)
            .env("HOME", &self.host.0)
            .env("PATH", "")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        command
    }
    fn delegated(&mut self, mode: &str) {
        let worker = worker();
        assert_eq!(hash(&fs::read(&worker.path).unwrap()), worker.sha);
        self.tool = worker.path.clone();
        self.sha = worker.sha.clone();
        self.grants["programs"]["ruff"] = json!(self.tool);
        self.grants["read_only"]
            .as_array_mut()
            .unwrap()
            .push(json!(self.tool.parent().unwrap()));
        self.grants["cgroup_parent"] = json!(PathBuf::from(
            std::env::var_os("FORGE_TEST_CGROUP_PARENT")
                .expect("execution tests require FORGE_TEST_CGROUP_PARENT")
        ));
        self.grants["environment"]["FORGE_FIXTURE_MODE"] = json!(mode);
    }
    fn execute(&self, paths: &[&str]) -> Output {
        self.command()
            .args(["--execute", "--"])
            .args(paths)
            .output()
            .unwrap()
    }
    fn clean(&self) {
        assert_eq!(
            fs::read_dir(&self.scratch).unwrap().count(),
            0,
            "scratch survived cleanup"
        );
    }
    fn batch(&self) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        (
            self.read("first.py"),
            self.read("second.py"),
            self.read("guard.txt"),
        )
    }
}
fn report(output: Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn refused(output: Output, expected: &str) {
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn cleaned_stages(value: &Value) {
    for stage in value["stages"].as_array().unwrap() {
        assert_eq!(stage["cleanup_complete"], true, "{stage}");
        assert_eq!(stage["errors"], json!([]), "{stage}");
    }
}

#[test]
fn default_is_dry_admission_and_does_not_run_version_or_change_sources() {
    let case = Case::new();
    let before = case.batch();
    let value = report(
        case.command()
            .args(["--", "first.py", "second.py"])
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(value["operation"], "candidate_fix");
    assert_eq!(value["executed"], false);
    assert_eq!(value["supported_ruff"], "ruff 0.16.5");
    assert_eq!(value["ruff_sha256"], case.sha);
    assert_eq!(value["files"].as_array().unwrap().len(), 2);
    assert_eq!(value["files"][0]["source_sha256"], hash(&before.0));
    assert!(value["stages"].is_null());
    assert_eq!(case.batch(), before);
    case.clean();
}

#[test]
fn dry_admission_accepts_owned_paths_json_and_binds_configuration() {
    let case = Case::new();
    case.write("ruff.toml", b"line-length = 88\n");
    let paths = case.host.0.join("paths.json");
    fs::write(&paths, br#"["first.py","second.py"]"#).unwrap();
    let value = report(
        case.command()
            .arg("--paths-from")
            .arg(&paths)
            .output()
            .unwrap(),
        0,
    );
    assert_eq!(value["files"].as_array().unwrap().len(), 2);
    assert!(value["config_files"]
        .as_array()
        .unwrap()
        .contains(&json!(case.repo.join("ruff.toml"))));
    assert_eq!(value["executed"], false);
    case.clean();
}

#[test]
fn dry_admission_refuses_unbounded_duplicate_and_unsupported_paths() {
    let case = Case::new();
    for (paths, message) in [
        (vec![], "1..256"),
        (vec!["*.py"], "literal"),
        (vec!["first.py", "first.py"], "duplicate"),
        (vec!["guard.txt"], ".py/.pyi"),
        (vec!["../escape.py"], "path"),
    ] {
        refused(
            case.command().arg("--").args(paths).output().unwrap(),
            message,
        );
    }
    case.clean();
}

#[test]
fn dry_admission_refuses_bad_tool_pin_and_script_launcher() {
    let mut case = Case::new();
    case.sha = "0".repeat(64);
    refused(
        case.command().args(["--", "first.py"]).output().unwrap(),
        "digest",
    );
    fs::write(&case.tool, b"#!/usr/bin/python3\nprint('ruff 0.16.5')\n").unwrap();
    case.sha = hash(&fs::read(&case.tool).unwrap());
    refused(
        case.command().args(["--", "first.py"]).output().unwrap(),
        "native ELF",
    );
    case.clean();
}

#[test]
fn dry_admission_refuses_non_utf8_bom_nul_cookie_and_oversized_sources() {
    let case = Case::new();
    for (bytes, message) in [
        (vec![0xff], "UTF-8"),
        (b"\xef\xbb\xbfx=1\n".to_vec(), "BOM/NUL"),
        (b"x=1\0\n".to_vec(), "BOM/NUL"),
        (b"# coding: latin-1\nx=1\n".to_vec(), "coding cookie"),
        (vec![b'x'; 1024 * 1024 + 1], "bounded"),
    ] {
        case.write("first.py", &bytes);
        refused(
            case.command().args(["--", "first.py"]).output().unwrap(),
            message,
        );
        assert_eq!(case.read("first.py"), bytes);
    }
    case.clean();
}

#[test]
fn dry_admission_refuses_symlink_hardlink_readonly_and_unsafe_config() {
    let case = Case::new();
    std::os::unix::fs::symlink(case.repo.join("first.py"), case.repo.join("link.py")).unwrap();
    refused(
        case.command().args(["--", "link.py"]).output().unwrap(),
        "input",
    );
    fs::hard_link(case.repo.join("first.py"), case.repo.join("hard.py")).unwrap();
    refused(
        case.command().args(["--", "hard.py"]).output().unwrap(),
        "input",
    );
    fs::remove_file(case.repo.join("hard.py")).unwrap();
    fs::set_permissions(
        case.repo.join("first.py"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    refused(
        case.command().args(["--", "first.py"]).output().unwrap(),
        "writable",
    );
    fs::set_permissions(
        case.repo.join("first.py"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    case.write("ruff.toml", b"exclude = ['${HOME}/ignored']\n");
    refused(
        case.command().args(["--", "first.py"]).output().unwrap(),
        "interpolation",
    );
    case.clean();
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and Landlock ABI 6"]
fn confined_success_preserves_crlf_and_streams_diagnostics_only_to_stderr() {
    let mut case = Case::new();
    case.delegated("success");
    let guard = case.read("guard.txt");
    let output = case.execute(&["first.py", "second.py"]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("fixture diagnostic: check"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("fixture diagnostic: format"));
    let value = report(output, 0);
    assert_eq!(case.read("first.py"), b"x = 1\r\n");
    assert_eq!(case.read("second.py"), b"y = 2\n");
    assert_eq!(case.read("guard.txt"), guard);
    assert_eq!(value["stages"].as_array().unwrap().len(), 5);
    assert_eq!(value["edits"].as_array().unwrap().len(), 2);
    cleaned_stages(&value);
    case.clean();
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and Landlock ABI 6"]
fn every_failed_filter_keeps_entire_batch_exact_and_preserves_child_status() {
    for mode in ["partial-fail", "format-fail", "second-fail"] {
        let mut case = Case::new();
        case.delegated(mode);
        let before = case.batch();
        let value = report(case.execute(&["first.py", "second.py"]), 7);
        assert_eq!(
            value["stages"].as_array().unwrap().last().unwrap()["exit_code"],
            7
        );
        assert!(value["edits"].is_null());
        assert_eq!(case.batch(), before);
        cleaned_stages(&value);
        case.clean();
    }
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and Landlock ABI 6"]
fn fatal_child_signal_has_shell_status_without_source_publication() {
    let mut case = Case::new();
    case.delegated("signal");
    let before = case.batch();
    let value = report(case.execute(&["first.py"]), 128 + libc::SIGABRT);
    let stage = value["stages"].as_array().unwrap().last().unwrap();
    assert_eq!(stage["signal"], libc::SIGABRT);
    assert_eq!(case.batch(), before);
    cleaned_stages(&value);
    case.clean();
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and Landlock ABI 6"]
fn timeout_has_bounded_cleanup_and_keeps_source_exact() {
    let mut case = Case::new();
    case.delegated("timeout");
    case.grants["max_wall_seconds"] = json!(1);
    let before = case.batch();
    let started = Instant::now();
    let value = report(case.execute(&["first.py"]), 124);
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(
        value["stages"].as_array().unwrap().last().unwrap()["outcome"],
        "timed_out"
    );
    assert_eq!(case.batch(), before);
    cleaned_stages(&value);
    case.clean();
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and Landlock ABI 6"]
fn failed_keep_scratch_is_explicit_and_contains_no_source_edits() {
    let mut case = Case::new();
    case.delegated("partial-fail");
    let before = case.batch();
    let output = case
        .command()
        .args(["--execute", "--keep-failed", "--", "first.py"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("scratch retained"));
    let value = report(output, 7);
    cleaned_stages(&value);
    assert_eq!(case.batch(), before);
    assert_eq!(fs::read_dir(&case.scratch).unwrap().count(), 1);
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and Landlock ABI 6"]
fn sandbox_denies_writes_to_source_and_unselected_guard() {
    let mut case = Case::new();
    case.delegated("outside");
    case.grants["environment"]["FORGE_FIXTURE_OUTSIDE"] = json!(case.repo.join("guard.txt"));
    let guard = case.read("guard.txt");
    let output = case.execute(&["first.py"]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("out-of-scope writes denied"));
    let value = report(output, 0);
    assert_eq!(case.read("guard.txt"), guard);
    assert_eq!(case.read("first.py"), b"x = 1\r\n");
    cleaned_stages(&value);
    case.clean();
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and Landlock ABI 6"]
fn excluded_file_parrot_output_causes_no_empty_or_spurious_edit() {
    let mut case = Case::new();
    case.delegated("parrot");
    case.write(
        "ruff.toml",
        b"force-exclude = true\nexclude = ['first.py']\n",
    );
    let before = case.batch();
    let value = report(case.execute(&["first.py"]), 0);
    assert_eq!(value["edits"], json!([]));
    assert_eq!(case.batch(), before);
    cleaned_stages(&value);
    case.clean();
}

struct Running(Option<Child>);
impl Running {
    fn wait(mut self) -> Output {
        self.0.take().unwrap().wait_with_output().unwrap()
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn paused(case: &Case) -> (Running, PathBuf, i32) {
    let child = case
        .command()
        .args(["--execute", "--", "first.py", "second.py"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let running = Running(Some(child));
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        for entry in fs::read_dir(&case.scratch).unwrap() {
            let ready = entry.unwrap().path().join("ready");
            if let Ok(pid) = fs::read_to_string(&ready) {
                return (
                    running,
                    ready.parent().unwrap().to_owned(),
                    pid.parse().unwrap(),
                );
            }
        }
        assert!(
            Instant::now() < end,
            "worker never reached cooperative pause"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and Landlock ABI 6"]
fn parent_source_or_config_drift_refuses_publication_of_all_candidates() {
    for config in [0, 1, 2] {
        let mut case = Case::new();
        case.delegated("pause");
        if config != 2 {
            case.write("ruff.toml", b"line-length = 88\n");
        }
        let second = case.read("second.py");
        let (running, scratch, _) = paused(&case);
        let (path, replacement): (&str, &[u8]) = if config != 0 {
            ("ruff.toml", b"line-length = 99\n")
        } else {
            ("first.py", b"parent_change = True\r\n")
        };
        case.write(path, replacement);
        fs::write(scratch.join("release"), b"continue").unwrap();
        let output = running.wait();
        assert!(!output.status.success());
        assert_eq!(case.read(path), replacement);
        assert_eq!(case.read("second.py"), second);
        if config != 0 {
            assert_eq!(case.read("first.py"), b"import unused\r\nx=1\r\n");
        }
        case.clean();
    }
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT and Landlock ABI 6"]
fn interrupt_cancels_owned_child_and_cleans_before_cli_return() {
    let mut case = Case::new();
    case.delegated("pause");
    let before = case.batch();
    let (running, _, worker_pid) = paused(&case);
    let parent_pid = running.0.as_ref().unwrap().id() as i32;
    // SAFETY: target is the still-owned CLI child; SIGINT is its supported interface.
    assert_eq!(unsafe { libc::kill(parent_pid, libc::SIGINT) }, 0);
    let value = report(running.wait(), 130);
    assert_eq!(case.batch(), before);
    cleaned_stages(&value);
    case.clean();
    // SAFETY: signal zero only observes whether the fixture remains alive.
    assert_eq!(unsafe { libc::kill(worker_pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[test]
#[ignore = "requires FORGE_TEST_CGROUP_PARENT, Landlock ABI 6 and explicit FORGE_TEST_RUFF 0.16.5"]
fn real_ruff_discovers_config_and_force_exclusion_preserves_selected_file() {
    let mut case = Case::new();
    let original = PathBuf::from(
        std::env::var_os("FORGE_TEST_RUFF").expect("explicit absolute Ruff path required"),
    );
    assert!(original.is_absolute());
    assert!(fs::read(&original).unwrap().starts_with(b"\x7fELF"));
    fs::copy(&original, &case.tool).unwrap();
    fs::set_permissions(&case.tool, fs::Permissions::from_mode(0o755)).unwrap();
    case.sha = hash(&fs::read(&case.tool).unwrap());
    case.grants["cgroup_parent"] = json!(PathBuf::from(
        std::env::var_os("FORGE_TEST_CGROUP_PARENT").unwrap()
    ));
    case.write("ruff.toml", b"line-length = 88\nforce-exclude = true\nexclude = ['second.py']\n[lint]\nselect = ['F401']\n[format]\nquote-style = 'single'\nline-ending = 'cr-lf'\n");
    case.write("first.py", b"import os\r\nx=\"hello\"\r\n");
    case.write("second.py", b"import os\ny=\"excluded\"\n");
    let excluded = case.read("second.py");
    let value = report(case.execute(&["first.py", "second.py"]), 0);
    assert_eq!(case.read("first.py"), b"x = 'hello'\r\n");
    assert_eq!(case.read("second.py"), excluded);
    cleaned_stages(&value);
    case.clean();
}

#[test]
fn operator_cannot_override_private_configuration_environment() {
    for variable in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_CONFIG_DIRS",
        "XDG_CACHE_HOME",
    ] {
        let mut case = Case::new();
        case.grants["environment"][variable] = json!(case.host.0);
        refused(
            case.command().args(["--", "first.py"]).output().unwrap(),
            "reserved workspace environment",
        );
        case.clean();
    }
}
