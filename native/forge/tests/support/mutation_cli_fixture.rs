//! Synthetic CLI protocol fixture. Delegation is required; never mutation evidence.
use super::*;

struct CargoFixture {
    root: Option<tempfile::TempDir>,
    repo: PathBuf,
    grant: PathBuf,
    pin: String,
    uncertain: std::rc::Rc<std::cell::Cell<bool>>,
}

impl CargoFixture {
    fn new(report: Value, engine_exit: i32, baseline_exit: i32) -> Self {
        let _guard = forks();
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let repo = root.path().join("repo");
        let tools = root.path().join("tools");
        let scratch = root.path().join("scratch");
        for dir in [
            repo.clone(),
            repo.join("src"),
            repo.join("tests"),
            repo.join("campaigns"),
            tools.clone(),
            scratch.clone(),
        ] {
            private_directory(&dir);
        }
        fs::write(repo.join("Cargo.toml"), "[package]\nname=\"fixture\"\nversion=\"0.1.0\"\nedition=\"2021\"\n\n[[test]]\nname=\"contract\"\npath=\"tests/contract.rs\"\n").unwrap();
        fs::write(
            repo.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(repo.join("src/lib.rs"), "pub fn value() -> bool { true }\n").unwrap();
        fs::write(
            repo.join("tests/contract.rs"),
            "#[test] fn check() { assert!(fixture::value()); }\n",
        )
        .unwrap();
        fs::write(repo.join(".gitignore"), "campaigns/receipts/\n").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);
        git(&repo, &["checkout", "-qb", "feat"]);
        fs::write(
            repo.join("src/lib.rs"),
            "pub fn value() -> bool { false }\n",
        )
        .unwrap();
        git(&repo, &["commit", "-qam", "change"]);
        let campaign = json!({"campaign_id":"fixture","title":"fixture","language":"rust",
            "mutation_engine":"cargo-mutants","generator":{"source":["src/lib.rs"],"jobs":1,
            "run_timeout_seconds":10,"options":{"manifest_path":"Cargo.toml","package":"fixture","package_root":""}},
            "test_argv":["cargo","test","--manifest-path","Cargo.toml","--package","fixture"],
            "source_sha256":{"src/lib.rs":digest(&repo.join("src/lib.rs"))},
            "test_sha256":{"tests/contract.rs":digest(&repo.join("tests/contract.rs"))}});
        fs::write(
            repo.join(MANIFEST),
            serde_json::to_vec_pretty(&campaign).unwrap(),
        )
        .unwrap();
        git(&repo, &["add", MANIFEST]);
        git(&repo, &["commit", "-qm", "campaign"]);
        let cargo = tools.join("cargo");
        stub(&cargo, &format!("#!/bin/sh\nexit {baseline_exit}\n"));
        let engine = tools.join("cargo-mutants");
        let mut report = report;
        for row in report["outcomes"].as_array_mut().unwrap() {
            if let Some(mutant) = row["scenario"].get_mut("Mutant") {
                mutant["file"] = json!("src/lib.rs");
            }
        }
        let encoded = report.to_string();
        stub(&engine, &format!("#!/bin/sh\nwhile [ $# -gt 0 ]; do [ \"$1\" = --output ] && output=$2; shift; done\n/usr/bin/mkdir -p \"$output/mutants.out\"\n/usr/bin/cat > \"$output/mutants.out/outcomes.json\" <<'REPORT'\n{encoded}\nREPORT\nexit {engine_exit}\n"));
        for name in ["rustc", "rustdoc"] {
            stub(&tools.join(name), "#!/bin/sh\nexit 0\n");
        }
        let parent = PathBuf::from(
            std::env::var_os("FORGE_TEST_CGROUP_PARENT")
                .expect("explicit delegated cgroup required"),
        )
        .canonicalize()
        .unwrap();
        let programs: serde_json::Map<String, Value> =
            ["cargo", "cargo-mutants", "rustc", "rustdoc"]
                .into_iter()
                .map(|name| {
                    let path = tools.join(name);
                    (name.into(), json!({"path":path,"sha256":digest(&path)}))
                })
                .collect();
        let read_only = vec![
            PathBuf::from("/usr"),
            PathBuf::from("/etc/ld.so.cache"),
            tools,
        ];
        let grant = root.path().join("grant.json");
        fs::write(&grant, serde_json::to_vec(&json!({"version":1,"source_root":repo,
            "scratch_parent":scratch,"cgroup_parent":parent,"read_only":read_only,"programs":programs,
            "resources":{"cpu_millis":1000,"memory_mib":256,"processes":64},"max_wall_seconds":30})).unwrap()).unwrap();
        fs::set_permissions(&grant, fs::Permissions::from_mode(0o600)).unwrap();
        let pin = digest(&grant);
        Self {
            root: Some(root),
            repo,
            grant,
            pin,
            uncertain: std::rc::Rc::new(std::cell::Cell::new(false)),
        }
    }
    fn mode(&mut self, action: &str, timeout: u64) {
        let engine = self
            .root
            .as_ref()
            .unwrap()
            .path()
            .join("tools/cargo-mutants");
        let text = fs::read_to_string(&engine).unwrap();
        fs::write(
            &engine,
            text.replacen("while [", &format!("{action}\nwhile ["), 1),
        )
        .unwrap();
        let mut grant: Value = serde_json::from_slice(&fs::read(&self.grant).unwrap()).unwrap();
        grant["programs"]["cargo-mutants"]["sha256"] = json!(digest(&engine));
        fs::write(&self.grant, serde_json::to_vec(&grant).unwrap()).unwrap();
        self.pin = digest(&self.grant);
        let path = self.repo.join(MANIFEST);
        let mut campaign: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        if campaign["generator"]["run_timeout_seconds"] == timeout {
            return;
        }
        campaign["generator"]["run_timeout_seconds"] = json!(timeout);
        fs::write(&path, serde_json::to_vec_pretty(&campaign).unwrap()).unwrap();
        let _guard = forks();
        git(&self.repo, &["add", MANIFEST]);
        git(&self.repo, &["commit", "-qm", "fixture mode"]);
    }
    fn spawn(&self) -> OwnedChild {
        self.spawn_with_receipt(RECEIPT)
    }
    fn spawn_with_receipt(&self, receipt: &str) -> OwnedChild {
        self.uncertain.set(true);
        let child = Command::new(forge_binary::path())
            .current_dir(&self.repo)
            .args([
                "mutation",
                "run",
                MANIFEST,
                "--allow-mutations",
                "--owner",
                "lane",
                "--base",
                "main",
                "--receipt",
                receipt,
                "--execution-grants",
            ])
            .arg(&self.grant)
            .arg("--execution-grants-sha256")
            .arg(&self.pin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        OwnedChild {
            child: Some(child),
            uncertain: self.uncertain.clone(),
            receipt: self.repo.join(RECEIPT),
        }
    }
    fn finish(&self, mut owner: OwnedChild) -> Run {
        use std::io::Read;
        use std::sync::mpsc;
        let child = owner.child.as_mut().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel();
        for (name, stream) in [
            ("stdout", Box::new(stdout) as Box<dyn Read + Send>),
            ("stderr", Box::new(stderr) as Box<dyn Read + Send>),
        ] {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let result = stream
                    .take(2 * 1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .and_then(|_| {
                        if bytes.len() <= 2 * 1024 * 1024 {
                            Ok(bytes)
                        } else {
                            Err(std::io::Error::other("fixture output overflow"))
                        }
                    });
                let _ = tx.send((name, result));
            });
        }
        drop(tx);
        let status = owner
            .wait_bounded(Duration::from_secs(20))
            .expect("owned CLI status");
        let mut captured = std::collections::BTreeMap::new();
        for _ in 0..2 {
            let (name, bytes) = rx.recv_timeout(Duration::from_secs(3)).unwrap_or_else(|_| {
                owner.uncertain.set(true);
                panic!(
                    "CLI output drain unresolved; retained receipt {}",
                    owner.receipt.display()
                );
            });
            let bytes = bytes.unwrap_or_else(|error| {
                owner.uncertain.set(true);
                panic!("CLI output read failed: {error}");
            });
            captured.insert(name, bytes);
        }
        let stdout = &captured["stdout"];
        let stderr = &captured["stderr"];
        let printed: Value = serde_json::from_slice(stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(stderr)));
        let receipt = match fs::read(self.repo.join(RECEIPT)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Null,
            Err(error) => panic!("receipt read failed: {error}"),
        };
        self.settle(&printed, &receipt);
        Run {
            code: status.code(),
            printed,
            receipt,
        }
    }
    fn settle(&self, printed: &Value, receipt: &Value) {
        if receipt.is_null() {
            let scratch = self.root.as_ref().unwrap().path().join("scratch");
            if printed["status"] == "REFUSED"
                && fs::read_dir(scratch).is_ok_and(|mut entries| entries.next().is_none())
            {
                self.uncertain.set(false);
            }
            return;
        }
        let Some(commands) = receipt["process_commands"].as_array() else {
            return;
        };
        let complete = !commands.is_empty()
            && receipt["process_cleanup"]["complete"] == true
            && receipt["process_cleanup"]["commands_observed"].as_u64()
                == Some(commands.len() as u64)
            && commands.iter().all(|row| {
                row["state"] == "observed"
                    && row["confinement"]["cleanup_complete"] == true
                    && row["confinement"]["resources"]["verified"] == true
                    && row["confinement"]["resources"]["path"]
                        .as_str()
                        .is_some_and(|path| absent(Path::new(path)))
            });
        let snapshot = receipt["snapshot"]["root"].as_str().is_some_and(|path| {
            if receipt["snapshot"]["removed"] == true {
                absent(Path::new(path))
            } else {
                fs::symlink_metadata(path).is_ok()
            }
        });
        if complete && snapshot {
            self.uncertain.set(false);
        }
    }
    fn run(&self) -> Run {
        self.finish(self.spawn())
    }
}
struct OwnedChild {
    child: Option<Child>,
    uncertain: std::rc::Rc<std::cell::Cell<bool>>,
    receipt: PathBuf,
}
impl OwnedChild {
    fn wait_bounded(&mut self, limit: Duration) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + limit;
        loop {
            let child = self.child.as_mut()?;
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.child.take();
                    return Some(status);
                }
                Ok(None) if Instant::now() < deadline => sleep(Duration::from_millis(10)),
                _ => return None,
            }
        }
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_ref() {
            self.uncertain.set(true);
            let pid = child.id();
            // PID is this still-owned, unreaped direct child; ask its actor to stop.
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            if self.wait_bounded(Duration::from_secs(5)).is_none() {
                self.uncertain.set(true);
                eprintln!(
                    "unresolved owned CLI pid {pid}; receipt {}",
                    self.receipt.display()
                );
            }
        }
    }
}

