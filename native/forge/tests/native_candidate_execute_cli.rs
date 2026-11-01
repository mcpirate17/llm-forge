#![cfg(target_os = "linux")]
#[path = "support/candidate_policy_fixture.rs"]
#[allow(dead_code)]
mod candidate_policy_fixture;
#[allow(dead_code)]
mod support;
use candidate_policy_fixture::{policy, Fixture};
use git2::{Repository, Signature};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

struct Case {
    fixture: Fixture,
    grants: Value,
}
impl Case {
    fn new(program: &Path) -> Self {
        let fixture = Fixture::new();
        let repository = Repository::open(fixture.root.join("repo")).unwrap();
        fixture.stage("baseline.txt", "baseline bytes\n");
        let oid = repository.index().unwrap().write_tree().unwrap();
        let tree = repository.find_tree(oid).unwrap();
        let author = Signature::now("fixture", "fixture@example.invalid").unwrap();
        repository
            .commit(Some("HEAD"), &author, &author, "initial", &tree, &[])
            .unwrap();
        fixture.stage("src/security/test_unit.rs", "fn selected() {}\n");
        let policy = policy("zeta")
            .replace(
                "kind='command'",
                "kind='command'\ntimeout_seconds=2\nmemory_mb=128",
            )
            .replace(
                "\ncommand=['/must-never-be-executed']",
                "\ncommand=['worker','echo','literal argument','{files}']",
            )
            .replace(
                "version_command=['/must-never-be-executed']",
                "version_command=['worker','echo','fixture-v1']",
            );
        fixture.stage("policies/main.toml", &policy);
        std::fs::create_dir(fixture.root.join("cgroup")).unwrap();
        let mut read_only = vec![
            fixture.root.join("repo").canonicalize().unwrap(),
            program.parent().unwrap().canonicalize().unwrap(),
        ];
        for directory in ["/usr", "/lib", "/lib64"] {
            if let Ok(path) = Path::new(directory).canonicalize() {
                read_only.push(path);
            }
        }
        read_only.sort();
        read_only.dedup();
        let grants = json!({"version":1,"source_root":fixture.root.join("repo").canonicalize().unwrap(),
            "read_only":read_only,"programs":{"worker":program.canonicalize().unwrap()},"environment":{},
            "scratch_parent":fixture.root.join("scratch"),"cgroup_parent":fixture.root.join("cgroup"),
            "resources":{"cpu_millis":1000,"memory_mib":256,"processes":16},
            "max_wall_seconds":10,"max_cpu_seconds":10,"max_address_space_mib":512,"python":null});
        Self { fixture, grants }
    }
    fn command(&self) -> Command {
        self.command_kind("index")
    }
    fn command_kind(&self, kind: &str) -> Command {
        let bytes = serde_json::to_vec(&self.grants).unwrap();
        std::fs::write(self.fixture.root.join("grants.json"), bytes).unwrap();
        std::fs::set_permissions(
            self.fixture.root.join("grants.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
        command
            .args(["candidate", "execute", "--repo"])
            .arg(self.fixture.root.join("repo"))
            .args([
                "--kind",
                kind,
                "--target-ref",
                "HEAD",
                "--policy",
                "policies/main.toml",
                "--profile",
                "fast",
                "--today",
                "2030-01-10",
                "--check",
                "zeta",
                "--grants",
            ])
            .arg(self.fixture.root.join("grants.json"))
            .env("PATH", "")
            .env("HOME", &self.fixture.root)
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("CONDUCTOR_POLICY")
            .env_remove("CONDUCTOR_CANDIDATE_POLICY")
            .env_remove("CONDUCTOR_INTEGRATION_REF")
            .env_remove("CONDUCTOR_INTEGRATION_BRANCH");
        command
    }
    fn staged_policy(&self, transform: impl FnOnce(String) -> String) {
        let path = self.fixture.root.join("repo/policies/main.toml");
        let content = std::fs::read_to_string(path).unwrap();
        self.fixture
            .stage("policies/main.toml", &transform(content));
    }
    fn source_state(&self) -> (Vec<u8>, Vec<u8>, String) {
        let root = self.fixture.root.join("repo");
        let repository = Repository::open(&root).unwrap();
        let head = repository.head().unwrap().target().unwrap().to_string();
        (
            std::fs::read(root.join(".git/index")).unwrap(),
            std::fs::read(root.join(".git/HEAD")).unwrap(),
            head,
        )
    }
    fn assert_preserved(&self, before: &(Vec<u8>, Vec<u8>, String)) {
        assert_eq!(&self.source_state(), before);
        self.fixture.clean_scratch();
    }
    fn delegated(&mut self) {
        self.grants["cgroup_parent"] = json!(std::path::PathBuf::from(
            std::env::var_os("FORGE_TEST_CGROUP_PARENT")
                .expect("ignored execution tests require FORGE_TEST_CGROUP_PARENT")
        ));
    }
}
fn value(output: Output, code: i32) -> Value {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn dry_run_empty_path_uses_staged_policy_and_never_executes_or_changes_source() {
    let case = Case::new(Path::new("/bin/true"));
    let before = case.source_state();
    let staged = std::fs::read(case.fixture.root.join("repo/policies/main.toml")).unwrap();
    std::fs::write(
        case.fixture.root.join("repo/policies/main.toml"),
        "invalid dirty policy",
    )
    .unwrap();
    let report = value(case.command().output().unwrap(), 0);
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["executed"], false);
    assert_eq!(report["check"]["check_id"], "zeta");
    assert_eq!(report["policy"]["path"], "policies/main.toml");
    assert_eq!(
        report["policy"]["sha256"],
        format!("{:x}", Sha256::digest(&staged))
    );
    assert_eq!(
        report["grants_sha256"],
        format!(
            "{:x}",
            Sha256::digest(std::fs::read(case.fixture.root.join("grants.json")).unwrap())
        )
    );
    assert_eq!(report["files"], json!(["src/security/test_unit.rs"]));
    assert!(report["result"].is_null());
    let stages = report["stages"].as_array().unwrap();
    assert_eq!(stages.len(), 2);
    assert_eq!(stages[0]["stage"], "version");
    assert_eq!(stages[1]["stage"], "command");
    for stage in stages {
        assert!(stage["report"].is_null());
        assert!(stage["sandbox"].is_object());
        assert_eq!(stage["sandbox"]["working_directory"], stage["snapshot"]);
        assert!(stage["limits"].is_object());
    }
    assert_eq!(
        std::fs::read(case.fixture.root.join("repo/policies/main.toml")).unwrap(),
        b"invalid dirty policy"
    );
    case.assert_preserved(&before);
}

#[test]
fn missing_granted_target_and_ungranted_alias_fail_before_execution() {
    let mut case = Case::new(Path::new("/bin/true"));
    case.staged_policy(|text| text.replace("'worker'", "'not-granted'"));
    let before = case.source_state();
    let output = case.command().output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("grant"));
    case.assert_preserved(&before);
    case.staged_policy(|text| text.replace("'not-granted'", "'worker'"));
    let before = case.source_state();
    case.grants["programs"]["worker"] = json!(case.fixture.root.join("missing-program"));
    assert!(!case.command().output().unwrap().status.success());
    case.assert_preserved(&before);
}

#[test]
fn expected_tree_mismatch_refuses_and_policy_file_filter_skips_without_launch() {
    let case = Case::new(Path::new("/bin/true"));
    let output = case
        .command()
        .args(["--expected-tree", &"0".repeat(40)])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("tree"));
    case.fixture.clean_scratch();
    case.staged_policy(|text| text.replace("classes=['rust']", "classes=['cuda']"));
    let before = case.source_state();
    let report = value(case.command().arg("--execute").output().unwrap(), 0);
    assert_eq!(report["skipped"], true);
    assert_eq!(report["files"], json!([]));
    assert_eq!(report["stages"], json!([]));
    assert!(report["result"].is_null());
    case.assert_preserved(&before);
}

