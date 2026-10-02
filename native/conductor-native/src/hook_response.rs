//! Shared hook decision algebra. Context budgets cannot weaken a permission vote.

use crate::context_projection::{compose_hook, Fragment};
use serde::Deserialize;
use serde_json::{json, Map, Value};

const SEP: &str = "\n\n";
const REWRITE: [&str; 2] = ["updatedToolOutput", "updatedMCPToolOutput"];
const RESERVED: [&str; 7] = [
    "hookEventName",
    "permissionDecision",
    "permissionDecisionReason",
    "additionalContext",
    "contextFragments",
    "updatedToolOutput",
    "updatedMCPToolOutput",
];

#[derive(Deserialize)]
pub struct HookOutcome {
    pub name: String,
    #[serde(default)]
    pub output: Value,
    pub error: Option<String>,
    pub fail_closed: bool,
}

fn rank(decision: &str) -> u8 {
    match decision {
        "allow" => 1,
        "ask" => 2,
        "deny" => 3,
        _ => 0,
    }
}

fn text(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

#[derive(Default)]
struct Response {
    votes: Vec<(String, String)>,
    contexts: Vec<Fragment>,
    system: Vec<String>,
    blocks: Vec<String>,
    stops: Vec<String>,
    rewrites: Map<String, Value>,
    extras: Map<String, Value>,
    stop: bool,
    suppress: bool,
}

impl Response {
    fn add_context(&mut self, outcome: &HookOutcome, specific: &Map<String, Value>) {
        let protect = matches!(
            specific.get("permissionDecision").and_then(Value::as_str),
            Some("deny" | "ask")
        ) || outcome.output.get("continue") == Some(&Value::Bool(false))
            || matches!(
                outcome.output.get("decision").and_then(Value::as_str),
                Some("deny" | "block")
            );
        if let Some(content) = specific
            .get("additionalContext")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            let mut fragment = Fragment::hook(&outcome.name, content.to_owned(), false);
            fragment.protected |= protect;
            self.contexts.push(fragment);
        }
        if let Some(fragments) = specific.get("contextFragments").and_then(Value::as_array) {
            for value in fragments {
                match serde_json::from_value::<Fragment>(value.clone()) {
                    Ok(mut fragment) => {
                        fragment.protected |= protect
                            || matches!(
                                fragment.category.as_str(),
                                "instructions" | "policy" | "blocker" | "approval"
                            );
                        self.contexts.push(fragment);
                    }
                    Err(_) => self.system.push(format!(
                        "HOOK ERROR [{}]: invalid context fragment",
                        outcome.name
                    )),
                }
            }
        }
    }

    fn add(&mut self, event: &str, outcome: &HookOutcome) {
        if let Some(error) = &outcome.error {
            let line = format!("HOOK ERROR [{}]: {error}", outcome.name);
            self.system.push(line.clone());
            if event == "PreToolUse" && outcome.fail_closed {
                self.votes.push(("deny".into(), line));
            } else {
                self.contexts
                    .push(Fragment::hook(&outcome.name, line, true));
            }
        }
        let Some(output) = outcome.output.as_object() else {
            return;
        };
        let empty = Map::new();
        let specific = output
            .get("hookSpecificOutput")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        if let Some(decision) = specific
            .get("permissionDecision")
            .and_then(Value::as_str)
            .filter(|d| rank(d) > 0)
        {
            self.votes.push((
                decision.into(),
                text(specific.get("permissionDecisionReason")),
            ));
        }
        self.add_context(outcome, specific);
        for key in REWRITE {
            if let Some(value) = specific.get(key) {
                if self.rewrites.contains_key(key) {
                    self.system.push(format!("HOOK CONFLICT [{}]: {key} already set by an earlier hook; the later value wins", outcome.name));
                }
                self.rewrites.insert(key.into(), value.clone());
            }
        }
        for (key, value) in specific {
            if !RESERVED.contains(&key.as_str()) {
                self.extras.insert(key.clone(), value.clone());
            }
        }
        if matches!(
            output.get("decision").and_then(Value::as_str),
            Some("block" | "deny")
        ) {
            let reason = text(output.get("reason"));
            if event == "PreToolUse" {
                self.votes.push(("deny".into(), reason));
            } else {
                self.blocks.push(reason);
            }
        }
        if output.get("continue") == Some(&Value::Bool(false)) {
            self.stop = true;
            if let Some(reason) = output
                .get("stopReason")
                .and_then(Value::as_str)
                .filter(|r| !r.is_empty())
            {
                self.stops.push(reason.into());
            }
        }
        self.suppress |= output.get("suppressOutput") == Some(&Value::Bool(true));
        if let Some(message) = output
            .get("systemMessage")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
        {
            self.system.push(message.into());
        }
    }

    fn finish(self, event: &str) -> Value {
        let mut specific = Map::new();
        specific.insert("hookEventName".into(), json!(event));
        if let Some((winner, _)) = self.votes.iter().max_by_key(|(decision, _)| rank(decision)) {
            specific.insert("permissionDecision".into(), json!(winner));
            let reasons: Vec<_> = self
                .votes
                .iter()
                .filter(|(decision, reason)| decision == winner && !reason.is_empty())
                .map(|(_, reason)| reason.as_str())
                .collect();
            if !reasons.is_empty() {
                specific.insert("permissionDecisionReason".into(), json!(reasons.join(SEP)));
            }
        }
        if !self.contexts.is_empty() {
            specific.insert(
                "additionalContext".into(),
                json!(compose_hook(&self.contexts)),
            );
        }
        specific.extend(self.rewrites);
        specific.extend(self.extras);
        let mut result = Map::new();
        if matches!(event, "PreToolUse" | "PostToolUse" | "SessionStart") {
            result.insert("hookSpecificOutput".into(), Value::Object(specific));
        }
        if !self.blocks.is_empty() {
            result.insert("decision".into(), json!("block"));
            result.insert(
                "reason".into(),
                json!(self
                    .blocks
                    .into_iter()
                    .filter(|r| !r.is_empty())
                    .collect::<Vec<_>>()
                    .join(SEP)),
            );
        }
        if self.stop {
            result.insert("continue".into(), json!(false));
            if !self.stops.is_empty() {
                result.insert("stopReason".into(), json!(self.stops.join(SEP)));
            }
        }
        if self.suppress {
            result.insert("suppressOutput".into(), json!(true));
        }
        if !self.system.is_empty() {
            result.insert("systemMessage".into(), json!(self.system.join(SEP)));
        }
        Value::Object(result)
    }
}

