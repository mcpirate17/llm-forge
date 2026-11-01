//! Final wire projection for the repository's Claude/Codex hook adapter contract.
//! Decisions are made by native handlers before projection; this cannot weaken a deny.
use clap::ValueEnum;
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Protocol {
    #[default]
    Claude,
    Codex,
}

pub fn project(protocol: Protocol, event: &str, answer: &Value) -> Value {
    if protocol != Protocol::Codex || !matches!(event, "PreToolUse" | "PostToolUse") {
        return answer.clone();
    }
    let mut specific = answer
        .get("hookSpecificOutput")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let allowed: &[&str] = if event == "PreToolUse" {
        &[
            "hookEventName",
            "permissionDecision",
            "permissionDecisionReason",
            "additionalContext",
            "updatedInput",
        ]
    } else {
        &["hookEventName", "additionalContext"]
    };
    specific.retain(|key, _| allowed.contains(&key.as_str()));
    if event == "PreToolUse" {
        match specific.get("permissionDecision").and_then(Value::as_str) {
            Some("allow") if !specific.contains_key("updatedInput") => {
                specific.remove("permissionDecision");
                specific.remove("permissionDecisionReason");
            }
            Some("ask") => {
                specific.insert("permissionDecision".into(), Value::String("deny".into()));
                specific.entry("permissionDecisionReason").or_insert_with(|| Value::String(
                    "Hook requested approval, but Codex PreToolUse hooks do not support ask.".into()));
            }
            _ => (),
        }
    }
    let top: &[&str] = if event == "PreToolUse" {
        &["systemMessage"]
    } else {
        &[
            "systemMessage",
            "decision",
            "reason",
            "continue",
            "stopReason",
        ]
    };
    let mut out = Map::new();
    for key in top {
        if let Some(value) = answer.get(*key) {
            out.insert((*key).into(), value.clone());
        }
    }
    if specific.keys().any(|key| key != "hookEventName") {
        specific.insert("hookEventName".into(), Value::String(event.into()));
        out.insert("hookSpecificOutput".into(), Value::Object(specific));
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codex_projection_preserves_denials_and_cannot_approve_an_ask() {
        for decision in ["deny", "ask"] {
            let answer = json!({"hookSpecificOutput":{"hookEventName":"wrong", "permissionDecision":decision,
                "permissionDecisionReason":"kept", "additionalContext":"context", "junk":1},"junk":1});
            let out = project(Protocol::Codex, "PreToolUse", &answer);
            assert_eq!(
                out,
                json!({"hookSpecificOutput":{"hookEventName":"PreToolUse",
                "permissionDecision":"deny","permissionDecisionReason":"kept","additionalContext":"context"}})
            );
        }
        let out = project(
            Protocol::Codex,
            "PreToolUse",
            &json!({"hookSpecificOutput":{"permissionDecision":"ask"}}),
        );
        assert!(out["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("do not support ask"));
    }

    #[test]
    fn plain_allows_are_quiet_but_input_rewrites_remain_explicit() {
        let mut answer = json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow", "permissionDecisionReason":"fine"}});
        assert_eq!(project(Protocol::Codex, "PreToolUse", &answer), json!({}));
        answer["hookSpecificOutput"]["updatedInput"] = json!({"model":"fixture"});
        let output = project(Protocol::Codex, "PreToolUse", &answer);
        assert_eq!(output["hookSpecificOutput"]["permissionDecision"], "allow");
        assert_eq!(
            output["hookSpecificOutput"]["updatedInput"],
            json!({"model":"fixture"})
        );
    }

    #[test]
    fn post_projection_keeps_stop_context_but_excludes_unsupported_output_rewrites() {
        let answer = json!({"decision":"block","continue":false,"stopReason":"stop","reason":"because","systemMessage":"system",
            "hookSpecificOutput":{"hookEventName":"PostToolUse","additionalContext":"bounded context","updatedMCPToolOutput":{"data":"raw"},"updatedInput":{}}});
        let output = project(Protocol::Codex, "PostToolUse", &answer);
        assert_eq!(
            output["hookSpecificOutput"],
            json!({"hookEventName":"PostToolUse","additionalContext":"bounded context"})
        );
        assert_eq!(output["continue"], false);
        assert_eq!(output["decision"], "block");
        assert_eq!(project(Protocol::Claude, "PostToolUse", &answer), answer);
        assert_eq!(project(Protocol::Codex, "SessionStart", &answer), answer);
    }
}
