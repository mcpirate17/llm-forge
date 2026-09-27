//! End-to-end native PreToolUse coverage. A shell sentinel stands in for the
//! Python dispatcher: any accidental delegation changes exit status and leaves
//! a marker in the scratch checkout.

use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Project(PathBuf);

impl Project {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "native-pre-edit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join(".git")).unwrap();
        fs::write(path.join(".git/HEAD"), "ref: refs/heads/claude-lane\n").unwrap();
        let sentinel = path.join("dispatcher-sentinel");
        fs::write(
            &sentinel,
            "#!/bin/sh\nprintf '%s' \"$FORGE_NATIVE_HOOKS\" > \"$CLAUDE_PROJECT_DIR/delegated.names\"\nprintf '{\"delegated\":true}\\n'\nexit 17\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o755)).unwrap();
        Project(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn run(&self, payload: &Value, native_hooks: Option<&str>, standalone: bool) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_forge"));
        command
            .args(["hook", "PreToolUse"])
            .current_dir(self.path())
            .env("CLAUDE_PROJECT_DIR", self.path())
            .env("PROJECT_DIR", self.path())
            .env("CRG_GATE_REPO_ROOT", self.path())
            .env("CRG_GATE_STATE_DIR", self.path().join("state"))
            .env("CRG_DATA_DIR", self.path().join("graph"))
            .env("GOVERNANCE_OWNER", "claude-lane")
            .env(
                "CONDUCTOR_SNAPSHOT_PYTHON",
                self.path().join("dispatcher-sentinel"),
            )
            .env("CONDUCTOR_PYTHON", "/bin/false")
            .env(
                "CONTEXT_TELEMETRY_PATH",
                self.path().join("telemetry.jsonl"),
            )
            .env_remove("FORGE_NATIVE_HOOKS")
            .env_remove("FORGE_NATIVE_ANSWERS")
            .env_remove("FORGE_HOOK_STANDALONE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(hooks) = native_hooks {
            command.env("FORGE_NATIVE_HOOKS", hooks);
        }
        if standalone {
            command.env("FORGE_HOOK_STANDALONE", "1");
        }
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn native(&self, payload: &Value, standalone: bool) -> Value {
        let output = self.run(payload, None, standalone);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(!self.path().join("delegated.names").exists());
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn graph(session: &str) -> Value {
    json!({"session_id": session, "tool_name": "mcp__code_review_graph__query_graph"})
}

fn edit(session: &str, tool: &str, path: &str) -> Value {
    json!({"session_id": session, "tool_name": tool, "tool_input": {"file_path": path}})
}

#[test]
fn graph_mark_then_edit_uses_the_native_gate_without_delegation() {
    let project = Project::new();
    let marked = project.native(&graph("s1"), false);
    assert_eq!(marked["hookSpecificOutput"]["hookEventName"], "PreToolUse");

    let outside = project.path().with_extension("scratch.txt");
    let allowed = project.native(&edit("s1", "Edit", &outside.to_string_lossy()), false);
    assert_ne!(allowed["hookSpecificOutput"]["permissionDecision"], "deny");

    let denied = project.native(&edit("s1", "Edit", "unclaimed.rs"), false);
    let reason = denied["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap();
    assert!(reason.contains("no live exact claim"), "{reason}");
}

#[test]
fn every_edit_tool_denies_before_graph_use_without_starting_python() {
    for tool in ["Edit", "Write", "NotebookEdit"] {
        let project = Project::new();
        let output = project.native(&edit("new", tool, "target.rs"), false);
        assert!(output["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("call a code-review-graph MCP tool"));
    }
}

#[test]
fn standalone_edit_and_graph_tools_keep_the_same_gate() {
    let project = Project::new();
    project.native(&graph("standalone"), true);
    let denied = project.native(&edit("standalone", "Write", "unclaimed.rs"), true);
    assert!(denied["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap()
        .contains("no live exact claim"));
}

#[test]
fn partial_native_selection_delegates_and_names_only_answered_hooks() {
    let project = Project::new();
    let output = project.run(
        &edit("partial", "Edit", "unclaimed.rs"),
        Some("crg_refresh_report_pre,current_work_guard_edit"),
        false,
    );
    assert_eq!(output.status.code(), Some(17));
    let names = fs::read_to_string(project.path().join("delegated.names")).unwrap();
    assert!(names.contains("current_work_guard_edit"));
    assert!(!names.contains("crg_gate_verify"));
}

#[test]
#[ignore = "bounded local latency probe; run explicitly after the correctness suite"]
fn native_edit_hook_latency_probe() {
    let project = Project::new();
    let payload = edit("probe", "Edit", "target.rs");
    let mut samples = Vec::with_capacity(32);
    for _ in 0..32 {
        let started = Instant::now();
        project.native(&payload, false);
        samples.push(started.elapsed().as_micros());
    }
    samples.sort_unstable();
    eprintln!(
        "native Edit PreToolUse: n=32 median={}us p95={}us",
        samples[16], samples[30]
    );
}
