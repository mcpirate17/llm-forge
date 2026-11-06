#![cfg(target_os = "linux")]
//! Explicitly delegated real-guest controls; every private fixture is retained.
#[path = "support/candidate_policy_fixture.rs"]
#[allow(dead_code)]
mod candidate_policy_fixture;
#[allow(dead_code)]
mod support;

use candidate_policy_fixture::Fixture;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    mem::ManuallyDrop,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const IGNORE: &str = "requires owned delegated cgroup, FORGE_TEST_BIN/PYTHON/SLOP_EXTENSION/SLOP_SHA256/CGROUP_PARENT and CPU torch+pytest";
const SUBJECT: &str = "def clamp(x):\n    return x.clamp_min(0)\n";
const DRIVER: &str = "import torch\nfrom src.probe_target import clamp\n\ndef test_clamp():\n    assert clamp(torch.tensor([-2.0])).item() == 0.0\n";
const WEAK_DRIVER: &str = "import torch\nfrom src.probe_target import clamp\n\ndef test_clamp():\n    assert clamp(torch.tensor([2.0])).item() == 2.0\n";
const STALL_DRIVER: &str = r#"import json, os, time
from src.probe_target import clamp

def test_clamp():
    read_fd, write_fd = os.pipe()
    child = os.fork()
    if child == 0:
        os.close(read_fd)
        os.setsid()
        os.write(write_fd, b'R')
        time.sleep(90)
        os._exit(0)
    os.close(write_fd)
    assert os.read(read_fd, 1) == b'R'
    def identity(pid):
        stat = open('/proc/%d/stat' % pid).read()
        return {'pid': pid, 'start': stat[stat.rfind(')')+2:].split()[19],
                'cgroup': open('/proc/%d/cgroup' % pid).read().strip()}
    record = {'parent': identity(os.getpid()), 'child': identity(child)}
    path = os.path.join(os.environ['TMPDIR'], 'gate-ready-%d.json' % os.getpid())
    with open(path + '.new', 'x') as f: json.dump(record, f)
    os.rename(path + '.new', path)
    time.sleep(90)
"#;