impl Drop for CargoFixture {
    fn drop(&mut self) {
        // Preserve the owner root when a pending/unknown snapshot may still exist.
        let scratch = self.root.as_ref().unwrap().path().join("scratch");
        if self.uncertain.get()
            || !fs::read_dir(scratch)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false)
        {
            let path = self.root.take().unwrap().keep();
            eprintln!("retained uncertain CLI fixture at {}", path.display());
        }
    }
}

#[test]
#[ignore = "requires explicit FORGE_TEST_CGROUP_PARENT delegation"]
fn confined_cargo_public_cli_passes_and_cleans_owned_resources() {
    let host = CargoFixture::new(
        cargo_report("Success", &["CaughtMutant", "CaughtMutant"]),
        0,
        0,
    );
    let run = host.run();
    assert_eq!(run.code, Some(0), "{:?}", run.printed);
    assert_eq!(run.receipt["status"], "PASS");
    assert_eq!(run.receipt["outcome_counts"]["KILLED"], 2);
    assert_eq!(
        run.receipt["accounting"],
        json!({"planned":2,"completed":2,"failed":0,"censored":0,"unstarted":0})
    );
    assert_eq!(run.receipt["complete_campaign"], true);
    assert_eq!(
        run.receipt["identity_check"],
        json!({"snapshot_drift":{},"repository_drift":{}})
    );
    assert_eq!(run.receipt["runner_kind"], "forge-native");
    assert_eq!(run.receipt["receipt_path"], RECEIPT);
    assert_eq!(run.receipt["snapshot"]["removed"], true);
    assert_eq!(run.receipt["process_cleanup"]["complete"], true);
    assert!(absent(Path::new(
        run.receipt["snapshot"]["root"].as_str().unwrap()
    )));
    for row in run.receipt["process_commands"].as_array().unwrap() {
        assert_eq!(row["state"], "observed");
        assert_eq!(row["confinement"]["cleanup_complete"], true);
        assert!(absent(Path::new(
            row["confinement"]["resources"]["path"].as_str().unwrap()
        )));
    }
}

