//! Isolated host and executable protocol fixtures for the hook seam contracts.

use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

pub const PAYLOAD: &str =
    r#"{"session_id":"s1","hook_event_name":"SessionStart","source":"startup"}"#;

pub fn hooks() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/tooling/hooks/claude")
}

pub fn child() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_hook_project_seam_child"))
}

pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    pub fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "forge-hook-project-seam-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("create isolated hook seam scratch");
        Self { root }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    pub fn host(&self) -> PathBuf {
        let root = self.root.join("repo");
        let hook_dir = root.join("tooling/hooks/claude");
        fs::create_dir_all(&hook_dir).unwrap();
        for name in [
            "session-start.sh",
            "_identity.sh",
            "_append_context.py",
            "_prune.sh",
        ] {
            fs::copy(hooks().join(name), hook_dir.join(name)).unwrap();
        }
        for name in ["session-start.sh", "_append_context.py"] {
            executable(&hook_dir.join(name));
        }
        let gate = root.join("tooling/hooks/agent/crg_gate.py");
        link_child(&gate);
        let python = root.join(".venv/bin/python");
        link_child(&python);
        root
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove hook seam scratch");
    }
}

pub fn executable(path: &Path) {
    let mut mode = fs::metadata(path).unwrap().permissions();
    mode.set_mode(0o755);
    fs::set_permissions(path, mode).unwrap();
}

pub fn link_child(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    symlink(child(), path).unwrap();
}

pub fn project_hook(root: &Path, body: &str) -> PathBuf {
    let file = root.join(".claude/hooks/project/session-start.sh");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, body).unwrap();
    executable(&file);
    file
}

pub enum ForgeSelection<'a> {
    Disabled,
    Explicit(&'a Path),
    Automatic,
}

pub fn session(
    root: &Path,
    selection: ForgeSelection<'_>,
    python: Option<&Path>,
    path_front: Option<&Path>,
    marker: Option<&Path>,
) -> Output {
    let script = root.join("tooling/hooks/claude/session-start.sh");
    let mut command = Command::new("bash");
    command
        .arg(script)
        .current_dir(root)
        .env("A2A_AGENT_NAME", "")
        .env("PYTHONPATH", "")
        .env_remove("PROJECT_DIR")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("PROJECT_HOOK_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match selection {
        ForgeSelection::Disabled => {
            command.env("FORGE_BIN", "");
        }
        ForgeSelection::Explicit(path) => {
            command.env("FORGE_BIN", path);
        }
        ForgeSelection::Automatic => {
            command.env_remove("FORGE_BIN");
        }
    }
    if let Some(python) = python {
        command.env("HOOK_PYTHON", python);
    } else {
        command.env_remove("HOOK_PYTHON");
    }
    if let Some(marker) = marker {
        command.env("SEAM_FORGE_ARGS", marker);
    } else {
        command.env_remove("SEAM_FORGE_ARGS");
    }
    if let Some(path_front) = path_front {
        let mut paths = vec![path_front.to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        command.env("PATH", std::env::join_paths(paths).unwrap());
    }
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(PAYLOAD.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

pub fn context_text(output: &Output) -> String {
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid SessionStart output: {error}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("hook additionalContext")
        .to_owned()
}

pub fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status={:?}; stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn payload() -> Value {
    json!({"session_id":"s1", "hook_event_name":"SessionStart", "source":"startup"})
}

pub fn python() -> String {
    std::env::var("PYO3_PYTHON").unwrap_or_else(|_| "python3".to_owned())
}