#[test]
#[ignore = "requires explicit delegated cgroup v2 parent in FORGE_TEST_CGROUP_PARENT"]
fn delegated_execution_runs_version_then_command_and_preserves_source() {
    let mut case = Case::new(support::task_worker());
    case.delegated();
    let before = case.source_state();
    let report = value(case.command().arg("--execute").output().unwrap(), 0);
    assert_eq!(report["executed"], true);
    assert_eq!(report["result"]["status"], "passed");
    assert_eq!(report["result"]["tool_version"], "fixture-v1");
    assert!(report["result"]["stdout_tail"]
        .as_str()
        .unwrap()
        .contains("literal argument"));
    for stage in report["stages"].as_array().unwrap() {
        assert_eq!(stage["report"]["outcome"], "exited");
        assert_eq!(stage["report"]["cleanup_complete"], true);
        assert_eq!(stage["report"]["resources"]["verified"], true);
    }
    case.assert_preserved(&before);
}

#[test]
#[ignore = "requires explicit delegated cgroup v2 parent in FORGE_TEST_CGROUP_PARENT"]
fn unavailable_version_blocks_main_command() {
    let mut case = Case::new(support::task_worker());
    case.delegated();
    case.staged_policy(|text| {
        text.replace(
            "version_command=['worker','echo','fixture-v1']",
            "version_command=['worker','exit','7']",
        )
        .replace(
            "command=['worker','echo','literal argument','{files}']",
            "command=['worker','pid-sleep','10000','main-must-not-run.pid']",
        )
    });
    let before = case.source_state();
    let report = value(case.command().arg("--execute").output().unwrap(), 1);
    assert_eq!(
        report["result"]["findings"][0]["rule_id"],
        "required-analyzer-unavailable"
    );
    let observed: Vec<_> = report["stages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|stage| !stage["report"].is_null())
        .collect();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0]["stage"], "version");
    case.assert_preserved(&before);
}

