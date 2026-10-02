//! Thin Python binding onto the shared hook algebra and context projection.

use crate::context_projection::{compose_hook, Fragment};
use crate::hook_response::{merge, HookOutcome};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

#[pyfunction]
fn hook_merge_native(event: &str, outcomes_json: &str) -> PyResult<String> {
    let outcomes: Vec<HookOutcome> = serde_json::from_str(outcomes_json)
        .map_err(|error| PyValueError::new_err(format!("invalid hook outcomes: {error}")))?;
    serde_json::to_string(&merge(event, &outcomes))
        .map_err(|error| PyValueError::new_err(format!("hook merge serialization failed: {error}")))
}

#[pyfunction]
fn hook_context_project_native(fragments_json: &str) -> PyResult<String> {
    let fragments: Vec<Fragment> = serde_json::from_str(fragments_json)
        .map_err(|error| PyValueError::new_err(format!("invalid context fragments: {error}")))?;
    Ok(compose_hook(&fragments))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(hook_merge_native, module)?)?;
    module.add_function(wrap_pyfunction!(hook_context_project_native, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::hook_merge_native;
    use serde_json::Value;

    #[test]
    fn pretool_fail_closed_error_beats_allow_and_preserves_context() {
        let result: Value = serde_json::from_str(
            &hook_merge_native(
                "PreToolUse",
                r#"[
                  {"name":"allow","output":{"hookSpecificOutput":{"permissionDecision":"allow","additionalContext":"kept"}},"error":null,"fail_closed":false},
                  {"name":"gate","output":null,"error":"boom","fail_closed":true}
                ]"#,
            )
            .expect("merge should succeed"),
        )
        .expect("result must be JSON");
        let specific = &result["hookSpecificOutput"];
        assert_eq!(specific["permissionDecision"], "deny");
        assert_eq!(specific["additionalContext"], "kept");
        assert!(result["systemMessage"]
            .as_str()
            .unwrap()
            .contains("HOOK ERROR [gate]"));
    }

    #[test]
    fn later_rewrite_wins_and_reports_conflict() {
        let result: Value = serde_json::from_str(
            &hook_merge_native(
                "PostToolUse",
                r#"[
                  {"name":"first","output":{"hookSpecificOutput":{"updatedToolOutput":{"value":1}}},"error":null,"fail_closed":false},
                  {"name":"second","output":{"hookSpecificOutput":{"updatedToolOutput":{"value":2}},"continue":false,"stopReason":"done"},"error":null,"fail_closed":false}
                ]"#,
            )
            .expect("merge should succeed"),
        )
        .expect("result must be JSON");
        assert_eq!(
            result["hookSpecificOutput"]["updatedToolOutput"]["value"],
            2
        );
        assert_eq!(result["continue"], false);
        assert_eq!(result["stopReason"], "done");
        assert!(result["systemMessage"]
            .as_str()
            .unwrap()
            .contains("HOOK CONFLICT [second]"));
    }

    /// Claude Code's schema has no `hookSpecificOutput` for SessionEnd
    /// (2.1.268 logged a validation error on every session end), and none
    /// for SubagentStop/Stop: those events fold to the top-level fields
    /// only, a quiet session end to `{}`. The three schema events keep the
    /// wrapper with `hookEventName` exactly as before.
    #[test]
    fn session_end_folds_to_top_level_fields_only() {
        let quiet: Value = serde_json::from_str(
            &hook_merge_native(
                "SessionEnd",
                r#"[{"name":"telemetry","output":null,"error":null,"fail_closed":false}]"#,
            )
            .expect("merge should succeed"),
        )
        .expect("result must be JSON");
        assert_eq!(quiet, serde_json::json!({}));

        let errored: Value = serde_json::from_str(
            &hook_merge_native(
                "SessionEnd",
                r#"[{"name":"ledger","output":null,"error":"rollup failed","fail_closed":false}]"#,
            )
            .expect("merge should succeed"),
        )
        .expect("result must be JSON");
        assert!(errored.get("hookSpecificOutput").is_none());
        assert!(errored["systemMessage"]
            .as_str()
            .unwrap()
            .contains("rollup failed"));

        for event in ["SubagentStop", "Stop"] {
            let result: Value = serde_json::from_str(
                &hook_merge_native(
                    event,
                    r#"[{"name":"gate","output":{"hookSpecificOutput":{"permissionDecision":"deny"}},"error":null,"fail_closed":false}]"#,
                )
                .expect("merge should succeed"),
            )
            .expect("result must be JSON");
            assert_eq!(
                result,
                serde_json::json!({}),
                "{event} has no schema for it"
            );
        }

        for event in ["PreToolUse", "PostToolUse", "SessionStart"] {
            let result: Value = serde_json::from_str(
                &hook_merge_native(
                    event,
                    r#"[{"name":"quiet","output":null,"error":null,"fail_closed":false}]"#,
                )
                .expect("merge should succeed"),
            )
            .expect("result must be JSON");
            assert_eq!(
                result["hookSpecificOutput"]["hookEventName"], event,
                "schema events keep the wrapper exactly as before"
            );
        }
    }
}