pub fn merge(event: &str, outcomes: &[HookOutcome]) -> Value {
    let mut response = Response::default();
    for outcome in outcomes {
        response.add(event, outcome);
    }
    response.finish(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(name: &str, output: Value) -> HookOutcome {
        HookOutcome {
            name: name.into(),
            output,
            error: None,
            fail_closed: false,
        }
    }

    #[test]
    fn budget_cannot_remove_permission_or_required_fragments() {
        let mandatory = "mandatory approval details".repeat(1000);
        let result = merge(
            "PreToolUse",
            &[
                outcome(
                    "approval",
                    json!({"hookSpecificOutput": {
                        "permissionDecision": "ask", "permissionDecisionReason": "approval required",
                        "contextFragments": [{"content": mandatory}]
                    }}),
                ),
                outcome(
                    "allow",
                    json!({"hookSpecificOutput": {"permissionDecision": "allow"}}),
                ),
            ],
        );
        assert_eq!(result["hookSpecificOutput"]["permissionDecision"], "ask");
        assert!(result["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains(&mandatory));
        assert!(result["hookSpecificOutput"]
            .get("contextFragments")
            .is_none());
    }

    #[test]
    fn invalid_fragment_is_visible_and_top_level_block_is_protected() {
        let reason = "required block context".repeat(1000);
        let result = merge(
            "PostToolUse",
            &[outcome(
                "blocker",
                json!({
                    "decision": "block", "reason": "stop", "hookSpecificOutput": {
                        "additionalContext": reason, "contextFragments": [{"priority": "bad"}]
                    }
                }),
            )],
        );
        assert_eq!(result["decision"], "block");
        assert_eq!(
            result["systemMessage"],
            "HOOK ERROR [blocker]: invalid context fragment"
        );
        assert!(result["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .contains(&reason));
    }
}