#[test]
#[ignore = "requires explicit FORGE_TEST_CGROUP_PARENT delegation"]
fn confined_cargo_public_cli_scores_survivor_timeout_and_red_baseline() {
    for (baseline, statuses, code, expected_code, expected_status) in [
        (
            "Success",
            vec!["CaughtMutant", "MissedMutant"],
            2,
            0,
            "RATCHET_HELD",
        ),
        (
            "Success",
            vec!["CaughtMutant", "Timeout"],
            3,
            1,
            "INCOMPLETE",
        ),
        ("Failure", vec!["CaughtMutant"], 4, 4, "ERROR"),
    ] {
        let host = CargoFixture::new(cargo_report(baseline, &statuses), code, 0);
        let run = host.run();
        assert_eq!(run.code, Some(expected_code), "{:?}", run.printed);
        assert_eq!(run.receipt["status"], expected_status);
        assert_eq!(run.receipt["snapshot"]["removed"], true);
    }
}

#[test]
#[ignore = "requires explicit FORGE_TEST_CGROUP_PARENT delegation"]
fn confined_cargo_cli_refuses_snapshot_source_and_test_drift() {
    for (action, path) in [
        ("echo '// changed' >> src/lib.rs", "src/lib.rs"),
        (
            "echo '// changed' >> tests/contract.rs",
            "tests/contract.rs",
        ),
    ] {
        let mut host = CargoFixture::new(cargo_report("Success", &["CaughtMutant"]), 0, 0);
        host.mode(action, 10);
        let original = fs::read_to_string(host.repo.join(path)).unwrap();
        let run = host.run();
        let retained = assert_held_drift_refused(&host, &run);
        assert_eq!(fs::read_to_string(host.repo.join(path)).unwrap(), original);
        assert_eq!(
            fs::read_to_string(retained.join("worktree").join(path)).unwrap(),
            format!("{original}// changed\n")
        );
    }
}

