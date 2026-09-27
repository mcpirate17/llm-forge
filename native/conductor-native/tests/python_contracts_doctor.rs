#![cfg(feature = "python-compat-tests")]
//! Rust-owned contracts for the shipped conductor doctor Python API.

#[path = "python_contracts/agent_comm_support.rs"]
#[allow(dead_code)]
mod comm_support;
#[path = "python_contracts/doctor_fixture.rs"]
mod doctor_fixture;
#[path = "python_contracts/support.rs"]
#[allow(dead_code)]
mod support;

use comm_support::{buffer_text, capture};
use doctor_fixture::{call_main, canonical_hooks, healthy, DoctorCase};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde_json::{json, Value};

fn run_with(
    case: &DoctorCase,
    py: Python<'_>,
    project: &Value,
    user: Option<&Value>,
    flags: &[&str],
) -> i32 {
    case.write_project(project);
    if let Some(user) = user {
        case.write_user(user);
    }
    case.run(py, flags)
}

fn set_hook_command(payload: &mut Value, command: &str) {
    payload["hooks"]["PostToolUse"][0]["hooks"][0]["command"] = json!(command);
}

fn assert_command_passes(command: &str) {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let mut payload = healthy(py);
        set_hook_command(&mut payload, command);
        assert_eq!(run_with(&case, py, &payload, None, &[]), 0);
    });
}

fn assert_bad_limit_fails(value: &str) {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let mut payload = healthy(py);
        payload["env"] = json!({"BASH_QUIET_LIMIT_BYTES": value});
        assert_eq!(run_with(&case, py, &payload, Some(&healthy(py)), &[]), 1);
    });
}

fn assert_model_passes(model: &str) {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let mut payload = healthy(py);
        payload["model"] = json!(model);
        assert_eq!(run_with(&case, py, &payload, Some(&healthy(py)), &[]), 0);
    });
}

fn assert_invalid_schema(serialized: &str) {
    let case = DoctorCase::new();
    case.write_raw_project(serialized);
    Python::attach(|py| {
        let error = case.diagnose(py).unwrap_err();
        assert!(error.is_instance_of::<PyValueError>(py), "{error}");
        assert!(error.to_string().contains("JSON object"), "{error}");
        assert_eq!(case.run(py, &[]), 2);
    });
}

#[test]
fn canonical_settings_pass_all_checks() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let (stdout, _stdout_patch) = capture(py, "stdout");
        let payload = healthy(py);
        assert_eq!(run_with(&case, py, &payload, Some(&payload), &[]), 0);
        let out = buffer_text(&stdout);
        assert!(out.contains("harness-doctor | PASS fails=0"), "{out}");
        for check in [
            "subagentPromptCacheTtl",
            "hooks-dispatcher",
            "BASH_QUIET_LIMIT_BYTES",
            "model",
        ] {
            assert!(out.contains(&format!("| {check} | PASS")), "{out}");
        }
    });
}

#[test]
fn short_prompt_cache_ttl_fails_and_fix_sets_one_hour() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let mut payload = healthy(py);
        payload["subagentPromptCacheTtl"] = json!("5m");
        assert_eq!(run_with(&case, py, &payload, Some(&healthy(py)), &[]), 1);
        assert_eq!(case.run(py, &["--fix"]), 0);
        assert_eq!(case.read_project()["subagentPromptCacheTtl"], "1h");
    });
}

#[test]
fn missing_project_ttl_inherits_user_value() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let mut payload = healthy(py);
        payload
            .as_object_mut()
            .unwrap()
            .remove("subagentPromptCacheTtl");
        assert_eq!(run_with(&case, py, &payload, Some(&healthy(py)), &[]), 0);
    });
}

#[test]
fn missing_effective_ttl_still_fails() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let mut payload = healthy(py);
        payload
            .as_object_mut()
            .unwrap()
            .remove("subagentPromptCacheTtl");
        assert_eq!(run_with(&case, py, &payload, None, &[]), 1);
    });
}

