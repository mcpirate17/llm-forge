//! Owned opt-in service fixture; evidence stays on disk unless cleanup is verified.
use crate::mcp_client::Mcp;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

pub fn sha(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}
pub fn missing(path: &Path) {
    assert!(
        matches!(fs::symlink_metadata(path), Err(error)
        if error.kind() == std::io::ErrorKind::NotFound),
        "not positively absent: {}",
        path.display()
    );
}
fn private_dir(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn write(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn copied(root: &Path, name: &str, env: &str) -> (PathBuf, Value) {
    let source = PathBuf::from(std::env::var_os(env).unwrap_or_else(|| panic!("set {env}")));
    let expected =
        std::env::var(format!("{env}_SHA256")).expect("supply independent executable SHA256");
    assert!(source.is_absolute() && fs::symlink_metadata(&source).unwrap().is_file());
    assert_eq!(sha(&source), expected);
    let output = root.join(name);
    fs::copy(&source, &output).unwrap();
    fs::set_permissions(&output, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(sha(&source), expected);
    assert_eq!(sha(&output), expected);
    (
        output.clone(),
        json!({"original":source,"copy":output,"sha256":expected}),
    )
}
fn private_tree(root: &Path) {
    let mut pending = vec![root.to_owned()];
    let mut count = 0;
    while let Some(directory) = pending.pop() {
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            count += 1;
            assert!(count <= 256);
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                pending.push(entry.path());
            } else {
                assert!(kind.is_file());
                fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
    }
}
fn fixture(root: &Path) {
    let repo = git2::Repository::init(root).unwrap();
    fs::set_permissions(root.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
    private_dir(&root.join("pkg"));
    let mut index = repo.index().unwrap();
    for (name, text) in [
        ("pkg/__init__.py", ""),
        ("test_broken.py", "import pkg.missing\n"),
    ] {
        write(&root.join(name), text.as_bytes());
        index.add_path(Path::new(name)).unwrap();
    }
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature =
        git2::Signature::new("fixture", "fixture@example.invalid", &git2::Time::new(0, 0)).unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "authored fixture",
        &tree,
        &[],
    )
    .unwrap();
}

pub struct Running {
    pub source: PathBuf,
    pub state: PathBuf,
    pub binary: PathBuf,
    pub denied: PathBuf,
    root: Option<tempfile::TempDir>,
    service: Option<forge::managed_service::Service>,
    verified: bool,
    pins: Vec<(PathBuf, String)>,
    sequence: std::cell::Cell<u64>,
    source_pins: BTreeMap<PathBuf, String>,
}
impl Drop for Running {
    fn drop(&mut self) {
        if !self.verified || std::thread::panicking() {
            if let Some(root) = self.root.take() {
                eprintln!("test report fixture retained: {}", root.keep().display());
            }
        }
    }
}
impl Running {
    pub async fn start(synthetic: bool, enabled: bool, seconds: u64) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let mut running = Self {
            source: root.path().join("source"),
            state: root.path().join("state"),
            binary: root.path().join("bin/forge"),
            denied: root.path().join("denied.txt"),
            root: Some(root),
            service: None,
            verified: false,
            pins: Vec::new(),
            sequence: std::cell::Cell::new(0),
            source_pins: BTreeMap::new(),
        };
        let parent = running.root.as_ref().unwrap().path().to_owned();
        for path in [&running.source, &running.state, &parent.join("bin")] {
            private_dir(path);
        }
        fixture(&running.source);
        private_tree(&running.source);
        for name in [
            "pkg/__init__.py",
            "test_broken.py",
            ".git/HEAD",
            ".git/index",
            ".git/config",
        ] {
            let path = running.source.join(name);
            running.source_pins.insert(path.clone(), sha(&path));
        }
        write(&running.denied, b"synthetic ungranted bytes\n");
        let (binary, forge_pin) = copied(&parent.join("bin"), "forge", "FORGE_TEST_BIN");
        running.binary = binary;
        let (worker, worker_pin) = if synthetic {
            copied(&parent.join("bin"), "worker", "FORGE_TEST_REPORT_WORKER")
        } else {
            (running.binary.clone(), forge_pin.clone())
        };
        let mut reads: Vec<PathBuf> = serde_json::from_str(
            &std::env::var("FORGE_TEST_REPORT_RUNTIME_READS")
                .expect("supply explicit canonical runtime read grants as JSON array"),
        )
        .unwrap();
        for path in &reads {
            assert!(path.is_absolute());
            assert_eq!(path.canonicalize().unwrap(), *path);
        }
        reads.push(worker.clone());
        reads.sort();
        reads.dedup();
        let mut document = json!({"version":1,"source_root":running.source,"write_paths":[],"read_only":reads,
            "programs":{},"aggregate":{"cpu_millis":1000,"memory_mib":1024,"processes":128},
            "session":{"cpu_millis":500,"memory_mib":128,"processes":32},
            "max_sessions":1,"max_retained_sessions":2,"max_tasks":4,"lifetime_seconds":60,
            "session_seconds":20,"retention_seconds":5,"replay_bytes":65536,"log_bytes":65536});
        if enabled {
            document["test_report"] = json!({"executable":{"path":worker,"sha256":sha(&worker)},
            "read_only":reads,"resources":{"cpu_millis":500,"memory_mib":128,"processes":16},
            "max_wall_seconds":seconds,"max_output_bytes":262144});
        }
        let policy: forge::harness::policy::Policy =
            serde_json::from_value(document.clone()).unwrap();
        let policy = policy.validate(&running.state).unwrap();
        write(
            &running.state.join("policy.json"),
            &serde_json::to_vec(&document).unwrap(),
        );
        for pin in [&forge_pin, &worker_pin] {
            running.pins.push((
                PathBuf::from(pin["copy"].as_str().unwrap()),
                pin["sha256"].as_str().unwrap().to_owned(),
            ));
        }
        running.save("provenance", json!({"scope":if synthetic {"synthetic worker lifecycle controls"} else {"real Forge classifier"},
            "forge":forge_pin,"worker":worker_pin,"runtime_read_grants":reads,"policy_sha256":sha(&running.state.join("policy.json")),
            "source_inputs":running.source_pins,"synthetic_fixture_source_sha256":format!("{:x}",Sha256::digest(include_bytes!("test_report_worker_fixture.rs")))}));
        running.launch(&policy);
        running.wait_ready().await;
        running
    }
    fn launch(&mut self, policy: &forge::harness::policy::Policy) {
        self.service = Some(
            forge::managed_service::Service::launch(
                &self.binary,
                &[
                    "harness".into(),
                    "serve".into(),
                    "--state".into(),
                    self.state.as_os_str().into(),
                ],
                policy.aggregate,
                policy.lifetime_seconds,
                &BTreeMap::from([(
                    "FORGE_TEST_REPORT_SYNTHETIC_SECRET".into(),
                    "never-forward".into(),
                )]),
            )
            .unwrap(),
        );
        self.save(
            "service-record",
            serde_json::to_value(self.service.as_ref().unwrap().record()).unwrap(),
        );
    }
    async fn wait_ready(&self) {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Ok(bytes) = fs::read(self.state.join("failure.json")) {
                    panic!("service refused: {}", String::from_utf8_lossy(&bytes));
                }
                if self.state.join("ready.json").is_file() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    pub fn save(&self, label: &str, value: Value) {
        let sequence = self.sequence.get();
        self.sequence.set(sequence + 1);
        let path = self.state.join(format!("{sequence:04}-{label}.json"));
        assert!(
            !path.exists(),
            "evidence must not overwrite a previous observation"
        );
        write(&path, &serde_json::to_vec_pretty(&value).unwrap());
    }
    pub async fn status(&self) -> Value {
        let reply = tokio::time::timeout(
            Duration::from_secs(4),
            forge::harness::transport::request(
                &self.state.join("socket"),
                br#"{"method":"status"}"#,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(reply["ok"], true, "{reply}");
        reply["result"].clone()
    }
    pub async fn connect(&self) -> Mcp {
        let mut command = std::process::Command::new(&self.binary);
        command
            .env_clear()
            .args(["harness", "stdio", "--socket"])
            .arg(self.state.join("socket"));
        let mut client = Mcp::from_command(command);
        client.send(json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25",
            "capabilities":{},"clientInfo":{"name":"test-report-control","version":"1"}}})).await;
        assert_eq!(
            client.receive().await["result"]["protocolVersion"],
            "2025-11-25"
        );
        client
            .send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .await;
        client
    }
    pub fn mode(&self, mode: &str) {
        write(&self.source.join("worker-mode-next"), mode.as_bytes());
        fs::rename(
            self.source.join("worker-mode-next"),
            self.source.join("worker-mode"),
        )
        .unwrap();
        write(
            &self.source.join("worker-denied"),
            self.denied.to_str().unwrap().as_bytes(),
        );
        let value = json!({"schema_version":1,"authority":"native-static-analysis","advisory":true,
            "root":self.source,"report":{"synthetic_fixture":true}});
        write(
            &self.source.join("worker-response.json"),
            &serde_json::to_vec(&value).unwrap(),
        );
        let mut large = value;
        large["report"]["padding"] = json!("x".repeat(16384));
        write(
            &self.source.join("worker-large-response.json"),
            &serde_json::to_vec(&large).unwrap(),
        );
    }
    pub async fn active(&self) -> (Value, Value) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let status = self.status().await;
                if let Some(scratch) = status["python_test_report"]["scratch_path"].as_str() {
                    match fs::read(Path::new(scratch).join("started.json")) {
                        Ok(bytes) => {
                            let value: Value = serde_json::from_slice(&bytes).unwrap();
                            self.bind_paths(&status);
                            self.save("active-witness", json!({"status":status,"witness":value}));
                            return (status, value);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                        Err(error) => panic!("witness read failed: {error}"),
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    pub async fn group_inventory(&self) -> Value {
        let status = self.status().await;
        let parent = self.owned_parent(&status);
        let mut groups = BTreeMap::new();
        let mut paths = vec![parent.clone()];
        for entry in fs::read_dir(&parent).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                paths.push(entry.path());
            }
            assert!(paths.len() <= 64);
        }
        for path in paths {
            let bytes = fs::read_to_string(path.join("cgroup.procs")).unwrap();
            assert!(bytes.len() <= 65536);
            let mut pids: Vec<u32> = bytes.lines().map(|line| line.parse().unwrap()).collect();
            pids.sort();
            groups.insert(path.to_str().unwrap().to_owned(), pids);
        }
        serde_json::to_value(groups).unwrap()
    }
    fn owned_parent(&self, status: &Value) -> PathBuf {
        let pid = status["pid"].as_u64().unwrap();
        let text = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
        let relative = text
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .unwrap();
        let supervisor = Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
        assert_eq!(supervisor.file_name().unwrap(), "supervisor");
        let parent = supervisor.parent().unwrap().to_owned();
        assert_eq!(
            parent.file_name().unwrap(),
            self.service.as_ref().unwrap().record().unit.as_str()
        );
        parent
    }
    pub fn bind_paths(&self, status: &Value) {
        let report = &status["python_test_report"];
        let scratch = Path::new(report["scratch_path"].as_str().unwrap());
        assert_eq!(scratch.parent(), Some(self.state.as_path()));
        assert!(scratch
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("test-report-"));
        let parent = self.owned_parent(status);
        assert_eq!(
            Path::new(report["cgroup_path"].as_str().unwrap()).parent(),
            Some(parent.as_path())
        );
    }
    pub async fn cleaned(&self, status: &Value) {
        self.bind_paths(status);
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let mut absent = true;
                for key in ["cgroup_path", "scratch_path"] {
                    match fs::symlink_metadata(status["python_test_report"][key].as_str().unwrap())
                    {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                        Ok(_) => absent = false,
                        Err(error) => panic!("owned path cannot be checked: {error}"),
                    }
                }
                if absent {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(self.status().await["python_test_report"]["fatal"], false);
        self.save(
            "cleanup-observation",
            json!({"recorded_paths":status["python_test_report"],"fresh_positive_enoent":true}),
        );
    }
    pub fn direct_command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(&self.binary);
        command
            .env_clear()
            .env("HOME", self.root.as_ref().unwrap().path())
            .env("XDG_CONFIG_HOME", self.root.as_ref().unwrap().path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(["analyze", "--root"])
            .arg(&self.source)
            .args(["--max-bytes", "262144", "tests"]);
        command
    }
    pub fn direct_output(&self, timeout: Duration) -> anyhow::Result<std::process::Output> {
        forge::process_probe::run(&mut self.direct_command(), timeout, 262145)
    }
    pub fn direct(&self) -> Value {
        let output = self.direct_output(Duration::from_secs(20)).unwrap();
        write(&self.state.join("direct.stdout"), &output.stdout);
        write(&self.state.join("direct.stderr"), &output.stderr);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    pub async fn finish(mut self) {
        let status = self.status().await;
        self.save("final-status", status.clone());
        if status["python_test_report"]["cgroup_path"].is_string() {
            self.cleaned(&status).await;
        }
        let parent = self.owned_parent(&status);
        let response = tokio::time::timeout(
            Duration::from_secs(8),
            forge::harness::transport::request(
                &self.state.join("socket"),
                br#"{"method":"shutdown"}"#,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response["ok"], true, "{response}");
        self.service.as_mut().unwrap().stop().unwrap();
        missing(&parent);
        for (path, expected) in &self.pins {
            assert_eq!(&sha(path), expected);
        }
        for (path, expected) in &self.source_pins {
            assert_eq!(&sha(path), expected);
        }
        self.save(
            "service-cleanup",
            json!({"owned_parent":parent,"fresh_positive_enoent":true}),
        );
        self.verified = true;
        let evidence = self.root.take().unwrap().keep();
        eprintln!(
            "verified test report evidence retained: {}",
            evidence.display()
        );
    }
}