#[test]
#[ignore = "requires explicit FORGE_TEST_CGROUP_PARENT delegation"]
fn confined_cargo_cli_times_out_and_removes_cgroup() {
    let mut host = CargoFixture::new(cargo_report("Success", &["CaughtMutant"]), 0, 0);
    host.mode("/usr/bin/sleep 15", 1);
    let run = host.run();
    assert_eq!(run.code, Some(REFUSED), "{:?}", run.printed);
    assert_eq!(run.receipt["status"], "ERROR");
    assert_eq!(run.receipt["snapshot"]["removed"], true);
    for row in run.receipt["process_commands"].as_array().unwrap() {
        assert_eq!(row["confinement"]["cleanup_complete"], true);
        assert!(absent(Path::new(
            row["confinement"]["resources"]["path"].as_str().unwrap()
        )));
    }
}

#[test]
#[ignore = "requires explicit FORGE_TEST_CGROUP_PARENT delegation"]
fn confined_cargo_cli_cancellation_waits_for_owned_cleanup() {
    let mut host = CargoFixture::new(cargo_report("Success", &["CaughtMutant"]), 0, 0);
    host.mode("/usr/bin/sleep 15", 10);
    let child = host.spawn();
    wait_for_engine(&host);
    let pid = child.child.as_ref().unwrap().id() as i32;
    // The child is our unreaped process, and its signal owner handles cleanup.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let run = host.finish(child);
    assert_eq!(run.code, Some(1), "{:?}", run.printed);
    assert_eq!(run.receipt["status"], "CANCELLED");
    assert_eq!(run.receipt["snapshot"]["removed"], true);
    for row in run.receipt["process_commands"].as_array().unwrap() {
        assert_eq!(row["confinement"]["cleanup_complete"], true);
        assert!(absent(Path::new(
            row["confinement"]["resources"]["path"].as_str().unwrap()
        )));
    }
}