#[test]
fn stray_hook_command_fails_and_fix_canonicalizes() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let mut payload = healthy(py);
        payload["hooks"]["PreToolUse"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "type": "command",
                "command": "python3 .claude/hooks/my_hook.py",
                "timeout": 5
            }));
        assert_eq!(run_with(&case, py, &payload, Some(&healthy(py)), &[]), 1);
        assert_eq!(case.run(py, &["--fix"]), 0);
        assert_eq!(case.read_project()["hooks"], canonical_hooks(py));
    });
}

#[test]
fn project_event_can_be_supplied_by_user_settings() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let mut payload = healthy(py);
        payload["hooks"]
            .as_object_mut()
            .unwrap()
            .remove("SessionEnd");
        assert_eq!(run_with(&case, py, &payload, Some(&healthy(py)), &[]), 0);
    });
}

#[test]
fn future_forge_hook_command_is_accepted() {
    assert_command_passes("forge hook PostToolUse");
}

#[test]
fn absolute_forge_hook_command_passes() {
    assert_command_passes("/opt/bin/forge hook PostToolUse");
}

#[test]
fn env_quoted_forge_hook_command_passes() {
    assert_command_passes(
        "env 'CONDUCTOR_PYTHON=/venv with spaces/bin/python' '/opt/forge directory/forge' hook PostToolUse",
    );
}

#[test]
fn env_assignment_forge_hook_command_passes() {
    assert_command_passes("FORGE_MODE=enforce /opt/bin/forge hook PostToolUse");
}

#[test]
fn project_values_override_invalid_user_values_without_rewriting_user() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let user = json!({
            "subagentPromptCacheTtl": "5m",
            "env": {"BASH_QUIET_LIMIT_BYTES": "bad"},
            "model": "glm-5"
        });
        let mut payload = healthy(py);
        payload["model"] = json!("sonnet");
        assert_eq!(run_with(&case, py, &payload, Some(&user), &[]), 0);
        let before = case.user_bytes();
        assert_eq!(case.run(py, &["--fix"]), 0);
        assert_eq!(case.user_bytes(), before);
    });
}

#[test]
fn unset_project_limit_inherits_user_and_fix_preserves_empty_env() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let mut payload = healthy(py);
        payload["env"] = json!({});
        assert_eq!(run_with(&case, py, &payload, Some(&healthy(py)), &[]), 0);
        assert_eq!(case.run(py, &["--fix"]), 0);
        assert_eq!(case.read_project()["env"], json!({}));
    });
}

#[test]
fn non_positive_limit_zero_word_fails() {
    assert_bad_limit_fails("zero");
}

#[test]
fn non_positive_limit_zero_digit_fails() {
    assert_bad_limit_fails("0");
}

#[test]
fn non_positive_limit_negative_fails() {
    assert_bad_limit_fails("-1");
}

#[test]
fn non_positive_limit_empty_fails() {
    assert_bad_limit_fails("");
}

#[test]
fn foreign_model_id_fails_and_fix_removes_key() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let mut payload = healthy(py);
        payload["model"] = json!("glm-5.3-flash");
        assert_eq!(run_with(&case, py, &payload, Some(&healthy(py)), &[]), 1);
        assert_eq!(case.run(py, &["--fix"]), 0);
        assert!(case.read_project().get("model").is_none());
    });
}

#[test]
fn known_model_claude_opus_passes() {
    assert_model_passes("claude-opus-5");
}

#[test]
fn known_model_sonnet_passes() {
    assert_model_passes("sonnet");
}

#[test]
fn known_model_claude_fable_passes() {
    assert_model_passes("claude-fable-5-1");
}

#[test]
fn non_string_model_in_user_settings_fails() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        assert_eq!(
            run_with(&case, py, &healthy(py), Some(&json!({"model": 7})), &[]),
            1
        );
    });
}

#[test]
fn model_with_thinking_budget_suffix_passes() {
    assert_model_passes("claude-fable-5-1[1m]");
}

#[test]
fn user_settings_without_dispatcher_hooks_pass() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let mut user = healthy(py);
        user["hooks"] = json!({});
        user["model"] = json!("claude-fable-5-1[1m]");
        assert_eq!(run_with(&case, py, &healthy(py), Some(&user), &[]), 0);
    });
}

