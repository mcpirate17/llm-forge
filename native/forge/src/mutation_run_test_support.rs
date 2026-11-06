//! Manifests, receipts and Git repositories for the mutation runner's unit tests.

use crate::mutation_campaign::Campaign;
use crate::mutation_score::{Receipt, RUNNING};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::Command;

/// A loadable manifest in `dir`, top-level keys replaced by `overrides`.
pub(crate) fn manifest(dir: &Path, overrides: Value) -> std::path::PathBuf {
    let pin = "0".repeat(64);
    let mut payload = json!({
        "campaign_id": "fixture",
        "title": "fixture",
        "language": "python",
        "mutation_engine": "fest",
        "generator": {"source": ["src/a.py"], "run_timeout_seconds": 60},
        "test_argv": ["python", "-m", "pytest", "tests/test_a.py"],
        "source_sha256": {"src/a.py": pin},
        "test_sha256": {"tests/test_a.py": pin},
    });
    let fields = payload.as_object_mut().expect("object");
    for (key, value) in overrides.as_object().expect("overrides are an object") {
        fields.insert(key.clone(), value.clone());
    }
    let path = dir.join("campaign.json");
    fs::write(&path, payload.to_string()).expect("write manifest");
    path
}

pub(crate) fn campaign(dir: &Path, overrides: Value) -> Campaign {
    Campaign::load(&manifest(dir, overrides)).expect("fixture manifest loads")
}

/// A running receipt holding one row per `(id, outcome)`.
pub(crate) fn receipt(outcomes: &[(&str, &str)]) -> Receipt {
    let rows: Vec<Value> = outcomes
        .iter()
        .map(|(id, outcome)| json!({"id": id, "outcome": outcome}))
        .collect();
    let mut receipt = Receipt::new();
    receipt.insert("status".into(), json!(RUNNING));
    receipt.insert("mutants".into(), Value::Array(rows));
    receipt
}

pub(crate) fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(output.status.success(), "{args:?}: {output:?}");
}

pub(crate) fn write(root: &Path, relative: &str, body: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, body).expect("write");
}

/// A repository with `files` committed on `main`, checked out on branch `feat`.
pub(crate) fn repository(files: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    for file in files {
        write(root, file, "x = 1\n");
    }
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "base", "--allow-empty"]);
    git(root, &["checkout", "-q", "-b", "feat"]);
    dir
}

/// Commit a change to `files`, so they are this branch's diff against `main`.
pub(crate) fn change(root: &Path, files: &[&str]) {
    for file in files {
        write(root, file, "x = 2\n");
    }
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "change"]);
}