fn wait_for_engine(host: &CargoFixture) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let value: Option<Value> = fs::read(host.repo.join(RECEIPT))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        if value
            .as_ref()
            .and_then(|v| v["process_commands"].as_array())
            .is_some_and(|v| v.len() == 2 && v[1]["pending_cgroup"].is_object())
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "engine ownership was not published"
        );
        sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires explicit FORGE_TEST_CGROUP_PARENT delegation"]
fn confined_cargo_cli_refuses_live_source_drift() {
    let mut host = CargoFixture::new(cargo_report("Success", &["CaughtMutant"]), 0, 0);
    host.mode("/usr/bin/sleep 2", 10);
    let child = host.spawn();
    wait_for_engine(&host);
    fs::write(
        host.repo.join("src/lib.rs"),
        "pub fn value() -> bool { true }\n",
    )
    .unwrap();
    let run = host.finish(child);
    let retained = assert_held_drift_refused(&host, &run);
    assert_eq!(
        fs::read_to_string(host.repo.join("src/lib.rs")).unwrap(),
        "pub fn value() -> bool { true }\n"
    );
    assert_eq!(
        fs::read_to_string(retained.join("worktree/src/lib.rs")).unwrap(),
        "pub fn value() -> bool { false }\n"
    );
}

#[test]
#[ignore = "requires explicit FORGE_TEST_CGROUP_PARENT delegation"]
fn confined_cargo_cli_retains_snapshot_on_removal_failure() {
    let mut host = CargoFixture::new(cargo_report("Success", &["CaughtMutant"]), 0, 0);
    host.mode(
        "/usr/bin/mkdir locked; : > locked/held; /usr/bin/chmod 0500 locked",
        10,
    );
    let before = fs::read(host.repo.join(MANIFEST)).unwrap();
    let run = host.run();
    assert_eq!(run.code, Some(REFUSED), "{:?}", run.printed);
    assert_eq!(run.receipt["status"], "ERROR");
    assert_eq!(run.receipt["snapshot"]["removed"], false);
    assert!(run.receipt["snapshot"]["removal_error"].as_str().is_some());
    assert_eq!(fs::read(host.repo.join(MANIFEST)).unwrap(), before);
    for row in run.receipt["process_commands"].as_array().unwrap() {
        assert_eq!(row["confinement"]["cleanup_complete"], true);
        assert!(absent(Path::new(
            row["confinement"]["resources"]["path"].as_str().unwrap()
        )));
    }
    let root = PathBuf::from(run.receipt["snapshot"]["root"].as_str().unwrap());
    let scratch = host
        .root
        .as_ref()
        .unwrap()
        .path()
        .join("scratch")
        .canonicalize()
        .unwrap();
    let owned = root.canonicalize().unwrap();
    assert_eq!(owned.parent(), Some(scratch.as_path()));
    use std::os::unix::fs::MetadataExt;
    assert_eq!(fs::metadata(&owned).unwrap().uid(), unsafe {
        libc::geteuid()
    });
    let locked = owned.join("worktree/locked");
    assert_eq!(locked.canonicalize().unwrap(), locked);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_dir_all(&owned).unwrap();
}

#[test]
#[ignore = "requires explicit FORGE_TEST_CGROUP_PARENT delegation"]
fn confined_cargo_cli_rejects_external_receipt_path_before_payload() {
    let host = CargoFixture::new(cargo_report("Success", &["CaughtMutant"]), 0, 0);
    let run = host.finish(host.spawn_with_receipt("/tmp/elsewhere.json"));
    assert_eq!(run.code, Some(REFUSED));
    assert_eq!(
        run.printed["error"],
        "receipt path must be inside the repository"
    );
    assert!(!host.repo.join(RECEIPT).exists());
}

