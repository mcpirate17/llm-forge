#![cfg(target_os = "linux")]
#[path = "support/forge_binary.rs"]
mod forge_binary;
#[path = "support/graph_test_select_real.rs"]
mod real;
use git2::{Repository, Signature};
use rusqlite::Connection;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::OnceLock,
    time::{Duration, Instant},
};

fn fixture_guest() -> &'static Path {
    static GUEST: OnceLock<PathBuf> = OnceLock::new();
    GUEST.get_or_init(|| {
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/graph_test_guest.rs");
        let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("graph-test-guest-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let binary = directory.join("guest");
        let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let result = Command::new(compiler)
            .args(["--edition=2021", "-C", "debuginfo=0"])
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        binary
    })
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn commit(root: &Path) {
    let repository = Repository::init(root).unwrap();
    let mut index = repository.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repository.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = Signature::now("Graph fixture", "graph@example.invalid").unwrap();
    repository
        .commit(Some("HEAD"), &signature, &signature, "fixture", &tree, &[])
        .unwrap();
}

fn graph(root: &Path) {
    fs::create_dir(root.join(".code-review-graph")).unwrap();
    Connection::open(root.join(".code-review-graph/graph.db"))
        .unwrap()
        .execute_batch(
            "CREATE TABLE nodes(qualified_name TEXT, file_path TEXT, is_test INTEGER);
        CREATE TABLE edges(source_qualified TEXT, target_qualified TEXT);",
        )
        .unwrap();
}

struct Fixture {
    _owned: tempfile::TempDir,
    repo: PathBuf,
    runtime: PathBuf,
    observations: PathBuf,
    tools: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let owned = tempfile::tempdir().unwrap();
        let base = owned.path().canonicalize().unwrap();
        let (repo, runtime, observations, tools) = (
            base.join("repo"),
            base.join("runtime"),
            base.join("observations"),
            base.join("tools"),
        );
        for directory in [&repo, &runtime, &observations, &tools] {
            fs::create_dir(directory).unwrap();
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        write(
            &repo,
            "Cargo.toml",
            "[package]\nname='supervisor-fixture'\nversion='0.1.0'\n",
        );
        write(&repo, "src/lib.rs", "#[test] fn marker() {}\n");
        commit(&repo);
        graph(&repo);
        fs::write(runtime.join("caller-owned"), "preserve").unwrap();
        for name in ["cargo", "rustc", "rustdoc"] {
            fs::copy(fixture_guest(), tools.join(name)).unwrap();
        }
        Self {
            _owned: owned,
            repo,
            runtime,
            observations,
            tools,
        }
    }
    fn command(&self, mode: &str, timeout: u64) -> Command {
        let mut command = Command::new(forge_binary::path());
        command
            .args(["graph-test-select", "--repo"])
            .arg(&self.repo)
            .args(["--run", "--runtime-dir"])
            .arg(&self.runtime)
            .args(["--timeout-seconds", &timeout.to_string(), "src/lib.rs"])
            .env("FORGE_CARGO_BIN", &self.tools)
            .env_remove("FORGE_CARGO_HOME_SEED")
            .env("FORGE_GRAPH_FIXTURE_MODE", mode)
            .env("FORGE_GRAPH_FIXTURE_OBSERVATIONS", &self.observations);
        command
    }
    fn run(&self, mode: &str, timeout: u64) -> Output {
        forge::process_probe::run(
            &mut self.command(mode, timeout),
            Duration::from_secs(8),
            1024 * 1024,
        )
        .unwrap()
    }
    fn scratch(&self) -> Vec<PathBuf> {
        assert_eq!(
            fs::read_to_string(self.runtime.join("caller-owned")).unwrap(),
            "preserve"
        );
        fs::read_dir(&self.runtime)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.file_name().unwrap() != "caller-owned")
            .collect()
    }
    fn assert_guests_dead(&self) {
        for name in ["leader.pid", "descendant.pid"] {
            let pid = fs::read_to_string(self.observations.join(name)).unwrap();
            let status = fs::read_to_string(format!("/proc/{pid}/stat"));
            assert!(
                status.as_ref().is_err_and(|error| {
                    error.kind() == std::io::ErrorKind::NotFound
                        || error.raw_os_error() == Some(libc::ESRCH)
                }) || status.as_ref().is_ok_and(|text| text
                    .rsplit_once(") ")
                    .unwrap()
                    .1
                    .starts_with('Z')),
                "guest still running: {pid} {status:?}"
            );
        }
    }
}