#[test]
#[ignore = "requires explicit delegated cgroup v2 parent in FORGE_TEST_CGROUP_PARENT"]
fn nonzero_analyzer_yields_configured_finding_and_fail_exit() {
    let mut case = Case::new(support::task_worker());
    case.delegated();
    case.staged_policy(|text| {
        text.replace(
            "command=['worker','echo','literal argument','{files}']",
            "command=['worker','exit','3']",
        )
        .replace("kind='command'", "kind='command'\nseverity='medium'")
    });
    let before = case.source_state();
    let report = value(case.command().arg("--execute").output().unwrap(), 1);
    assert_eq!(report["result"]["status"], "failed");
    assert_eq!(
        report["result"]["findings"][0]["rule_id"],
        "analyzer-finding"
    );
    assert_eq!(report["result"]["findings"][0]["severity"], "medium");
    assert_eq!(report["result"]["exit_code"], 3);
    case.assert_preserved(&before);
}

#[test]
fn commit_dry_run_uses_committed_tree_despite_new_staged_and_dirty_changes() {
    let case = Case::new(Path::new("/bin/true"));
    let repository = Repository::open(case.fixture.root.join("repo")).unwrap();
    let tree_oid = repository.index().unwrap().write_tree().unwrap();
    let tree = repository.find_tree(tree_oid).unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let author = Signature::now("fixture", "fixture@example.invalid").unwrap();
    repository
        .commit(
            Some("HEAD"),
            &author,
            &author,
            "candidate",
            &tree,
            &[&parent],
        )
        .unwrap();
    case.fixture
        .stage("src/security/test_unit.rs", "fn staged_later() {}\n");
    std::fs::write(
        case.fixture.root.join("repo/policies/main.toml"),
        "invalid dirty policy",
    )
    .unwrap();
    let before = case.source_state();
    let report = value(case.command_kind("commit").output().unwrap(), 0);
    assert_eq!(report["candidate"]["tree_oid"], tree_oid.to_string());
    assert_eq!(report["executed"], false);
    assert!(report["result"].is_null());
    case.assert_preserved(&before);
}

