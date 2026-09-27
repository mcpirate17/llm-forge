//! Native child workloads shared by CLI integration tests.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    OnceLock,
};

pub struct Host(pub PathBuf);

impl Host {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "forge-tasks-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join(".git")).unwrap();
        Self(path)
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
        command
            .current_dir(&self.0)
            .env_remove("CONDUCTOR_HOST_ROOT")
            .env_remove("CLAUDE_PROJECT_DIR")
            .env("LEDGER_ROOT", self.0.join("ledger"));
        command
    }

    pub fn cli(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    pub fn ok(&self, args: &[&str]) -> Value {
        let result = self.cli(args);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice(&result.stdout).unwrap()
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Compile once per integration-test process, outside the tracked source tree.
pub fn task_worker() -> &'static Path {
    static WORKER: OnceLock<PathBuf> = OnceLock::new();
    WORKER.get_or_init(|| {
        let directory = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("forge-task-worker-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let executable = directory.join(format!("worker{}", std::env::consts::EXE_SUFFIX));
        let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/task_worker.rs");
        let output = Command::new(compiler)
            .args(["--edition=2021", "--crate-name=forge_task_test_worker"])
            .args(["-C", "codegen-units=1", "-C", "debuginfo=0"])
            .arg(source)
            .arg("-o")
            .arg(&executable)
            .output()
            .expect("launch Rust compiler for native task fixture");
        assert!(
            output.status.success(),
            "native task fixture compilation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        executable
    })
}