#[test]
fn actual_cli_success_cleans_only_owned_scratch() {
    let fixture = Fixture::new();
    let output = fixture.run("success", 3);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("native supervisor fixture success"));
    assert!(fixture.scratch().is_empty());
    assert!(!fixture.repo.join("target").exists());
}

#[test]
fn actual_cli_retains_failure_scratch_and_propagates_exit_and_signal() {
    for (mode, expected) in [("exit", 37), ("signal", 143)] {
        let fixture = Fixture::new();
        let output = fixture.run(mode, 3);
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let scratch = fixture.scratch();
        assert_eq!(scratch.len(), 1);
        assert!(String::from_utf8_lossy(&output.stderr).contains(scratch[0].to_str().unwrap()));
    }
}

#[test]
fn actual_cli_deadline_stops_descendants_and_retains_failure_scratch() {
    let fixture = Fixture::new();
    let began = Instant::now();
    let output = fixture.run("sleep", 1);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("timed out"));
    assert!(began.elapsed() < Duration::from_secs(5));
    assert_eq!(fixture.scratch().len(), 1);
    fixture.assert_guests_dead();
}

fn cancel_on_ready(fixture: &Fixture) -> std::thread::JoinHandle<()> {
    let ready = fixture.observations.join("ready");
    let pid_file = fixture.observations.join("supervisor.pid");
    let binary = PathBuf::from(forge_binary::path()).canonicalize().unwrap();
    std::thread::spawn(move || {
        let began = Instant::now();
        while !ready.exists() {
            assert!(
                began.elapsed() < Duration::from_secs(3),
                "guest did not become ready"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let pid: i32 = fs::read_to_string(pid_file).unwrap().parse().unwrap();
        assert_eq!(fs::read_link(format!("/proc/{pid}/exe")).unwrap(), binary);
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    })
}

#[test]
fn actual_cli_cancellation_stops_descendants_and_preserves_signal_status() {
    let fixture = Fixture::new();
    let sender = cancel_on_ready(&fixture);
    let output = fixture.run("sleep", 5);
    sender.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(143),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fixture.scratch().len(), 1);
    fixture.assert_guests_dead();
}

#[test]
fn actual_cli_preserves_compatibility_build_failure_status() {
    for (mode, expected) in [("exit", 37), ("signal", 143)] {
        let fixture = Fixture::new();
        write(
            &fixture.repo,
            "native/conductor-native/Cargo.toml",
            "[package]\nname='guest'\nversion='0.1.0'\n",
        );
        write(
            &fixture.repo,
            "native/forge/Cargo.toml",
            "[package]\nname='forge'\nversion='0.1.0'\n",
        );
        write(
            &fixture.repo,
            "native/conductor-native/src/python_contract_targets.tsv",
            "src/conductor/probe.py\tpython_contracts_guest\n",
        );
        write(
            &fixture.repo,
            "native/conductor-native/tests/python_contracts_guest.rs",
            "#[test] fn guest() {}\n",
        );
        fs::copy(fixture_guest(), fixture.tools.join("python")).unwrap();
        let mut command = fixture.command(mode, 3);
        command.arg("--python").arg(fixture.tools.join("python"));
        let output =
            forge::process_probe::run(&mut command, Duration::from_secs(8), 1024 * 1024).unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("candidate contract build"));
        assert_eq!(fixture.scratch().len(), 1);
    }
}

