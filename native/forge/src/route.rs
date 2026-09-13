//! `forge route`: the routing-policy decision for one `Agent` dispatch
//! (`docs/design/cost_ledger.md` section 5, `docs/roadmap.md` Phase 3 step
//! 2). Pure function over `ledger/routing_policy.toml` (embedded at build
//! time via `include_str!`, overridable with `--policy PATH` for tests) plus
//! the dispatch's own `subagent_type`/`model`/`description` -- no I/O, no
//! clock, so `dispatch.rs`'s `PreToolUse` seam can call it inline.

use anyhow::{bail, Context, Result};
use clap::Args;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;

/// The policy shipped in this repo. `--policy PATH` (CLI) or
/// `Policy::load_from` (library callers, e.g. `dispatch.rs` tests) can
/// override it; production `dispatch.rs` always uses this default so the
/// hook never depends on a file existing on disk at call time.
const DEFAULT_POLICY_TOML: &str = include_str!("../../../ledger/routing_policy.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct ClassRule {
    pub name: String,
    #[serde(default)]
    pub subagent_types: Vec<String>,
    #[serde(default)]
    pub description_prefixes: Vec<String>,
    #[serde(default)]
    pub requested_models: Vec<String>,
    pub tier: String,
    pub cap_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct DenyConfig {
    default_class: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Policy {
    pub policy_version: String,
    pub tier_order: Vec<String>,
    pub justify_marker: String,
    #[serde(rename = "class")]
    pub classes: Vec<ClassRule>,
    deny: DenyConfig,
}

impl Policy {
    /// Parses `text` as the routing policy. Fails loud on anything
    /// malformed or structurally incomplete (missing `default_class`
    /// referring to a real class, empty `tier_order`) -- callers map this to
    /// exit code 2, never a silent fallback to some built-in default.
    pub fn parse(text: &str) -> Result<Self> {
        let policy: Policy =
            toml::from_str(text).context("routing_policy.toml: invalid TOML or wrong shape")?;
        if policy.tier_order.is_empty() {
            bail!("routing_policy.toml: tier_order must not be empty");
        }
        if policy.classes.is_empty() {
            bail!("routing_policy.toml: at least one [[class]] is required");
        }
        if !policy
            .classes
            .iter()
            .any(|c| c.name == policy.deny.default_class)
        {
            bail!(
                "routing_policy.toml: [deny].default_class {:?} names no [[class]]",
                policy.deny.default_class
            );
        }
        Ok(policy)
    }

    /// Loads and parses the policy file at `path`.
    pub fn load_from(path: &std::path::Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading routing policy {path:?}"))?;
        Self::parse(&text)
    }

    /// The policy embedded in this binary at build time.
    pub fn embedded() -> Result<Self> {
        Self::parse(DEFAULT_POLICY_TOML)
    }

    fn class(&self, name: &str) -> &ClassRule {
        self.classes
            .iter()
            .find(|c| c.name == name)
            .expect("caller only passes names already validated against this policy")
    }

    fn tier_rank(&self, tier: &str) -> Option<usize> {
        self.tier_order.iter().position(|t| t == tier)
    }
}

/// The `Agent` tool_use's routing-relevant fields -- `tool_input.subagent_type`,
/// `tool_input.model`, `tool_input.description`, exactly the three fields
/// `ledger/schema.rs`'s `AgentDispatch` already reads out of a transcript.
/// `prompt` and every other block never reach this struct.
#[derive(Debug, Clone, Default)]
pub struct AgentInput {
    pub subagent_type: Option<String>,
    pub requested_model: Option<String>,
    pub description: Option<String>,
}

impl AgentInput {
    /// Reads the three fields out of a `PreToolUse` hook payload's
    /// `tool_input` object. Any other shape (missing `tool_input`, non-string
    /// fields) yields `None`s rather than an error -- an `Agent` payload
    /// missing all three still routes (to `general`, unset requested model).
    pub fn from_payload(payload: &Value) -> Self {
        let input = payload.get("tool_input");
        let field = |key: &str| -> Option<String> {
            input
                .and_then(|v| v.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        AgentInput {
            subagent_type: field("subagent_type"),
            requested_model: field("model"),
            description: field("description"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
}

#[derive(Debug, Clone)]
pub struct Decision {
    pub class: String,
    /// The resolved model tier, when the decision says anything about one.
    /// `None` for a bare inherit-class allow (policy has no opinion) and for
    /// every deny.
    pub model: Option<String>,
    pub cap_tokens: u64,
    pub decision: Verdict,
    pub reason: String,
    pub policy_version: String,
}

impl Decision {
    pub fn to_json(&self) -> Value {
        json!({
            "class": self.class,
            "model": self.model,
            "cap_tokens": self.cap_tokens,
            "decision": match self.decision { Verdict::Allow => "allow", Verdict::Deny => "deny" },
            "reason": self.reason,
            "policy_version": self.policy_version,
        })
    }
}

fn justified(policy: &Policy, description: Option<&str>) -> bool {
    description
        .map(|d| d.contains(&policy.justify_marker))
        .unwrap_or(false)
}

fn classify<'a>(policy: &'a Policy, input: &AgentInput) -> (&'a ClassRule, &'static str) {
    // The `inherit` class outranks every other match: a `fork` subagent_type
    // or a requested model naming `fable`/`inherit` is the whole point of
    // this class, regardless of what else the dispatch looks like.
    let inherit = policy.classes.iter().find(|c| c.tier == "deny");
    if let Some(inherit) = inherit {
        if input
            .subagent_type
            .as_deref()
            .is_some_and(|t| inherit.subagent_types.iter().any(|s| s == t))
        {
            return (inherit, "subagent_type matches the inherit class");
        }
        if input
            .requested_model
            .as_deref()
            .is_some_and(|m| inherit.requested_models.iter().any(|r| r == m))
        {
            return (inherit, "requested model matches the inherit class");
        }
    }
    for class in &policy.classes {
        if class.tier == "deny" {
            continue; // already checked above
        }
        if input
            .subagent_type
            .as_deref()
            .is_some_and(|t| class.subagent_types.iter().any(|s| s == t))
        {
            return (class, "subagent_type matches the class");
        }
        if let Some(desc) = input.description.as_deref() {
            if class
                .description_prefixes
                .iter()
                .any(|p| desc.starts_with(p.as_str()))
            {
                return (class, "description prefix matches the class");
            }
        }
    }
    let default = policy.class(&policy.deny.default_class);
    if input.subagent_type.is_none() {
        (
            default,
            "no subagent_type given, falls to the default class",
        )
    } else {
        (
            default,
            "unrecognized subagent_type, falls to the default class",
        )
    }
}

/// The pure routing decision: classify, then compare the requested model (if
/// any) against the class's tier, applying the `justify:` escape uniformly
/// to both the inherit class's outright deny and a real class's
/// above-tier-model deny.
pub fn route(policy: &Policy, input: &AgentInput) -> Decision {
    let (class, class_reason) = classify(policy, input);
    let has_justify = justified(policy, input.description.as_deref());

    if class.tier == "deny" {
        return if has_justify {
            Decision {
                class: class.name.clone(),
                model: input.requested_model.clone(),
                cap_tokens: class.cap_tokens,
                decision: Verdict::Allow,
                reason: format!(
                    "class {} ({class_reason}); justified via description ('{}')",
                    class.name, policy.justify_marker
                ),
                policy_version: policy.policy_version.clone(),
            }
        } else {
            Decision {
                class: class.name.clone(),
                model: None,
                cap_tokens: class.cap_tokens,
                decision: Verdict::Deny,
                reason: format!(
                    "class {} ({class_reason}); policy tier is deny. To pass: prefix the description with '{} <reason>'.",
                    class.name, policy.justify_marker
                ),
                policy_version: policy.policy_version.clone(),
            }
        };
    }

    let class_rank = policy.tier_rank(&class.tier).unwrap_or_else(|| {
        panic!(
            "class {} names tier {:?} not in tier_order",
            class.name, class.tier
        )
    });

    let Some(requested) = input.requested_model.as_deref() else {
        return Decision {
            class: class.name.clone(),
            model: Some(class.tier.clone()),
            cap_tokens: class.cap_tokens,
            decision: Verdict::Allow,
            reason: format!(
                "class {} ({class_reason}); no model requested, using policy tier {}",
                class.name, class.tier
            ),
            policy_version: policy.policy_version.clone(),
        };
    };

    // A requested model this policy has never heard of ranks above every
    // known tier -- fail closed (deny-unless-justify) rather than silently
    // allow an escalation we cannot compare.
    let requested_rank = policy.tier_rank(requested);
    let above = match requested_rank {
        Some(r) => r > class_rank,
        None => true,
    };

    if !above {
        Decision {
            class: class.name.clone(),
            model: Some(requested.to_string()),
            cap_tokens: class.cap_tokens,
            decision: Verdict::Allow,
            reason: format!(
                "class {} ({class_reason}); requested model {requested} is at or below policy tier {}",
                class.name, class.tier
            ),
            policy_version: policy.policy_version.clone(),
        }
    } else if has_justify {
        Decision {
            class: class.name.clone(),
            model: Some(requested.to_string()),
            cap_tokens: class.cap_tokens,
            decision: Verdict::Allow,
            reason: format!(
                "class {} ({class_reason}); requested model {requested} exceeds policy tier {} but justified via description ('{}')",
                class.name, class.tier, policy.justify_marker
            ),
            policy_version: policy.policy_version.clone(),
        }
    } else {
        Decision {
            class: class.name.clone(),
            model: None,
            cap_tokens: class.cap_tokens,
            decision: Verdict::Deny,
            reason: format!(
                "class {} ({class_reason}); requested model {requested} exceeds policy tier {}. To pass: add model: \"{}\" or prefix the description with '{} <reason>'.",
                class.name, class.tier, class.tier, policy.justify_marker
            ),
            policy_version: policy.policy_version.clone(),
        }
    }
}

/// Merges `model` into a clone of `payload.tool_input`, for the
/// `updatedInput` hook field -- confirmed by probe (`docs/routing.md`) to
/// *replace* the whole tool_input rather than patch it, so every existing
/// field (`subagent_type`, `description`, `prompt`, ...) must ride along or
/// the Agent tool's own schema validation rejects the call.
fn updated_input_with_model(payload: &Value, model: &str) -> Value {
    let mut merged = payload
        .get("tool_input")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(map) = merged.as_object_mut() {
        map.insert("model".to_string(), json!(model));
    }
    merged
}

/// The name `dispatch.rs` and `docs/routing.md` use for this hook's own
/// contribution when merging it with `crg_refresh_report_pre`'s answer for
/// an `Agent` `PreToolUse` call. Not a Python-recognized `HookSpec` name --
/// same status as `handlers::BashWriteTargets`'s `"bash_write_targets"`.
pub const AGENT_ROUTE_HOOK_NAME: &str = "forge_route_agent";

/// Computes this hook's own, unmerged contribution to an `Agent`
/// `PreToolUse` call: a `crate::merge::HookOutcome` ready to fold in
/// alongside `crg_refresh_report_pre`'s via `crate::merge::merge`.
/// `FORGE_ROUTE_DISABLE=1` skips routing entirely (bare allow, no
/// `additionalContext`, no `updatedInput`). A malformed embedded policy
/// becomes a `fail_closed` error outcome -- `merge` turns that into a deny,
/// matching `route`'s own CLI exit code 2 in spirit (fail loud, never a
/// silent allow).
pub fn hook_outcome_for_agent(payload: &Value) -> crate::merge::HookOutcome {
    use crate::merge::HookOutcome;

    if std::env::var("FORGE_ROUTE_DISABLE").as_deref() == Ok("1") {
        return HookOutcome {
            name: AGENT_ROUTE_HOOK_NAME.to_string(),
            output: json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                }
            }),
            error: None,
            fail_closed: false,
        };
    }

    let policy = match Policy::embedded() {
        Ok(policy) => policy,
        Err(err) => {
            return HookOutcome {
                name: AGENT_ROUTE_HOOK_NAME.to_string(),
                output: Value::Null,
                error: Some(format!("{err:#}")),
                fail_closed: true,
            };
        }
    };

    let input = AgentInput::from_payload(payload);
    let decision = route(&policy, &input);
    let context_line = format!(
        "forge route: {} -> {} (policy {})",
        decision.class,
        decision.model.as_deref().unwrap_or("-"),
        decision.policy_version,
    );
    let already_had_model = payload
        .get("tool_input")
        .and_then(|ti| ti.get("model"))
        .is_some();
    // A "re-route": enforce mode would set `updatedInput.model` (no model was
    // already on the call, and the policy has one to add). Not a re-route
    // when the call already named its own model -- enforce mode never
    // overrides an explicit request.
    let would_reroute = !already_had_model && decision.model.is_some();
    let mode = resolve_mode();

    let mut specific = serde_json::Map::new();
    specific.insert("hookEventName".to_string(), json!("PreToolUse"));
    match decision.decision {
        Verdict::Deny => {
            if mode == "warn" {
                specific.insert("permissionDecision".to_string(), json!("allow"));
                specific.insert(
                    "additionalContext".to_string(),
                    json!(format!(
                        "forge warn-only: would deny -- {}",
                        decision.reason
                    )),
                );
            } else {
                specific.insert("permissionDecision".to_string(), json!("deny"));
                specific.insert(
                    "permissionDecisionReason".to_string(),
                    json!(decision.reason),
                );
            }
        }
        Verdict::Allow => {
            specific.insert("permissionDecision".to_string(), json!("allow"));
            if mode == "warn" {
                // An allow with no change stays silent -- no
                // `additionalContext`, no `updatedInput`. Only a would-be
                // re-route is worth telling the caller about.
                if would_reroute {
                    specific.insert(
                        "additionalContext".to_string(),
                        json!(format!(
                            "forge warn-only: would route {} to {} (class {})",
                            input.subagent_type.as_deref().unwrap_or("-"),
                            decision.model.as_deref().unwrap_or("-"),
                            decision.class,
                        )),
                    );
                }
            } else {
                specific.insert("additionalContext".to_string(), json!(context_line));
                if would_reroute {
                    specific.insert(
                        "updatedInput".to_string(),
                        updated_input_with_model(payload, decision.model.as_deref().unwrap()),
                    );
                }
            }
        }
    }

    HookOutcome {
        name: AGENT_ROUTE_HOOK_NAME.to_string(),
        output: json!({ "hookSpecificOutput": Value::Object(specific) }),
        error: None,
        fail_closed: false,
    }
}

/// `FORGE_MODE`: `"warn"` or the default `"enforce"`. Any other value is a
/// loud stderr line and behaves as `"enforce"` -- an unrecognized mode must
/// never silently soften enforcement. Read fresh on every call (no caching):
/// this is a per-process env var, not a hot loop.
pub fn resolve_mode() -> &'static str {
    match std::env::var("FORGE_MODE") {
        Ok(v) if v == "warn" => "warn",
        Ok(v) if v == "enforce" => "enforce",
        Ok(v) => {
            eprintln!(
                "forge: FORGE_MODE={v:?} is neither \"warn\" nor \"enforce\"; behaving as enforce"
            );
            "enforce"
        }
        Err(_) => "enforce",
    }
}

#[derive(Args)]
pub struct RouteArgs {
    #[arg(long = "subagent-type")]
    pub subagent_type: Option<String>,
    #[arg(long)]
    pub requested: Option<String>,
    #[arg(long)]
    pub description: Option<String>,
    /// Print the decision as JSON. Without it, a one-line human summary.
    #[arg(long)]
    pub json: bool,
    /// Override the embedded policy, for tests and ad hoc checks.
    #[arg(long)]
    pub policy: Option<PathBuf>,
}

/// `forge route`: exit 0 on allow, 1 on deny, 2 on a malformed policy file.
pub fn run(args: RouteArgs) -> Result<i32> {
    let policy = match args.policy {
        Some(path) => Policy::load_from(&path)?,
        None => Policy::embedded()?,
    };
    let input = AgentInput {
        subagent_type: args.subagent_type,
        requested_model: args.requested,
        description: args.description,
    };
    let decision = route(&policy, &input);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&decision.to_json())?);
    } else {
        println!(
            "{} class={} model={} cap_tokens={} reason={:?}",
            match decision.decision {
                Verdict::Allow => "allow",
                Verdict::Deny => "deny",
            },
            decision.class,
            decision.model.as_deref().unwrap_or("-"),
            decision.cap_tokens,
            decision.reason,
        );
    }
    Ok(if decision.decision == Verdict::Deny {
        1
    } else {
        0
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `FORGE_MODE` is a per-process env var read fresh by `resolve_mode`
    /// on every call; any test that sets it must serialize against every
    /// other test in this module touching it, or a parallel `cargo test`
    /// run races (codebase convention -- see `handlers::tests::ENV_LOCK`).
    pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn policy() -> Policy {
        Policy::embedded().expect("embedded policy parses")
    }

    fn input(
        subagent_type: Option<&str>,
        requested: Option<&str>,
        description: Option<&str>,
    ) -> AgentInput {
        AgentInput {
            subagent_type: subagent_type.map(str::to_string),
            requested_model: requested.map(str::to_string),
            description: description.map(str::to_string),
        }
    }

    #[test]
    fn explore_row_haiku_default() {
        let d = route(&policy(), &input(Some("Explore"), None, None));
        assert_eq!(d.class, "explore");
        assert_eq!(d.model.as_deref(), Some("haiku"));
        assert_eq!(d.decision, Verdict::Allow);
        assert_eq!(d.cap_tokens, 150_000);
    }

    #[test]
    fn explore_row_via_claude_code_guide_and_statusline() {
        for t in ["claude-code-guide", "statusline-setup"] {
            let d = route(&policy(), &input(Some(t), None, None));
            assert_eq!(d.class, "explore", "subagent_type {t}");
        }
    }

    #[test]
    fn explore_row_via_clerical_description_prefix() {
        let d = route(
            &policy(),
            &input(None, None, Some("clerical: tidy the log")),
        );
        assert_eq!(d.class, "explore");
        assert_eq!(d.model.as_deref(), Some("haiku"));
    }

    #[test]
    fn general_row_general_purpose_and_claude() {
        for t in ["general-purpose", "claude"] {
            let d = route(&policy(), &input(Some(t), None, None));
            assert_eq!(d.class, "general", "subagent_type {t}");
            assert_eq!(d.model.as_deref(), Some("sonnet"));
        }
    }

    #[test]
    fn general_row_absent_subagent_type() {
        let d = route(&policy(), &input(None, None, None));
        assert_eq!(d.class, "general");
        assert_eq!(d.decision, Verdict::Allow);
    }

    #[test]
    fn design_row_plan_and_prefixes() {
        let d = route(&policy(), &input(Some("Plan"), None, None));
        assert_eq!(d.class, "design");
        assert_eq!(d.model.as_deref(), Some("opus"));
        for prefix in ["design: pick an approach", "rust: port this loop"] {
            let d = route(&policy(), &input(None, None, Some(prefix)));
            assert_eq!(d.class, "design", "prefix {prefix:?}");
        }
    }

    #[test]
    fn inherit_row_fork_denied_without_justify() {
        let d = route(&policy(), &input(Some("fork"), None, None));
        assert_eq!(d.class, "inherit");
        assert_eq!(d.decision, Verdict::Deny);
        assert!(d.model.is_none());
        assert!(d.reason.contains("justify:"));
    }

    #[test]
    fn inherit_row_requested_fable_or_inherit_denied() {
        for m in ["fable", "inherit"] {
            let d = route(&policy(), &input(Some("general-purpose"), Some(m), None));
            assert_eq!(d.class, "inherit", "requested {m}");
            assert_eq!(d.decision, Verdict::Deny);
        }
    }

    #[test]
    fn inherit_row_justify_escape_allows() {
        let d = route(
            &policy(),
            &input(
                Some("fork"),
                None,
                Some("justify: needs the parent's own model"),
            ),
        );
        assert_eq!(d.class, "inherit");
        assert_eq!(d.decision, Verdict::Allow);
    }

    #[test]
    fn above_tier_model_denied_without_justify() {
        let d = route(&policy(), &input(Some("Explore"), Some("opus"), None));
        assert_eq!(d.class, "explore");
        assert_eq!(d.decision, Verdict::Deny);
        assert!(d.reason.contains("add model: \"haiku\""));
        assert!(d.reason.contains("justify:"));
    }

    #[test]
    fn above_tier_model_justify_escape_allows() {
        let d = route(
            &policy(),
            &input(
                Some("Explore"),
                Some("opus"),
                Some("justify: needs deep review"),
            ),
        );
        assert_eq!(d.class, "explore");
        assert_eq!(d.decision, Verdict::Allow);
        assert_eq!(d.model.as_deref(), Some("opus"));
    }

    #[test]
    fn at_or_below_tier_model_allowed_as_requested() {
        let d = route(
            &policy(),
            &input(Some("general-purpose"), Some("haiku"), None),
        );
        assert_eq!(d.class, "general");
        assert_eq!(d.decision, Verdict::Allow);
        assert_eq!(d.model.as_deref(), Some("haiku"));
    }

    #[test]
    fn unknown_subagent_type_falls_to_general() {
        let d = route(&policy(), &input(Some("some-future-agent"), None, None));
        assert_eq!(d.class, "general");
        assert!(d.reason.contains("unrecognized subagent_type"));
    }

    #[test]
    fn malformed_policy_file_fails_loud() {
        let err = Policy::parse("not = [valid").unwrap_err();
        assert!(!format!("{err:#}").is_empty());

        // Well-formed TOML, valid shape, but an empty tier_order -- caught by
        // Policy::parse's own structural check, not serde's.
        let empty_tier_order = r#"
            policy_version = "x"
            tier_order = []
            justify_marker = "justify:"
            [[class]]
            name = "general"
            tier = "sonnet"
            cap_tokens = 1
            [deny]
            default_class = "general"
        "#;
        let err = Policy::parse(empty_tier_order).unwrap_err();
        assert!(format!("{err:#}").contains("tier_order"));

        // deny.default_class names no [[class]].
        let bad_default_class = r#"
            policy_version = "x"
            tier_order = ["haiku"]
            justify_marker = "justify:"
            [[class]]
            name = "general"
            tier = "haiku"
            cap_tokens = 1
            [deny]
            default_class = "missing"
        "#;
        let err = Policy::parse(bad_default_class).unwrap_err();
        assert!(format!("{err:#}").contains("default_class"));
    }

    fn agent_payload(subagent_type: &str, model: Option<&str>) -> Value {
        let mut tool_input = serde_json::json!({ "subagent_type": subagent_type });
        if let Some(m) = model {
            tool_input["model"] = json!(m);
        }
        json!({ "tool_name": "Agent", "tool_input": tool_input })
    }

    #[test]
    fn warn_mode_deny_becomes_allow_with_would_deny_context() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("FORGE_MODE", "warn");

        // "fork" with no requested model and no justify: is an inherit-class
        // deny in enforce mode (see `inherit_row_fork_denied_without_justify`).
        let payload = agent_payload("fork", None);
        let outcome = hook_outcome_for_agent(&payload);
        let specific = &outcome.output["hookSpecificOutput"];

        assert_eq!(specific["permissionDecision"], "allow");
        let context = specific["additionalContext"]
            .as_str()
            .expect("warn-mode deny must carry additionalContext");
        assert!(
            context.starts_with("forge warn-only: would deny -- "),
            "got {context:?}"
        );
        assert!(specific.get("updatedInput").is_none());

        std::env::remove_var("FORGE_MODE");
    }

    #[test]
    fn warn_mode_reroute_becomes_allow_with_context_and_no_updated_input() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("FORGE_MODE", "warn");

        // "Explore" with no model on the call is a would-be reroute to
        // haiku in enforce mode (see `explore_row_haiku_default`).
        let payload = agent_payload("Explore", None);
        let outcome = hook_outcome_for_agent(&payload);
        let specific = &outcome.output["hookSpecificOutput"];

        assert_eq!(specific["permissionDecision"], "allow");
        let context = specific["additionalContext"]
            .as_str()
            .expect("warn-mode reroute must carry additionalContext");
        assert!(
            context.starts_with("forge warn-only: would route "),
            "got {context:?}"
        );
        assert!(
            specific.get("updatedInput").is_none(),
            "warn mode must never apply updatedInput: {specific}"
        );

        std::env::remove_var("FORGE_MODE");
    }

    #[test]
    fn enforce_mode_reroute_is_unchanged_by_warn_mode_support() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::remove_var("FORGE_MODE");

        let payload = agent_payload("Explore", None);
        let outcome = hook_outcome_for_agent(&payload);
        let specific = &outcome.output["hookSpecificOutput"];

        assert_eq!(specific["permissionDecision"], "allow");
        let context = specific["additionalContext"]
            .as_str()
            .expect("enforce mode must still carry its own context line");
        assert!(!context.starts_with("forge warn-only"), "got {context:?}");
        assert_eq!(
            specific["updatedInput"]["model"], "haiku",
            "enforce mode must still apply the reroute: {specific}"
        );
    }

    #[test]
    fn agent_input_from_payload_reads_tool_input() {
        let payload = json!({
            "tool_name": "Agent",
            "tool_input": {
                "subagent_type": "Explore",
                "description": "clerical: tidy",
                "model": "haiku",
            }
        });
        let input = AgentInput::from_payload(&payload);
        assert_eq!(input.subagent_type.as_deref(), Some("Explore"));
        assert_eq!(input.requested_model.as_deref(), Some("haiku"));
        assert_eq!(input.description.as_deref(), Some("clerical: tidy"));
    }
}