#[test]
#[ignore = "requires explicit FORGE_TEST_CGROUP_PARENT delegation"]
fn confined_cargo_cli_refuses_live_drift_before_launch_and_red_baseline() {
    let host = CargoFixture::new(cargo_report("Success", &["CaughtMutant"]), 0, 0);
    fs::write(
        host.repo.join("src/lib.rs"),
        "pub fn value() -> bool { true }\n",
    )
    .unwrap();
    let run = host.run();
    assert_eq!(run.code, Some(REFUSED), "{:?}", run.printed);
    assert!(run.receipt.is_null());
    let host = CargoFixture::new(cargo_report("Success", &["CaughtMutant"]), 0, 1);
    let before = fs::read(host.repo.join(MANIFEST)).unwrap();
    let run = host.run();
    assert_eq!(run.code, Some(REFUSED));
    assert_eq!(run.printed["status"], "REFUSED");
    assert_eq!(run.receipt["status"], "BASELINE_FAILED");
    assert!(error_of(&run).contains("unmutated baseline failed"));
    assert!(run.receipt.get("engine_result").is_none());
    assert_eq!(run.receipt["process_commands"].as_array().unwrap().len(), 1);
    assert_eq!(fs::read(host.repo.join(MANIFEST)).unwrap(), before);
    assert_eq!(run.receipt["snapshot"]["removed"], true);
}

#[test]
#[ignore = "requires explicit FORGE_TEST_CGROUP_PARENT delegation"]
fn confined_cargo_cli_rejects_unexplained_engine_exits() {
    for code in [1, 4, 70] {
        let host = CargoFixture::new(
            cargo_report("Success", &["CaughtMutant", "MissedMutant"]),
            code,
            0,
        );
        let run = host.run();
        assert_eq!(run.code, Some(REFUSED), "{:?}", run.printed);
        assert_eq!(run.receipt["status"], "ERROR");
        assert!(run.printed["error"]
            .as_str()
            .unwrap()
            .contains(&format!("cargo-mutants failed (rc={code})")));
    }
    for code in [2, 3] {
        let host = CargoFixture::new(
            cargo_report("Success", &["CaughtMutant", "CaughtMutant"]),
            code,
            0,
        );
        let run = host.run();
        assert_eq!(run.code, Some(1), "{:?}", run.printed);
        assert_eq!(run.receipt["status"], "INCOMPLETE");
    }
}

/// Held-input drift is discovered before the later identity report. The owner
/// has completed its groups, but the unverified snapshot remains for inspection.
fn assert_held_drift_refused(host: &CargoFixture, run: &Run) -> PathBuf {
    assert_eq!(run.code, Some(REFUSED), "{:?}", run.printed);
    assert_eq!(run.receipt["status"], "ERROR");
    assert!(run.receipt["error"]
        .as_str()
        .unwrap()
        .contains("input identity changed"));
    assert!(run.receipt["execution_verification_error"]
        .as_str()
        .unwrap()
        .contains("input identity changed"));
    assert!(run.receipt["identity_check"].is_null());
    assert_eq!(run.receipt["snapshot"]["removed"], false);
    assert_eq!(
        run.receipt["snapshot"]["retained_reason"],
        "process cleanup or execution input verification is incomplete"
    );
    assert_eq!(run.receipt["process_cleanup"]["complete"], true);
    assert_eq!(run.receipt["process_cleanup"]["commands_observed"], 2);
    let commands = run.receipt["process_commands"].as_array().unwrap();
    assert_eq!(commands.len(), 2);
    for row in commands {
        assert_eq!(row["state"], "observed");
        assert_eq!(row["confinement"]["cleanup_complete"], true);
        assert!(absent(Path::new(
            row["confinement"]["resources"]["path"].as_str().unwrap()
        )));
    }
    let scratch = host
        .root
        .as_ref()
        .unwrap()
        .path()
        .join("scratch")
        .canonicalize()
        .unwrap();
    let root = PathBuf::from(run.receipt["snapshot"]["root"].as_str().unwrap());
    assert_eq!(root.canonicalize().unwrap(), root);
    assert_eq!(root.parent(), Some(scratch.as_path()));
    assert!(fs::symlink_metadata(root.join("worktree"))
        .unwrap()
        .is_dir());
    root
}

fn absent(path: &Path) -> bool {
    matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
}

fn private_directory(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