/// Inspect an adapter without authorizing any payload process.
pub(crate) fn with_run<T>(
    root: &Path,
    interpreter: Option<&str>,
    overrides: Value,
    body: impl FnOnce(&mut crate::mutation_run::EngineRun<'_>) -> T,
) -> T {
    with_session(root, interpreter, overrides, None, body)
}

pub(crate) fn with_session<T>(
    root: &Path,
    interpreter: Option<&str>,
    overrides: Value,
    session: Option<&crate::mutation_execution::CommandSession<'_>>,
    body: impl FnOnce(&mut crate::mutation_run::EngineRun<'_>) -> T,
) -> T {
    let mut subject = campaign(root, overrides);
    let mut receipt = Receipt::new();
    let output = root.join("receipt.json");
    let _owner = crate::process_signals::TEST_OWNER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let interrupts = crate::process_signals::Interrupts::install().unwrap();
    let supervisor = crate::mutation_process::Supervisor {
        interrupts: &interrupts,
        registry: root.join("native_pgids.json"),
        memory_cap: None,
    };
    let mut run = crate::mutation_run::EngineRun {
        campaign: &mut subject,
        receipt: &mut receipt,
        output_path: &output,
        repo_root: root,
        worktree: root,
        interpreter,
        supervisor: &supervisor,
        runtime: None,
        test_session: session,
        planned: None,
    };
    body(&mut run)
}

/// A test-owned sandbox admitted while empty, then populated by its test.
/// All payloads still use the production confined actor and owned cgroups.
/// Callers must be explicitly ignored unless real delegation is supplied.
pub(crate) struct Delegated {
    root: Option<tempfile::TempDir>,
    sandbox: crate::sandbox::Sandbox,
    reactor: tokio::runtime::Runtime,
    parent: std::path::PathBuf,
    cleanup: std::cell::Cell<bool>,
    verified: std::cell::Cell<bool>,
    signal: std::cell::Cell<Option<i32>>,
    programs: std::collections::BTreeMap<String, std::path::PathBuf>,
}
impl Delegated {
    pub(crate) fn new(interpreter: Option<&str>) -> Self {
        use std::collections::BTreeMap;
        let parent = std::env::var_os("FORGE_TEST_CGROUP_PARENT")
            .expect("explicit FORGE_TEST_CGROUP_PARENT delegation is required");
        let parent = std::path::PathBuf::from(parent).canonicalize().unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            root.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let path = root.path();
        let cache = crate::mutation_bytecode::scratch_root_for(path);
        let mut environment = BTreeMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("LANG".into(), "C".into()),
            (
                "PYTHONPYCACHEPREFIX".into(),
                cache.join("pycache").to_string_lossy().into_owned(),
            ),
        ]);
        if let Some(python) = interpreter {
            environment.insert("CONDUCTOR_SNAPSHOT_PYTHON".into(), python.into());
        }
        let reads = ["/usr", "/lib", "/lib64", "/etc/ld.so.cache"]
            .into_iter()
            .map(std::path::PathBuf::from)
            .collect();
        let sandbox = crate::sandbox::Sandbox::prepare(crate::sandbox::Policy {
            read_only: reads,
            scratch: path.into(),
            working_directory: path.into(),
            environment: environment.clone(),
        })
        .expect("strict test sandbox must be available");
        // Prepare bytecode writes only AFTER the empty-root sandbox admission.
        let isolated =
            crate::mutation_bytecode::isolated_python_env(&environment, &cache, &[]).unwrap();
        assert_eq!(isolated, environment);
        let reactor = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(2)
            .build()
            .unwrap();
        Self {
            root: Some(root),
            sandbox,
            reactor,
            parent,
            cleanup: std::cell::Cell::new(true),
            verified: std::cell::Cell::new(false),
            signal: std::cell::Cell::new(None),
            programs: BTreeMap::from([("sh".into(), "/bin/sh".into())]),
        }
    }
    pub(crate) fn path(&self) -> &Path {
        self.root.as_ref().unwrap().path()
    }
    pub(crate) fn session(&self) -> crate::mutation_execution::CommandSession<'_> {
        self.verified.set(false);
        crate::mutation_execution::CommandSession {
            sandbox: &self.sandbox,
            reactor: &self.reactor,
            cgroup_parent: &self.parent,
            limits: crate::cgroup::Limits {
                cpu_millis: 1000,
                memory_mib: 128,
                processes: 32,
            },
            cleanup_complete: &self.cleanup,
            signal: &self.signal,
            programs: Some(&self.programs),
            scratch: self.path(),
            max_wall_seconds: 60,
            cargo_value: false,
            private_files: false,
        }
    }
    pub(crate) fn with_run<T>(
        &self,
        interpreter: Option<&str>,
        overrides: Value,
        body: impl FnOnce(&mut crate::mutation_run::EngineRun<'_>) -> T,
    ) -> T {
        let session = self.session();
        let result = with_session(self.path(), interpreter, overrides, Some(&session), |run| {
            let result = body(run);
            self.assert_ledger(run.receipt);
            result
        });
        self.assert_clean();
        result
    }
    pub(crate) fn assert_ledger(&self, receipt: &Receipt) {
        self.verified.set(false);
        let commands = receipt["process_commands"]
            .as_array()
            .expect("actor command ledger");
        let mut valid = !commands.is_empty()
            && receipt["process_cleanup"]["complete"] == true
            && receipt["process_cleanup"]["commands_observed"].as_u64()
                == Some(commands.len() as u64);
        for command in commands {
            let confinement = &command["confinement"];
            let absent = confinement["resources"]["path"].as_str()
                .is_some_and(|path| matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound));
            valid &=
                command["state"] == "observed" && confinement["cleanup_complete"] == true && absent;
        }
        if !valid {
            self.cleanup.set(false);
        }
        self.assert_clean();
        self.verified.set(true);
    }
    pub(crate) fn assert_clean(&self) {
        assert!(
            self.cleanup.get(),
            "confined test cleanup is unresolved; root will be retained"
        );
    }
}
impl Drop for Delegated {
    fn drop(&mut self) {
        if !self.cleanup.get() || !self.verified.get() || std::thread::panicking() {
            if let Some(root) = self.root.take() {
                eprintln!(
                    "retained unresolved confined test root: {}",
                    root.keep().display()
                );
            }
        }
    }
}