fn env_path(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} required")));
    assert!(path.is_absolute());
    assert_eq!(path.canonicalize().unwrap(), path);
    path
}
fn interpreter_path() -> PathBuf {
    let path =
        PathBuf::from(std::env::var_os("FORGE_TEST_PYTHON").expect("FORGE_TEST_PYTHON required"));
    assert!(path.is_absolute(), "interpreter must be absolute");
    assert!(
        path.canonicalize().unwrap().is_file(),
        "interpreter target must be a file"
    );
    path
}
fn private_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
struct Harness {
    fixture: ManuallyDrop<Fixture>,
    repo: PathBuf,
    scratch: PathBuf,
    artifacts: PathBuf,
    grants: PathBuf,
    forge: PathBuf,
    guest: PathBuf,
    guest_hash: String,
    commit: String,
}
impl Harness {
    fn new(driver: &str, timeout: f64) -> Self {
        let fixture = ManuallyDrop::new(Fixture::new());
        let repo = fixture.root.join("repo");
        let scratch = fixture.root.join("scratch");
        let artifacts = fixture.root.join("artifacts");
        fs::create_dir(&artifacts).unwrap();
        fs::set_permissions(&artifacts, fs::Permissions::from_mode(0o700)).unwrap();
        let forge = env_path("FORGE_TEST_BIN");
        let python = interpreter_path();
        let python_target = python.canonicalize().unwrap();
        let guest = env_path("FORGE_TEST_SLOP_EXTENSION");
        let group = env_path("FORGE_TEST_CGROUP_PARENT");
        let guest_hash = std::env::var("FORGE_TEST_SLOP_SHA256").expect("guest SHA required");
        assert_eq!(
            format!("{:x}", Sha256::digest(fs::read(&guest).unwrap())),
            guest_hash
        );
        fixture.stage("src/probe_target.py", SUBJECT);
        fixture.stage("tests/test_probe_target.py", driver);
        fixture.stage("pyproject.toml", &format!("[tool.forge.equivalence_probe]\nrun_artifacts='{}'\nper_module_timeout={timeout}\njobs=1\n", artifacts.display()));
        fixture.stage(".gitignore", "ignored.py\n");
        let repository = git2::Repository::open(&repo).unwrap();
        let tree_id = repository.index().unwrap().write_tree().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
        let commit = repository
            .commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
            .unwrap()
            .to_string();
        let mut reads = vec![repo.clone()];
        for path in [
            python.parent().unwrap().parent().unwrap(),
            python_target.parent().unwrap(),
            guest.parent().unwrap(),
            forge.parent().unwrap(),
            Path::new("/usr"),
            Path::new("/usr/local"),
            Path::new("/etc"),
            Path::new("/proc"),
            Path::new("/sys"),
            Path::new("/dev"),
            Path::new("/lib"),
            Path::new("/lib64"),
        ] {
            if let Ok(path) = path.canonicalize() {
                if !reads.contains(&path) {
                    reads.push(path);
                }
            }
        }
        let grants = fixture.root.join("grants.json");
        let value = json!({"version":1,"source_root":repo,"read_only":reads,
            "programs":{"forge":forge,"python":python},"python":"python",
            "environment":{"PYTEST_DISABLE_PLUGIN_AUTOLOAD":"1","OMP_NUM_THREADS":"1","MKL_NUM_THREADS":"1","OPENBLAS_NUM_THREADS":"1","CUDA_VISIBLE_DEVICES":""},
            "scratch_parent":scratch,"cgroup_parent":group,
            "resources":{"cpu_millis":1000,"memory_mib":4096,"processes":128},
            "batch":{"max_workers":1,"cpu_millis":1000,"memory_mib":4096,"processes":128},
            "max_wall_seconds":150,"max_cpu_seconds":120,"max_address_space_mib":16384});
        private_file(&grants, &serde_json::to_vec(&value).unwrap());
        eprintln!(
            "retained equivalence gate fixture {}",
            fixture.root.display()
        );
        Self {
            fixture,
            repo,
            scratch,
            artifacts,
            grants,
            forge,
            guest,
            guest_hash,
            commit,
        }
    }
    fn command(&self, wall: u64) -> Command {
        let mut cmd = Command::new(&self.forge);
        cmd.args(["equivalence", "gate", "--root"])
            .arg(&self.repo)
            .args([
                "--commit",
                &self.commit,
                "--module",
                "src/probe_target.py",
                "--grants",
            ])
            .arg(&self.grants)
            .arg("--guest-path")
            .arg(&self.guest)
            .args(["--guest-sha256", &self.guest_hash])
            .arg("--artifacts")
            .arg(&self.artifacts)
            .args(["--json", "--enforce", "--wall-seconds", &wall.to_string()])
            .env("PATH", "")
            .env("HOME", &self.fixture.root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        cmd
    }
    fn live_command(&self, wall: u64) -> Command {
        let mut command = Command::new(&self.forge);
        command
            .args(["equivalence", "gate", "--root"])
            .arg(&self.repo)
            .args([
                "--live",
                "--input",
                "src",
                "--input",
                "tests",
                "--input",
                "data",
                "--module",
                "src/probe_target.py",
                "--grants",
            ])
            .arg(&self.grants)
            .arg("--guest-path")
            .arg(&self.guest)
            .args(["--guest-sha256", &self.guest_hash])
            .arg("--artifacts")
            .arg(&self.artifacts)
            .args(["--json", "--enforce", "--wall-seconds", &wall.to_string()])
            .env("PATH", "")
            .env("HOME", &self.fixture.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        command
    }
    fn assert_no_scratch(&self) {
        assert_eq!(fs::read_dir(&self.scratch).unwrap().count(), 0);
    }
}
struct OwnedChild {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}
impl OwnedChild {
    fn spawn(h: &Harness, wall: u64) -> Self {
        Self::spawn_command(h, h.command(wall))
    }
    fn spawn_live(h: &Harness, wall: u64) -> Self {
        Self::spawn_command(h, h.live_command(wall))
    }
    fn spawn_command(h: &Harness, mut cmd: Command) -> Self {
        let stdout = h.fixture.root.join("stdout.json");
        let stderr = h.fixture.root.join("stderr.log");
        cmd.stdout(Stdio::from(fs::File::create(&stdout).unwrap()))
            .stderr(Stdio::from(fs::File::create(&stderr).unwrap()));
        Self {
            child: cmd.spawn().unwrap(),
            stdout,
            stderr,
        }
    }
    fn signal(&mut self) {
        assert!(
            self.child.try_wait().unwrap().is_none(),
            "owned Forge already exited"
        );
        assert_eq!(
            unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) },
            0
        );
    }
    fn wait(&mut self, seconds: u64) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            let oversized = [&self.stdout, &self.stderr]
                .into_iter()
                .any(|path| fs::metadata(path).unwrap().len() > 8 * 1024 * 1024);
            if oversized {
                self.signal();
                panic!("Forge output limit exceeded; fixture retained");
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                self.signal();
                panic!("Forge wait unresolved; fixture retained");
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn diagnostic(&self) -> String {
        format!(
            "stdout={} stderr={}",
            fs::read_to_string(&self.stdout).unwrap_or_default(),
            fs::read_to_string(&self.stderr).unwrap_or_default()
        )
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            unsafe {
                libc::kill(self.child.id() as i32, libc::SIGTERM);
            }
            eprintln!(
                "owned Forge still active; fixture retained pid={}",
                self.child.id()
            );
        }
    }
}
fn marker_files(root: &Path, depth: usize, budget: &mut usize, out: &mut Vec<PathBuf>) {
    assert!(depth <= 8, "fixture scan depth exceeded");
    for entry in fs::read_dir(root).unwrap() {
        *budget += 1;
        assert!(*budget <= 4096, "fixture scan budget exceeded");
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        if ty.is_dir() {
            marker_files(&entry.path(), depth + 1, budget, out);
        } else if ty.is_file()
            && entry
                .file_name()
                .to_string_lossy()
                .starts_with("gate-ready-")
            && entry.path().extension().is_some_and(|v| v == "json")
        {
            out.push(entry.path());
        }
    }
}
fn read_marker(path: &Path) -> Option<Vec<u8>> {
    let stamp = |meta: &fs::Metadata| {
        (
            meta.dev(),
            meta.ino(),
            meta.mode(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec(),
        )
    };
    let before = fs::symlink_metadata(path).ok()?;
    if !before.is_file() || before.len() > 8192 {
        return None;
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if stamp(&before) != stamp(&file.metadata().ok()?) {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(8193).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 8192 || stamp(&before) != stamp(&fs::symlink_metadata(path).ok()?) {
        return None;
    }
    Some(bytes)
}
fn proc_identity(pid: u32) -> (String, String) {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let rest = stat.rsplit_once(')').unwrap().1.trim();
    let start = rest.split_whitespace().nth(19).unwrap().to_owned();
    let group = fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .unwrap()
        .trim()
        .to_owned();
    (start, group)
}
fn assert_gone_or_reused(pid: u32, expected: &str) {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => panic!("cannot verify observed pid {pid}: {error}"),
    };
    let fields = stat
        .rsplit_once(')')
        .expect("malformed process stat")
        .1
        .trim();
    let start = fields
        .split_whitespace()
        .nth(19)
        .expect("missing process start identity");
    assert_ne!(start, expected, "observed process {pid} still alive");
}
fn descends_from(mut pid: u32, owner: u32) -> bool {
    for _ in 0..64 {
        if pid == owner {
            return true;
        }
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        let Some((_, fields)) = stat.rsplit_once(')') else {
            return false;
        };
        let Some(parent) = fields
            .split_whitespace()
            .nth(1)
            .and_then(|p| p.parse::<u32>().ok())
        else {
            return false;
        };
        if parent <= 1 || parent == pid {
            return false;
        }
        pid = parent;
    }
    false
}
fn ready(h: &Harness, child: &mut OwnedChild) -> (Value, PathBuf) {
    let deadline = Instant::now() + Duration::from_secs(75);
    loop {
        assert!(
            child.child.try_wait().unwrap().is_none(),
            "Forge exited before witness: {}",
            child.diagnostic()
        );
        let mut paths = Vec::new();
        marker_files(&h.scratch, 0, &mut 0, &mut paths);
        for path in paths {
            let Some(bytes) = read_marker(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            let mut group = None;
            for name in ["parent", "child"] {
                let pid = value[name]["pid"].as_u64().unwrap() as u32;
                assert!(
                    descends_from(pid, child.child.id()),
                    "witness is not owned by Forge"
                );
                let (start, actual) = proc_identity(pid);
                assert_eq!(value[name]["start"], start);
                assert_eq!(value[name]["cgroup"], actual);
                if let Some(previous) = &group {
                    assert_eq!(previous, &actual);
                }
                group = Some(actual);
            }
            let child_pid = value["child"]["pid"].as_u64().unwrap() as i32;
            assert_eq!(
                unsafe { libc::getsid(child_pid) },
                child_pid,
                "child did not detach"
            );
            let group = group.unwrap();
            let relative = group.strip_prefix("0::/").expect("cgroup v2 required");
            let cgroup = Path::new("/sys/fs/cgroup").join(relative);
            let parent = env_path("FORGE_TEST_CGROUP_PARENT");
            assert!(cgroup.starts_with(&parent) && cgroup != parent && cgroup.is_dir());
            private_file(&h.fixture.root.join("observed-witness.json"), &bytes);
            return (value, cgroup);
        }
        assert!(
            Instant::now() < deadline,
            "readiness not observed; {}",
            child.diagnostic()
        );
        thread::sleep(Duration::from_millis(20));
    }
}
fn assert_removed(path: &Path) {
    assert!(
        matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound),
        "owned cgroup remains: {}",
        path.display()
    );
}
fn assert_clean_result(h: &Harness, value: &Value) {
    assert_eq!(value["capture_cleanup_complete"], true);
    assert_eq!(value["execution"]["cleanup_complete"], true);
    assert_eq!(value["execution"]["resources"]["verified"], true);
    let path = Path::new(value["execution"]["resources"]["path"].as_str().unwrap());
    let parent = env_path("FORGE_TEST_CGROUP_PARENT");
    assert!(
        path.starts_with(&parent) && path != parent,
        "unexpected execution cgroup: {}",
        path.display()
    );
    assert_removed(path);
    h.assert_no_scratch();
}

#[test]
#[ignore = "requires explicitly delegated external Python and native guest"]
fn committed_module_ignores_dirty_staged_untracked_and_ignored_host_inputs() {
    let _ = IGNORE;
    let h = Harness::new(WEAK_DRIVER, 90.0);
    h.fixture.stage(
        "src/probe_target.py",
        "raise RuntimeError('staged decoy')\n",
    );
    private_file(
        &h.repo.join("src/probe_target.py"),
        b"raise RuntimeError('dirty decoy')\n",
    );
    private_file(&h.repo.join("untracked.py"), b"untracked sentinel\n");
    private_file(&h.repo.join("ignored.py"), b"ignored sentinel\n");
    let index = fs::read(h.repo.join(".git/index")).unwrap();
    let mut owned = OwnedChild::spawn(&h, 120);
    let status = owned.wait(150);
    assert_eq!(status.code(), Some(1), "{}", owned.diagnostic());
    let value: Value = serde_json::from_slice(&fs::read(&owned.stdout).unwrap()).unwrap();
    assert_eq!(value["commit_oid"], h.commit);
    assert_eq!(value["working_tree_evaluated"], false);
    assert_eq!(value["scope"], "committed-whole-module");
    assert!(value["summary"]["blocking"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["module"] == "src/probe_target.py"
            && row["qualname"] == "clamp"
            && row["rule"] == "drop_clamp_min"
            && row["verdict"] == "REACHABLE_BUT_UNTESTED"));
    assert_clean_result(&h, &value);
    assert_eq!(fs::read(h.repo.join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::read(h.repo.join("src/probe_target.py")).unwrap(),
        b"raise RuntimeError('dirty decoy')\n"
    );
    assert_eq!(
        fs::read(h.repo.join("untracked.py")).unwrap(),
        b"untracked sentinel\n"
    );
    assert_eq!(
        fs::read(h.repo.join("ignored.py")).unwrap(),
        b"ignored sentinel\n"
    );
}

#[test]
#[ignore = "requires explicitly delegated external Python and native guest"]
fn nonblocking_committed_module_completes_without_enforcement_failure() {
    let h = Harness::new(DRIVER, 90.0);
    let mut owned = OwnedChild::spawn(&h, 120);
    let status = owned.wait(150);
    assert_eq!(status.code(), Some(0), "{}", owned.diagnostic());
    let value: Value = serde_json::from_slice(&fs::read(&owned.stdout).unwrap()).unwrap();
    assert_eq!(value["summary"]["modules_probed"], 1);
    assert!(value["summary"]["blocking"].as_array().unwrap().is_empty());
    assert!(value["summary"]["incomplete"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(value["summary"]["live"].as_u64().unwrap() > 0);
    assert_clean_result(&h, &value);
}

#[test]
#[ignore = "requires explicitly delegated external Python and native guest"]
fn inner_module_timeout_is_incomplete_and_owned_cleanup_completes() {
    let h = Harness::new(DRIVER, 0.001);
    let mut owned = OwnedChild::spawn(&h, 120);
    let status = owned.wait(150);
    assert_eq!(status.code(), Some(0), "{}", owned.diagnostic());
    let value: Value = serde_json::from_slice(&fs::read(&owned.stdout).unwrap()).unwrap();
    assert!(value["summary"]["incomplete"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["verdict"] == "TIMEOUT"));
    assert_clean_result(&h, &value);
}

fn interrupted_control(signal: bool) {
    let h = Harness::new(STALL_DRIVER, 120.0);
    let mut owned = OwnedChild::spawn(&h, if signal { 120 } else { 90 });
    let (witness, group) = ready(&h, &mut owned);
    if signal {
        owned.signal();
    }
    let status = owned.wait(145);
    assert!(
        !status.success(),
        "expected refused execution: {}",
        owned.diagnostic()
    );
    let diagnostic = fs::read_to_string(&owned.stderr).unwrap();
    if signal {
        assert!(
            diagnostic.contains("equivalence interrupted after cleanup"),
            "{diagnostic}"
        );
    } else {
        assert!(
            diagnostic
                .contains("equivalence Stage did not complete with verified cleanup/resources"),
            "{diagnostic}"
        );
    }
    assert_removed(&group);
    h.assert_no_scratch();
    assert_eq!(fs::read_dir(&h.artifacts).unwrap().count(), 0);
    for name in ["parent", "child"] {
        let pid = witness[name]["pid"].as_u64().unwrap();
        assert_gone_or_reused(pid as u32, witness[name]["start"].as_str().unwrap());
    }
}
#[test]
#[ignore = "requires fresh owned delegated unit with 900-second backstop"]
fn stage_wall_timeout_kills_observed_detached_descendant() {
    interrupted_control(false);
}
#[test]
#[ignore = "requires fresh owned delegated unit with 900-second backstop"]
fn sigterm_kills_observed_detached_descendant() {
    interrupted_control(true);
}

const LIVE_DATA_CHECKS: &str = r#"    assert Path('data/threshold').read_bytes() == b'-2.0\n'
    assert Path('data/opaque').read_bytes() == b'\x00\xffignored data'
    assert not Path('tests/test_probe_target.py').exists()
"#;
const LIVE_DRIVER: &str = r#"from pathlib import Path
import torch
from src.probe_target import clamp

def test_live_clamp():
__DATA_CHECKS__    value = float(Path('data/threshold').read_text())
    assert clamp(torch.tensor([value])).item() == 0.0
"#;
const LIVE_READY_DRIVER: &str = r#"from pathlib import Path
import json, os, time, torch
from src.probe_target import clamp

def test_live_clamp():
__DATA_CHECKS__    stat = Path('/proc/self/stat').read_text()
    witness = {'pid': os.getpid(), 'start': stat[stat.rfind(')')+2:].split()[19],
               'cgroup': Path('/proc/self/cgroup').read_text().strip(),
               'snapshot_root': str(Path(__file__).resolve().parents[1])}
    marker = Path(os.environ['TMPDIR']) / ('gate-ready-live-%d.json' % os.getpid())
    temporary = marker.with_suffix('.new')
    temporary.write_text(json.dumps(witness))
    temporary.rename(marker)
    release = Path(__RELEASE_PATH__)
    deadline = time.monotonic() + 30
    while release.read_text() == 'pending':
        assert time.monotonic() < deadline, 'host release not observed'
        time.sleep(0.02)
    value = float(release.read_text())
    assert value == -3.0
    assert clamp(torch.tensor([value])).item() == 0.0
"#;

fn live_harness(driver: &str, timeout: f64) -> Harness {
    let deleted_driver = "from src.probe_target import clamp\nraise RuntimeError('deleted tracked driver resurrected')\n";
    let mut h = Harness::new(deleted_driver, timeout);
    h.fixture.stage(
        "src/probe_target.py",
        "raise RuntimeError('committed decoy')\n",
    );
    let repository = git2::Repository::open(&h.repo).unwrap();
    let parent = repository.head().unwrap().peel_to_commit().unwrap();
    let tree = repository
        .find_tree(repository.index().unwrap().write_tree().unwrap())
        .unwrap();
    let signature = git2::Signature::now("fixture", "fixture@example.invalid").unwrap();
    h.commit = repository
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "committed live decoy",
            &tree,
            &[&parent],
        )
        .unwrap()
        .to_string();
    h.fixture.stage(
        "src/probe_target.py",
        "raise RuntimeError('staged decoy')\n",
    );
    private_file(&h.repo.join("src/probe_target.py"), SUBJECT.as_bytes());
    fs::remove_file(h.repo.join("tests/test_probe_target.py")).unwrap();
    h.fixture.stage(
        ".gitignore",
        "ignored.py\ndata/\noutside/\ntests/test_live_ignored.py\n",
    );
    for name in ["data", "outside"] {
        let path = h.repo.join(name);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    private_file(&h.repo.join("data/threshold"), b"-2.0\n");
    private_file(&h.repo.join("data/opaque"), b"\0\xffignored data");
    private_file(&h.repo.join("outside/release"), b"pending");
    let driver = driver.replace("__DATA_CHECKS__", LIVE_DATA_CHECKS).replace(
        "__RELEASE_PATH__",
        &serde_json::to_string(&h.repo.join("outside/release")).unwrap(),
    );
    private_file(
        &h.repo.join("tests/test_live_ignored.py"),
        driver.as_bytes(),
    );
    for name in [
        "data/threshold",
        "data/opaque",
        "tests/test_live_ignored.py",
        "outside/release",
    ] {
        assert!(repository
            .status_file(Path::new(name))
            .unwrap()
            .contains(git2::Status::IGNORED));
    }
    h
}

fn release_live(h: &Harness) {
    let next = h.repo.join("outside/release.next");
    private_file(&next, b"-3.0\n");
    fs::rename(next, h.repo.join("outside/release")).unwrap();
}
fn live_ready(h: &Harness, child: &mut OwnedChild) -> (Value, PathBuf, Vec<PathBuf>) {
    let deadline = Instant::now() + Duration::from_secs(75);
    loop {
        assert!(
            child.child.try_wait().unwrap().is_none(),
            "Forge exited before witness: {}",
            child.diagnostic()
        );
        let mut paths = Vec::new();
        marker_files(&h.scratch, 0, &mut 0, &mut paths);
        for path in paths {
            if !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("gate-ready-live-")
            {
                continue;
            }
            let Some(bytes) = read_marker(&path) else {
                continue;
            };
            let witness: Value = serde_json::from_slice(&bytes).unwrap();
            let pid = witness["pid"].as_u64().unwrap() as u32;
            assert!(descends_from(pid, child.child.id()));
            let (start, group) = proc_identity(pid);
            assert_eq!(witness["start"], start);
            assert_eq!(witness["cgroup"], group);
            let group = Path::new("/sys/fs/cgroup").join(group.strip_prefix("0::/").unwrap());
            let parent = env_path("FORGE_TEST_CGROUP_PARENT");
            assert!(group.starts_with(&parent) && group != parent && group.is_dir());
            let stage_tree = Path::new(witness["snapshot_root"].as_str().unwrap());
            assert!(stage_tree.starts_with(&h.scratch) && stage_tree.is_dir());
            assert!(!stage_tree.join("outside/release").exists());
            assert_eq!(
                fs::read(stage_tree.join("data/threshold")).unwrap(),
                b"-2.0\n"
            );
            assert!(!stage_tree.join("tests/test_probe_target.py").exists());
            let roots: Vec<_> = fs::read_dir(&h.scratch)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            assert!(
                roots.len() >= 3,
                "live capture, candidate capture and Stage owners not observed: {roots:?}"
            );
            private_file(&h.fixture.root.join("observed-live-witness.json"), &bytes);
            return (witness, group, roots);
        }
        assert!(
            Instant::now() < deadline,
            "live readiness watchdog expired: {}",
            child.diagnostic()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn assert_live_result(h: &Harness, value: &Value, index_hash: &str) {
    assert_eq!(value["scope"], "live-whole-module-declared-inputs");
    assert_eq!(value["working_tree_evaluated"], true);
    assert!(value["commit_oid"].is_null());
    assert_eq!(value["inputs"]["source_kind"], "live_snapshot_tree");
    assert_eq!(value["live_inputs"]["profile"], "explicit-live-inputs-v1");
    assert_eq!(
        value["live_inputs"]["roots"],
        json!({"data":true,"src":true,"tests":true})
    );
    assert_eq!(value["live_inputs"]["host_head"], h.commit);
    assert_eq!(value["live_inputs"]["host_index_sha256"], index_hash);
    assert_eq!(value["live_inputs"]["snapshot_tree"], value["tree_oid"]);
    assert_eq!(
        value["live_inputs"]["inputs"]["digest"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(value["live_capture_cleanup_complete"], true);
    assert_eq!(value["execution_read_scope"]["policy"], "operator_grants");
    assert_eq!(
        value["execution_read_scope"]["original_source_readable"],
        true
    );
    assert_eq!(
        value["execution_read_scope"]["captured_inputs_are_dependency_closure"],
        false
    );
    assert_eq!(value["guest_output_authenticated"], false);
    assert_clean_result(h, value);
}

#[test]
#[ignore = "requires explicit real guest/interpreter pins and fresh delegated unit; sequential live control"]
fn live_gate_reads_ignored_extensionless_data_and_live_bytes_over_staged_and_committed_decoys() {
    let h = live_harness(LIVE_DRIVER, 90.0);
    let index = fs::read(h.repo.join(".git/index")).unwrap();
    let index_hash = format!("{:x}", Sha256::digest(&index));
    let mut child = OwnedChild::spawn_live(&h, 120);
    assert_eq!(child.wait(150).code(), Some(0), "{}", child.diagnostic());
    let value: Value = serde_json::from_slice(&fs::read(&child.stdout).unwrap()).unwrap();
    assert_live_result(&h, &value, &index_hash);
    assert_eq!(value["summary"]["modules_probed"], 1);
    assert!(value["summary"]["live"].as_u64().unwrap() > 0);
    assert!(value["summary"]["blocking"].as_array().unwrap().is_empty());
    assert!(value["summary"]["incomplete"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(fs::read(h.repo.join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::read(h.repo.join("src/probe_target.py")).unwrap(),
        SUBJECT.as_bytes()
    );
    assert_eq!(fs::read(h.repo.join("data/threshold")).unwrap(), b"-2.0\n");
    assert!(!h.repo.join("tests/test_probe_target.py").exists());
}

#[test]
#[ignore = "requires explicit real guest/interpreter pins and fresh delegated unit; sequential live control"]
fn live_gate_refuses_post_readiness_selected_data_drift_before_publication() {
    let h = live_harness(LIVE_READY_DRIVER, 90.0);
    let mut child = OwnedChild::spawn_live(&h, 120);
    let (witness, group, roots) = live_ready(&h, &mut child);
    private_file(&h.repo.join("data/threshold"), b"-3.0\n");
    release_live(&h);
    assert!(!child.wait(150).success(), "{}", child.diagnostic());
    let error = fs::read_to_string(&child.stderr).unwrap();
    assert!(
        error.contains("live host input selection changed"),
        "{error}"
    );
    assert_eq!(fs::read_dir(&h.artifacts).unwrap().count(), 0);
    assert_removed(&group);
    for root in roots {
        assert_removed(&root);
    }
    h.assert_no_scratch();
    assert_gone_or_reused(
        witness["pid"].as_u64().unwrap() as u32,
        witness["start"].as_str().unwrap(),
    );
    assert_eq!(fs::read(h.repo.join("data/threshold")).unwrap(), b"-3.0\n");
}

#[test]
#[ignore = "requires explicit real guest/interpreter pins and fresh delegated unit; sequential live boundary control"]
fn live_gate_exposes_authorized_absolute_unselected_host_reads_outside_captured_identity() {
    let h = live_harness(LIVE_READY_DRIVER, 90.0);
    let index = fs::read(h.repo.join(".git/index")).unwrap();
    let index_hash = format!("{:x}", Sha256::digest(&index));
    let mut child = OwnedChild::spawn_live(&h, 120);
    let (witness, group, roots) = live_ready(&h, &mut child);
    // Original source read access already exists in the operator grants. No grant is widened.
    release_live(&h);
    assert_eq!(child.wait(150).code(), Some(0), "{}", child.diagnostic());
    let value: Value = serde_json::from_slice(&fs::read(&child.stdout).unwrap()).unwrap();
    assert_live_result(&h, &value, &index_hash);
    assert!(value["summary"]["live"].as_u64().unwrap() > 0);
    assert!(value["summary"]["incomplete"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(fs::read(h.repo.join(".git/index")).unwrap(), index);
    assert_removed(&group);
    for root in roots {
        assert_removed(&root);
    }
    assert_gone_or_reused(
        witness["pid"].as_u64().unwrap() as u32,
        witness["start"].as_str().unwrap(),
    );
    assert_eq!(fs::read(h.repo.join("outside/release")).unwrap(), b"-3.0\n");
}

fn live_interrupted_control(signal: bool) {
    let driver = STALL_DRIVER
        .replace(
            "import json, os, time",
            "from pathlib import Path\nimport json, os, time",
        )
        .replace(
            "    read_fd, write_fd = os.pipe()",
            &(LIVE_DATA_CHECKS.to_owned() + "    read_fd, write_fd = os.pipe()"),
        );
    let h = live_harness(&driver, 120.0);
    let mut child = OwnedChild::spawn_live(&h, if signal { 120 } else { 90 });
    let (witness, group) = ready(&h, &mut child);
    let roots: Vec<_> = fs::read_dir(&h.scratch)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert!(roots.len() >= 3);
    if signal {
        child.signal();
    }
    assert!(!child.wait(145).success(), "{}", child.diagnostic());
    let error = fs::read_to_string(&child.stderr).unwrap();
    assert!(
        error.contains(if signal {
            "equivalence interrupted after cleanup"
        } else {
            "equivalence Stage did not complete with verified cleanup/resources"
        }),
        "{error}"
    );
    assert_removed(&group);
    for root in roots {
        assert_removed(&root);
    }
    h.assert_no_scratch();
    assert_eq!(fs::read_dir(&h.artifacts).unwrap().count(), 0);
    for name in ["parent", "child"] {
        assert_gone_or_reused(
            witness[name]["pid"].as_u64().unwrap() as u32,
            witness[name]["start"].as_str().unwrap(),
        );
    }
}

#[test]
#[ignore = "requires fresh owned delegated unit with 900-second backstop; sequential live control"]
fn live_stage_wall_timeout_cleans_capture_candidate_stage_and_observed_detached_descendant() {
    live_interrupted_control(false);
}
#[test]
#[ignore = "requires fresh owned delegated unit with 900-second backstop; sequential live control"]
fn live_sigterm_cleans_capture_candidate_stage_and_observed_detached_descendant() {
    live_interrupted_control(true);
}