#[test]
fn actual_cli_pytest_guest_gets_explicit_argv_and_propagates_status() {
    for (mode, expected) in [("success", 0), ("exit", 37), ("signal", 143)] {
        let fixture = Fixture::new();
        write(&fixture.repo, "src/lib.rs", "pub fn library() {}\n");
        write(
            &fixture.repo,
            "tests/test_guest.py",
            "def test_guest(): pass\n",
        );
        fs::copy(fixture_guest(), fixture.tools.join("python")).unwrap();
        let mut command = fixture.command(mode, 3);
        command.arg("--python").arg(fixture.tools.join("python"));
        let output =
            forge::process_probe::run(&mut command, Duration::from_secs(8), 1024 * 1024).unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let argv = fs::read_to_string(fixture.observations.join("pytest.argv")).unwrap();
        assert_eq!(
            argv.split('\0').collect::<Vec<_>>(),
            [
                "-m",
                "pytest",
                "-o",
                "addopts=",
                "-q",
                "--tb=short",
                "--",
                "tests/test_guest.py"
            ]
        );
        assert!(fixture.scratch().is_empty());
    }
}

#[test]
fn combined_guests_share_live_fresh_runtime_and_failure_retains_it() {
    for (mode, expected) in [("combined-success", 0), ("combined-pytest-fail", 41)] {
        let fixture = Fixture::new();
        write(
            &fixture.repo,
            "native/conductor-native/Cargo.toml",
            "[package]\nname='guest'\nversion='0.1.0'\n",
        );
        write(
            &fixture.repo,
            "native/forge/Cargo.toml",
            "[package]\nname='forge'\nversion='0.1.0'\n",
        );
        write(
            &fixture.repo,
            "native/conductor-native/src/python_contract_targets.tsv",
            "src/conductor/probe.py\tpython_contracts_guest\n",
        );
        write(
            &fixture.repo,
            "native/conductor-native/tests/python_contracts_guest.rs",
            "#[test] fn guest() {}\n",
        );
        write(
            &fixture.repo,
            "tests/test_guest.py",
            "def test_guest(): pass\n",
        );
        fs::copy(fixture_guest(), fixture.tools.join("python")).unwrap();
        let mut command = fixture.command(mode, 3);
        command.arg("--python").arg(fixture.tools.join("python"));
        let output =
            forge::process_probe::run(&mut command, Duration::from_secs(8), 1024 * 1024).unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let contract = fs::read_to_string(fixture.observations.join("contract.runtime")).unwrap();
        let pytest = fs::read_to_string(fixture.observations.join("pytest.runtime")).unwrap();
        assert_eq!(contract, pytest);
        for path in pytest.lines() {
            assert_eq!(Path::new(path).exists(), expected != 0);
        }
        assert_eq!(fixture.scratch().len(), usize::from(expected != 0));
    }
}

#[test]
fn cancellation_during_python_admission_preserves_signal_and_stops_guest() {
    let fixture = Fixture::new();
    write(&fixture.repo, "src/lib.rs", "pub fn library() {}\n");
    write(
        &fixture.repo,
        "tests/test_guest.py",
        "def test_guest(): pass\n",
    );
    fs::copy(fixture_guest(), fixture.tools.join("python")).unwrap();
    let sender = cancel_on_ready(&fixture);
    let mut command = fixture.command("probe-sleep", 5);
    command.arg("--python").arg(fixture.tools.join("python"));
    let output =
        forge::process_probe::run(&mut command, Duration::from_secs(8), 1024 * 1024).unwrap();
    sender.join().unwrap();
    assert_eq!(
        output.status.code(),
        Some(143),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.assert_guests_dead();
    assert!(fixture.scratch().is_empty());
    assert!(!fixture.observations.join("pytest.argv").exists());
}
