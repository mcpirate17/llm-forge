//! Native Read and generic PreToolUse contracts. The delegation sentinel is
//! a shell script: these tests never execute a Python interpreter.

#[path = "../src/pre_read.rs"]
mod pre_read;

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
            "native-read-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        let path = fs::canonicalize(path).unwrap();
        let sentinel = path.join("dispatcher-sentinel");
        fs::write(
            &sentinel,
            concat!(
            "#!/bin/sh\n",
            "cat > \"$CLAUDE_PROJECT_DIR/delegated.input\"\n",
            "printf '%s' \"$FORGE_NATIVE_HOOKS\" > \"$CLAUDE_PROJECT_DIR/delegated.names\"\n",
            "printf '%s' \"$FORGE_NATIVE_ANSWERS\" > \"$CLAUDE_PROJECT_DIR/delegated.answers\"\n",
            "printf '{\"delegated\":true}\\n'\nexit 17\n",
        ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(sentinel, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self(path)
    }

    fn file(&self, name: &str, content: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, content).unwrap();
        path
    }

    fn run(&self, payload: &str, native_hooks: Option<&str>, standalone: bool) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_forge"));
        cmd.args(["hook", "PreToolUse"])
            .current_dir(&self.0)
            .env("CLAUDE_PROJECT_DIR", &self.0)
            .env(
                "CONDUCTOR_SNAPSHOT_PYTHON",
                self.0.join("dispatcher-sentinel"),
            )
            .env("CONDUCTOR_PYTHON", "/bin/false")
            .env("CONTEXT_TELEMETRY_PATH", self.0.join("telemetry.jsonl"))
            .env("CRG_DATA_DIR", self.0.join(".code-review-graph"))
            .env_remove("FORGE_MODE")
            .env_remove("FORGE_NATIVE_ANSWERS")
            .env_remove("FORGE_HOOK_STANDALONE")
            .env_remove("FORGE_NATIVE_HOOKS")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(names) = native_hooks {
            cmd.env("FORGE_NATIVE_HOOKS", names);
        }
        if standalone {
            cmd.env("FORGE_HOOK_STANDALONE", "1");
        }
        let mut child = cmd.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn native(&self, payload: &Value, standalone: bool) -> Value {
        let out = self.run(&payload.to_string(), None, standalone);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            !self.0.join("delegated.input").exists(),
            "dispatcher unexpectedly started"
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn stage_failure(&self) {
        let store = self.0.join(".code-review-graph");
        fs::create_dir_all(&store).unwrap();
        fs::write(
            store.join("refresh.failed"),
            "{\"kind\":\"failure\",\"text\":\"offline\"}\n",
        )
        .unwrap();
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn read(path: &Path) -> Value {
    json!({"tool_name": "Read", "tool_input": {"file_path": path}, "session_id": "read-session"})
}

fn reason(output: &Value) -> &str {
    output["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or("")
}

#[test]
fn read_denial_preserves_the_exact_message_and_telemetry_without_an_interpreter() {
    let project = Project::new();
    let path = project.file("large.py", "abcdefghij\n".repeat(400));
    let output = project.native(&read(&path), false);
    assert_eq!(output["hookSpecificOutput"]["permissionDecision"], "deny");
    assert_eq!(reason(&output), concat!(
        "PRE-READ DENIED: large.py is 400 lines (~1,100 tokens); whole-file ",
        "Read of a code file >= 400 lines is not allowed (KB-OPS-CTX-01). ",
        "Use mcp__code-review-graph__ast_context_tool(file_path=..., symbol=<name>) for signatures+callers, ",
        "symbol_source_tool for one definition, query_graph(file_summary) for the node ",
        "list, or Read with offset/limit for the slice you will edit."
    ));
    let rows: Vec<Value> = fs::read_to_string(project.0.join("telemetry.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(rows.iter().any(|row| row["tool"] == "pre-read-skeleton"
        && row["session_id"] == "read-session"
        && row["output_bytes"].as_u64().unwrap_or(0) > 0));
    assert!(rows.iter().any(|row| row["delegated"] == false));
}

#[test]
fn newline_boundary_and_binary_bytes_match_the_guard_contract() {
    let project = Project::new();
    let path = project.file("boundary.rs", format!("{}tail", "x\n".repeat(399)));
    assert!(reason(&pre_read::hook_output(&read(&path)).unwrap()).is_empty());
    let mut bytes = vec![0xff; 70_000];
    bytes.extend_from_slice(&vec![b'\n'; 400]);
    fs::write(&path, bytes).unwrap();
    let output = pre_read::hook_output(&read(&path)).unwrap();
    assert!(reason(&output).contains("400 lines (~17,600 tokens)"));
    assert!(reason(&output).contains("file_path=..., no symbol)"));
}

#[test]
fn file_kinds_and_truthy_slices_remain_quiet() {
    let project = Project::new();
    for name in ["document.md", "upper.PY", "dot.py.", "no_extension"] {
        let path = project.file(name, "x\n".repeat(500));
        assert!(
            reason(&pre_read::hook_output(&read(&path)).unwrap()).is_empty(),
            "{name}"
        );
    }
    for path in [project.0.join("missing.rs"), project.0.clone()] {
        assert!(reason(&pre_read::hook_output(&read(&path)).unwrap()).is_empty());
    }
    let path = project.file("large.rs", "x\n".repeat(400));
    for key in ["offset", "limit"] {
        for value in [json!(1), json!(-1), json!(true), json!("0"), json!([0])] {
            let mut payload = read(&path);
            payload["tool_input"][key] = value;
            assert!(reason(&pre_read::hook_output(&payload).unwrap()).is_empty());
        }
        for value in [
            json!(0),
            json!(false),
            Value::Null,
            json!(""),
            json!([]),
            json!({}),
        ] {
            let mut payload = read(&path);
            payload["tool_input"][key] = value;
            assert!(!reason(&pre_read::hook_output(&payload).unwrap()).is_empty());
        }
    }
}

#[test]
fn every_supported_code_suffix_is_guarded() {
    let project = Project::new();
    for suffix in ["py", "rs", "c", "cc", "cpp", "h", "hpp", "ts", "js", "sh"] {
        let path = project.file(&format!("large.{suffix}"), "x\n".repeat(400));
        assert_eq!(
            pre_read::hook_output(&read(&path)).unwrap()["hookSpecificOutput"]
                ["permissionDecision"],
            "deny"
        );
    }
}

#[test]
fn aliases_relative_paths_and_empty_primary_input_are_native() {
    let project = Project::new();
    project.file("large.rs", "x\n".repeat(400));
    for standalone in [false, true] {
        for primary in [
            Value::Null,
            json!(""),
            json!(false),
            json!(0),
            json!([]),
            json!({}),
        ] {
            let payload = json!({"tool_name": primary, "toolName": "Read", "tool_input": {},
                "toolInput": {"file_path": "", "path": "large.rs"}});
            assert_eq!(
                project.native(&payload, standalone)["hookSpecificOutput"]["permissionDecision"],
                "deny"
            );
        }
    }
}

#[test]
fn protected_current_work_reads_still_deny_in_both_modes() {
    let project = Project::new();
    let path = project.0.join(".current_work.md");
    for standalone in [false, true] {
        let output = project.native(&read(&path), standalone);
        assert_eq!(output["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(reason(&output).contains("cannot access .current_work.md directly"));
    }
}

#[test]
fn read_merges_refresh_failure_and_consumes_it_once() {
    let project = Project::new();
    let path = project.file("large.rs", "x\n".repeat(400));
    project.stage_failure();
    let first = project.native(&read(&path), false);
    assert_eq!(first["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(first["systemMessage"].as_str().unwrap().contains("offline"));
    assert!(first["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("STALE"));
    let second = project.native(&read(&path), false);
    assert!(second.get("systemMessage").is_none());
}

#[test]
fn generic_tools_run_the_refresh_report_without_starting_a_dispatcher() {
    let project = Project::new();
    for name in ["Grep", "Glob", "WebFetch", "Task", "mcp__other__lookup"] {
        project.stage_failure();
        let output = project.native(&json!({"tool_name": name}), false);
        assert!(
            output["systemMessage"]
                .as_str()
                .unwrap()
                .contains("offline"),
            "{name}"
        );
    }
}

#[test]
fn edit_graph_and_malformed_payloads_keep_delegating() {
    for name in [
        "Edit",
        "Write",
        "NotebookEdit",
        "mcp__code-review-graph__query",
        "mcp__code-review_graph__query",
        "mcp__code_review-graph__query",
        "mcp__code_review_graph__query",
    ] {
        let project = Project::new();
        let raw = json!({"tool_name": name}).to_string();
        let output = project.run(&raw, None, false);
        assert_eq!(output.status.code(), Some(17), "{name}");
        assert_eq!(
            fs::read_to_string(project.0.join("delegated.input")).unwrap(),
            raw
        );
    }
    for raw in ["not json", "[]", "null", "{\"tool_name\":17}"] {
        let project = Project::new();
        assert_eq!(project.run(raw, None, false).status.code(), Some(17));
    }
}

#[test]
fn empty_opt_out_and_incomplete_read_selection_preserve_delegation_and_splices() {
    for selected in [
        "",
        "crg_refresh_report_pre",
        "current_work_guard_read,pre_read_skeleton",
    ] {
        let project = Project::new();
        let path = project.file("large.rs", "x\n".repeat(400));
        let raw = read(&path).to_string();
        let output = project.run(&raw, Some(selected), false);
        assert_eq!(output.status.code(), Some(17), "{selected}");
        assert_eq!(
            fs::read_to_string(project.0.join("delegated.input")).unwrap(),
            raw
        );
        let names = fs::read_to_string(project.0.join("delegated.names")).unwrap();
        let answers = fs::read_to_string(project.0.join("delegated.answers")).unwrap();
        if selected.is_empty() {
            assert!(names.is_empty() && answers.is_empty());
        } else {
            let answers: Value = serde_json::from_str(&answers).unwrap();
            let expected: Vec<_> = selected.split(',').collect();
            assert_eq!(answers.as_object().unwrap().len(), expected.len());
            for name in expected {
                assert!(names.split(',').any(|actual| actual == name));
                assert!(answers.get(name).is_some());
            }
            if selected.contains("pre_read_skeleton") {
                assert_eq!(
                    answers["pre_read_skeleton"]["hookSpecificOutput"]["permissionDecision"],
                    "deny"
                );
            }
        }
    }
}

#[test]
fn generic_opt_out_preserves_the_refresh_notice_for_the_dispatcher() {
    let project = Project::new();
    project.stage_failure();
    for names in ["", "pre_read_skeleton"] {
        assert_eq!(
            project
                .run("{\"tool_name\":\"Grep\"}", Some(names), false)
                .status
                .code(),
            Some(17)
        );
        assert!(project.0.join(".code-review-graph/refresh.failed").exists());
        assert!(fs::read_to_string(project.0.join("delegated.names"))
            .unwrap()
            .is_empty());
    }
}

#[test]
fn malformed_adapter_input_surfaces_a_hook_error() {
    let project = Project::new();
    let output = project.native(&json!({"tool_name": "Read", "tool_input": ["bad"]}), false);
    assert!(output["systemMessage"]
        .as_str()
        .unwrap()
        .contains("HOOK ERROR [pre_read_skeleton]"));
    assert!(pre_read::hook_output(&json!({"tool_input": {"file_path": 123}})).is_err());
    assert!(pre_read::hook_output(&Value::Null).is_ok());
}

#[test]
#[ignore = "bounded native-only CLI latency probe; run explicitly with --ignored --nocapture"]
fn native_pretool_cli_latency_probe() {
    let project = Project::new();
    let path = project.file("large.rs", "x\n".repeat(400));
    for (label, payload) in [
        ("Grep", json!({"tool_name": "Grep"})),
        ("Read deny", read(&path)),
    ] {
        let mut samples = Vec::new();
        for _ in 0..40 {
            let start = Instant::now();
            project.native(&payload, false);
            samples.push(start.elapsed().as_secs_f64() * 1_000.0);
        }
        samples.sort_by(f64::total_cmp);
        eprintln!(
            "{label}: 40 native CLI calls, median {:.3} ms, p95 {:.3} ms",
            samples[20], samples[38]
        );
    }
}