#[test]
fn stray_user_hook_command_fails() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let stray = json!({"hooks": {"PreToolUse": [{
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": "bash mine.sh"}]
        }]}});
        assert_eq!(run_with(&case, py, &healthy(py), Some(&stray), &[]), 1);
    });
}

#[test]
fn missing_user_settings_reports_skip_without_failure() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let (stdout, _patch) = capture(py, "stdout");
        assert_eq!(run_with(&case, py, &healthy(py), None, &[]), 0);
        assert!(buffer_text(&stdout).contains("user | (file) | SKIP"));
    });
}

#[test]
fn missing_project_settings_fails_then_fix_writes_canonical_block() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        case.write_user(&healthy(py));
        assert_eq!(case.run(py, &[]), 1);
        assert_eq!(case.run(py, &["--fix"]), 0);
        let created = case.read_project();
        assert_eq!(created["hooks"], canonical_hooks(py));
        assert_eq!(created["subagentPromptCacheTtl"], "1h");
        assert_eq!(created["env"]["BASH_QUIET_LIMIT_BYTES"], "8000");
    });
}

#[test]
fn malformed_settings_json_fails_loud_with_exit_two() {
    let case = DoctorCase::new();
    case.write_raw_project("{ not json");
    Python::attach(|py| {
        let (stderr, _patch) = capture(py, "stderr");
        assert_eq!(case.run(py, &[]), 2);
        assert!(buffer_text(&stderr).contains("not readable JSON"));
    });
}

#[test]
fn schema_array_is_value_error_and_cli_exit_two() {
    assert_invalid_schema("[]");
}

#[test]
fn schema_null_is_value_error_and_cli_exit_two() {
    assert_invalid_schema("null");
}

#[test]
fn schema_env_array_is_value_error_and_cli_exit_two() {
    assert_invalid_schema("{\"env\": []}");
}

#[test]
fn fix_is_idempotent_a_second_run_changes_nothing() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let mut payload = healthy(py);
        payload["model"] = json!("glm-5.3-flash");
        payload["subagentPromptCacheTtl"] = json!("5m");
        case.write_project(&payload);
        case.write_user(&json!({"model": "glm-4.9"}));
        assert_eq!(case.run(py, &["--fix"]), 0);
        let project_before = case.project_bytes();
        let user_before = case.user_bytes();
        assert_eq!(case.run(py, &["--fix"]), 0);
        assert_eq!(case.project_bytes(), project_before);
        assert_eq!(case.user_bytes(), user_before);
    });
}

#[test]
fn roots_come_from_environment_when_flags_absent() {
    let mut case = DoctorCase::new();
    let project = case.project().display().to_string();
    let home = case.home().display().to_string();
    case.set_env("CLAUDE_PROJECT_DIR", &project);
    case.set_env("HOME", &home);
    Python::attach(|py| {
        let payload = healthy(py);
        case.write_project(&payload);
        case.write_user(&payload);
        assert_eq!(call_main(py, &["--harness".to_owned()]), 0);
    });
}

#[test]
fn mode_is_required() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let args = vec![
            "--project-dir".to_owned(),
            case.project().display().to_string(),
            "--home".to_owned(),
            case.home().display().to_string(),
        ];
        assert_eq!(call_main(py, &args), 2);
    });
}

#[test]
fn json_mode_reports_post_fix_findings_and_fix_count() {
    let case = DoctorCase::new();
    Python::attach(|py| {
        let mut payload = healthy(py);
        payload["model"] = json!("glm-5.3-flash");
        case.write_project(&payload);
        case.write_user(&healthy(py));
        let (stdout, _patch) = capture(py, "stdout");
        assert_eq!(case.run(py, &["--fix", "--json"]), 0);
        let report: Value = serde_json::from_str(&buffer_text(&stdout)).unwrap();
        assert_eq!(report["fixed"], 1);
        let findings = report["findings"].as_array().unwrap();
        assert!(findings
            .iter()
            .all(|finding| { matches!(finding["status"].as_str(), Some("PASS" | "SKIP")) }));
    });
}