/// Emergency cleanup for this test's CLI and native non-forking worker only.
/// A pidfd pins worker identity; an owned cgroup.kill FD covers its group.
struct InterruptedCli {
    child: std::process::Child,
    worker: Option<std::os::fd::OwnedFd>,
    group_kill: Option<std::fs::File>,
    group_path: Option<std::path::PathBuf>,
}
impl Drop for InterruptedCli {
    fn drop(&mut self) {
        use std::{io::Write, os::fd::AsRawFd};
        let worker_live = self.worker.as_ref().is_some_and(|fd| !worker_exited(fd));
        if let Some(file) = self.group_kill.as_mut().filter(|_| worker_live) {
            if let Err(error) = file.write_all(b"1") {
                eprintln!("test emergency cgroup cleanup: {error}");
            }
        }
        if let Some(fd) = self.worker.as_ref().filter(|_| worker_live) {
            // SAFETY: signal targets the identity retained by an owned pidfd.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            if result != 0 {
                eprintln!(
                    "test emergency worker cleanup: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        if let Err(error) = self.child.kill() {
            eprintln!("test emergency CLI cleanup: {error}");
        }
        if let Err(error) = self.child.wait() {
            eprintln!("test emergency CLI reap: {error}");
        }
    }
}
fn ready_worker(case: &Case) -> Option<u32> {
    use std::io::Read;
    let entries = std::fs::read_dir(case.fixture.root.join("scratch")).unwrap();
    for (index, entry) in entries.enumerate() {
        assert!(index < 16, "unexpected scratch entry count");
        let entry = entry.unwrap();
        // Snapshot ownership retains the unpublished random staging name.
        // Inspect only the two levels owned by this private fixture.
        let mut ready = None;
        for (index, child) in std::fs::read_dir(entry.path()).unwrap().enumerate() {
            assert!(index < 16, "unexpected stage entry count");
            let child = child.unwrap();
            if child.file_type().unwrap().is_dir() {
                let path = child.path().join("ready.pid");
                if path.is_file() {
                    ready = Some(path);
                    break;
                }
            }
        }
        let Some(path) = ready else {
            continue;
        };
        let raw = std::fs::File::open(path).and_then(|file| {
            let mut value = String::new();
            file.take(33).read_to_string(&mut value)?;
            Ok(value)
        });
        match raw {
            Ok(value) if value.len() <= 32 => {
                if let Ok(pid) = value.parse() {
                    return Some(pid);
                }
            }
            Ok(_) => panic!("oversized readiness PID"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => panic!("read readiness: {error}"),
        }
    }
    None
}
fn pin_worker(running: &mut InterruptedCli, case: &Case, pid: u32) {
    use std::os::{fd::FromRawFd, unix::fs::OpenOptionsExt};
    // SAFETY: open a new identity-pinning descriptor for the ready native worker.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    assert!(
        fd >= 0,
        "open worker pidfd: {}",
        std::io::Error::last_os_error()
    );
    running.worker = Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) });
    let raw = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
    assert!(raw.len() <= 4096);
    let relative = raw.strip_prefix("0::/").unwrap().trim_end_matches('\n');
    let path = Path::new("/sys/fs/cgroup")
        .join(relative)
        .canonicalize()
        .unwrap();
    let parent = Path::new(case.grants["cgroup_parent"].as_str().unwrap())
        .canonicalize()
        .unwrap();
    assert_eq!(path.parent(), Some(parent.as_path()));
    assert!(path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("forge-native-"));
    running.group_kill = Some(
        std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path.join("cgroup.kill"))
            .unwrap(),
    );
    running.group_path = Some(path);
}
fn worker_exited(fd: &std::os::fd::OwnedFd) -> bool {
    use std::os::fd::AsRawFd;
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll borrows one valid owned descriptor, without blocking.
    let ready = unsafe { libc::poll(&mut poll, 1, 0) };
    assert!(ready >= 0);
    ready == 1 && poll.revents & libc::POLLIN != 0
}

#[test]
#[ignore = "requires explicit delegated cgroup v2 parent in FORGE_TEST_CGROUP_PARENT"]
fn sigterm_cancels_worker_and_cleans_owned_resources_before_cli_exit() {
    use std::{
        process::Stdio,
        time::{Duration, Instant},
    };
    let mut case = Case::new(support::task_worker());
    case.delegated();
    case.staged_policy(|text| {
        text.replace("timeout_seconds=2", "timeout_seconds=10")
            .replace(
                "command=['worker','echo','literal argument','{files}']",
                "command=['worker','pid-sleep','20000','ready.pid']",
            )
    });
    let before = case.source_state();
    let stdout = case.fixture.root.join("cancel.stdout");
    let stderr = case.fixture.root.join("cancel.stderr");
    let child = case
        .command()
        .arg("--execute")
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&stdout).unwrap())
        .stderr(std::fs::File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    let mut running = InterruptedCli {
        child,
        worker: None,
        group_kill: None,
        group_path: None,
    };
    let deadline = Instant::now() + Duration::from_secs(8);
    let pid = loop {
        if let Some(pid) = ready_worker(&case) {
            break pid;
        }
        assert!(
            running.child.try_wait().unwrap().is_none(),
            "CLI exited before worker readiness: {}",
            std::fs::read_to_string(&stderr).unwrap()
        );
        assert!(
            Instant::now() < deadline,
            "worker readiness watchdog expired"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    pin_worker(&mut running, &case, pid);
    // SAFETY: child remains owned and unreaped, so this PID cannot be reused.
    assert_eq!(
        unsafe { libc::kill(running.child.id() as i32, libc::SIGTERM) },
        0
    );
    let status = loop {
        if let Some(status) = running.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "CLI cancellation watchdog expired"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(
        status.code(),
        Some(1),
        "{}",
        std::fs::read_to_string(&stderr).unwrap()
    );
    let report: Value = serde_json::from_slice(&std::fs::read(stdout).unwrap()).unwrap();
    assert_eq!(report["result"]["status"], "failed");
    assert_eq!(report["stages"][1]["report"]["outcome"], "cancelled");
    assert_eq!(report["stages"][1]["report"]["cleanup_complete"], true);
    assert!(worker_exited(running.worker.as_ref().unwrap()));
    assert!(!running.group_path.as_ref().unwrap().exists());
    case.assert_preserved(&before);
}

#[test]
#[ignore = "requires explicit delegated cgroup v2 parent and native /usr/bin/git"]
fn native_git_cached_diff_uses_private_candidate_index_and_detached_worktree() {
    let git = Path::new("/usr/bin/git");
    assert!(git.is_file());
    let mut case = Case::new(git);
    case.delegated();
    case.staged_policy(|text| {
        text.replace(
            "version_command=['worker','echo','fixture-v1']",
            "version_command=['worker','--version']",
        )
        .replace(
            "command=['worker','echo','literal argument','{files}']",
            "command=['worker','diff','--cached','--exit-code']",
        )
    });
    // The source index is dirty relative to HEAD; only the private index matches
    // the synthetic detached candidate commit, so this command must exit zero.
    let repository = Repository::open(case.fixture.root.join("repo")).unwrap();
    let source_tree = repository.head().unwrap().peel_to_tree().unwrap().id();
    let index_tree = repository.index().unwrap().write_tree().unwrap();
    assert_ne!(source_tree, index_tree);
    let before = case.source_state();
    let report = value(case.command().arg("--execute").output().unwrap(), 0);
    assert_eq!(report["result"]["status"], "passed");
    assert_eq!(report["result"]["exit_code"], 0);
    assert!(report["result"]["tool_version"]
        .as_str()
        .unwrap()
        .starts_with("git version "));
    case.assert_preserved(&before);
}

#[test]
#[ignore = "requires explicit delegated cgroup v2 parent in FORGE_TEST_CGROUP_PARENT"]
fn version_stage_obeys_cpu_and_address_space_grants() {
    for (probe, expected) in [
        (
            "version_command=['worker','map-probe','96']",
            "mapping denied",
        ),
        (
            "version_command=['worker','resource-limits']",
            "cpu=2 as=67108864",
        ),
    ] {
        let mut case = Case::new(support::task_worker());
        case.delegated();
        case.staged_policy(|text| {
            text.replace("memory_mb=128", "memory_mb=64")
                .replace("version_command=['worker','echo','fixture-v1']", probe)
        });
        let before = case.source_state();
        let mut command = case.command();
        command.arg("--execute");
        let output = forge::process_probe::run(
            &mut command,
            std::time::Duration::from_secs(15),
            1024 * 1024,
        )
        .unwrap();
        let report = value(output, 0);
        assert_eq!(report["result"]["status"], "passed");
        assert_eq!(report["result"]["tool_version"], expected);
        let stages = report["stages"].as_array().unwrap();
        assert_eq!(stages.len(), 2);
        for stage in stages {
            assert_eq!(stage["limits"]["address_space_bytes"], 64 * 1024 * 1024);
            assert_eq!(stage["limits"]["cpu_seconds"], 2);
            assert_eq!(stage["report"]["address_space_bytes"], 64 * 1024 * 1024);
            assert_eq!(stage["report"]["cpu_seconds"], 2);
            assert_eq!(stage["report"]["resources"]["verified"], true);
            assert_eq!(stage["report"]["cleanup_complete"], true);
        }
        case.assert_preserved(&before);
    }
}
