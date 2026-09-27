//! Coverage data for `forge hooks install --takeover`: for a Python
//! dispatcher `HookSpec` set matched by one `(event, tool_matcher)` pair (the
//! `tool_matcher` being either a literal tool name forge special-cases --
//! `"Bash"`, `"Agent"` -- or the matcher string a host `.claude/settings.json`
//! entry actually carries, e.g. `".*"` or `""`), says whether every hook
//! Python's registry (`src/tooling/hooks/dispatch/registry.py`) matches for
//! it already has a native twin (`Coverage::Full`) or not
//! (`Coverage::Partial`, naming what is missing).
//!
//! Ground truth lives in two places forge does not parse at runtime:
//! `registry.py`'s `HOOKS` tuple (the Python names and their matchers) and
//! this crate's own `handlers.rs` (`BASH_PRETOOLUSE_HOOK_NAMES`,
//! `AGENT_PRETOOLUSE_HOOK_NAMES`, `POST_TOOL_USE_HOOK_NAMES`,
//! `SESSIONSTART_HOOK_NAMES`) and `dispatch.rs` (which tool names ever reach
//! a fully-native branch at all). `coverage`'s unit tests cross-check every
//! `Full` name against those constants so the two files cannot silently
//! drift apart.
//!
//! Both ordinary and standalone dispatch serve every registered `PreToolUse`
//! tool family natively. Takeover implies standalone, so its catchall Python
//! entry can be removed. Arbitrary regex matchers remain conservative.

use crate::handlers::{
    generic_pretooluse_tool, AGENT_PRETOOLUSE_HOOK_NAMES, BASH_PRETOOLUSE_HOOK_NAMES,
    POST_TOOL_USE_HOOK_NAMES, READ_PRETOOLUSE_HOOK_NAMES,
};
use crate::pre_edit::{EDIT_PRETOOLUSE_HOOK_NAMES, GRAPH_PRETOOLUSE_HOOK_NAMES};

/// Native coverage for one `(event, tool_matcher)` pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coverage {
    /// Every Python hook this pair matches has a native twin; the host's
    /// Python dispatcher entry for this event can be removed outright.
    Full { native: Vec<&'static str> },
    /// At least one matching Python hook has no native twin, named here;
    /// the host's Python dispatcher entry must stay.
    Partial { missing: Vec<&'static str> },
}

fn all_pretooluse_names() -> Vec<&'static str> {
    let mut names = Vec::new();
    for name in GRAPH_PRETOOLUSE_HOOK_NAMES
        .into_iter()
        .chain(BASH_PRETOOLUSE_HOOK_NAMES)
        .chain(READ_PRETOOLUSE_HOOK_NAMES)
        .chain(EDIT_PRETOOLUSE_HOOK_NAMES)
    {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// `registry.py` `SessionStart` hooks with no native twin: everything
/// except `workspace_exposure_session` (`SESSIONSTART_HOOK_NAMES`).
/// `dispatch::run_session_start` always delegates to Python regardless, so
/// every matcher is `Partial`.
const SESSIONSTART_MISSING: [&str; 4] = [
    "crg_refresh_report_session",
    "session_start",
    "session_handoff",
    "native_freshness",
];

/// `registry.py`'s only `SessionEnd` hook. `dispatch::run_session_end`
/// always delegates to Python after its native rollup side effect -- there
/// is no fully-native `SessionEnd` path at all, so this event is always
/// `Partial`.
const SESSIONEND_MISSING: [&str; 1] = ["obsidian_session_end"];

/// Coverage for one Python dispatcher event, given the tool matcher its
/// host settings entry (or forge's own special-cased tool name) carries.
pub fn coverage(event: &str, tool_matcher: &str) -> Coverage {
    match event {
        "PreToolUse" => match tool_matcher {
            "" | ".*" => Coverage::Full {
                native: all_pretooluse_names(),
            },
            // Full: `run_pre_tool_use_standalone` -- the only path
            // `--takeover` (implies `--standalone`) ever runs -- now runs
            // the same fully-native Bash guard the non-standalone path
            // does; see the module doc comment.
            "Bash" => Coverage::Full {
                native: BASH_PRETOOLUSE_HOOK_NAMES.to_vec(),
            },
            "Agent" => Coverage::Full {
                native: AGENT_PRETOOLUSE_HOOK_NAMES.to_vec(),
            },
            "Read" => Coverage::Full {
                native: READ_PRETOOLUSE_HOOK_NAMES.to_vec(),
            },
            "Edit" | "Write" | "NotebookEdit" | "Edit|Write|NotebookEdit" => Coverage::Full {
                native: EDIT_PRETOOLUSE_HOOK_NAMES.to_vec(),
            },
            "mcp__code[-_]review[-_]graph__.*" => Coverage::Full {
                native: GRAPH_PRETOOLUSE_HOOK_NAMES.to_vec(),
            },
            name if name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
                && crate::pre_edit::is_graph_tool_name(name) =>
            {
                Coverage::Full {
                    native: GRAPH_PRETOOLUSE_HOOK_NAMES.to_vec(),
                }
            }
            name if name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
                && generic_pretooluse_tool(name) =>
            {
                Coverage::Full {
                    native: AGENT_PRETOOLUSE_HOOK_NAMES.to_vec(),
                }
            }
            _ => Coverage::Partial { missing: vec![] },
        },
        "PostToolUse" => Coverage::Full {
            native: POST_TOOL_USE_HOOK_NAMES.to_vec(),
        },
        "SessionStart" => Coverage::Partial {
            missing: SESSIONSTART_MISSING.to_vec(),
        },
        "SessionEnd" => Coverage::Partial {
            missing: SESSIONEND_MISSING.to_vec(),
        },
        _ => Coverage::Partial { missing: vec![] },
    }
}

/// The alternation of tool matchers that still need Python for `event`, or
/// `None` when the event is fully native or has no narrowed residual matcher.
pub fn residual_python_matcher(_event: &str) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::SESSIONSTART_HOOK_NAMES;
    use std::collections::HashSet;

    #[test]
    fn pretooluse_bash_is_full_now_that_standalone_runs_the_guard() {
        // `--takeover` implies `--standalone`, and
        // `run_pre_tool_use_standalone` now runs the same fully-native Bash
        // guard the non-standalone path does.
        let Coverage::Full { native } = coverage("PreToolUse", "Bash") else {
            panic!("expected Full");
        };
        let expected: HashSet<&str> = BASH_PRETOOLUSE_HOOK_NAMES.into_iter().collect();
        let got: HashSet<&str> = native.into_iter().collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn residual_python_matcher_is_none_for_native_pretooluse() {
        assert_eq!(residual_python_matcher("PreToolUse"), None);
    }

    #[test]
    fn residual_python_matcher_is_none_once_coverage_is_full() {
        assert_eq!(residual_python_matcher("PostToolUse"), None);
    }

    #[test]
    fn pretooluse_agent_is_full_and_matches_handlers_constant() {
        let Coverage::Full { native } = coverage("PreToolUse", "Agent") else {
            panic!("expected Full");
        };
        let expected: HashSet<&str> = AGENT_PRETOOLUSE_HOOK_NAMES.into_iter().collect();
        let got: HashSet<&str> = native.into_iter().collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn pretooluse_catchall_matcher_covers_every_registry_family() {
        let Coverage::Full { native } = coverage("PreToolUse", ".*") else {
            panic!("expected Full for the real host matcher shape");
        };
        for name in BASH_PRETOOLUSE_HOOK_NAMES
            .into_iter()
            .chain(READ_PRETOOLUSE_HOOK_NAMES)
            .chain(EDIT_PRETOOLUSE_HOOK_NAMES)
            .chain(GRAPH_PRETOOLUSE_HOOK_NAMES)
        {
            assert!(native.contains(&name), "missing native hook: {name}");
        }
    }

    #[test]
    fn pretooluse_read_and_literal_generic_matchers_are_full() {
        assert_eq!(
            coverage("PreToolUse", "Read"),
            Coverage::Full {
                native: READ_PRETOOLUSE_HOOK_NAMES.to_vec(),
            }
        );
        for name in ["Grep", "Glob", "WebFetch", "mcp__other__lookup"] {
            assert_eq!(
                coverage("PreToolUse", name),
                Coverage::Full {
                    native: AGENT_PRETOOLUSE_HOOK_NAMES.to_vec(),
                }
            );
        }
    }

    #[test]
    fn specialized_gates_are_full_but_arbitrary_regexes_stay_partial() {
        for matcher in [
            "Edit",
            "Write",
            "NotebookEdit",
            "Edit|Write|NotebookEdit",
            "mcp__code[-_]review[-_]graph__.*",
            "mcp__code-review-graph__query",
            "mcp__code-review_graph__query",
            "mcp__code_review-graph__query",
            "mcp__code_review_graph__query",
        ] {
            assert!(
                matches!(coverage("PreToolUse", matcher), Coverage::Full { .. }),
                "{matcher}"
            );
        }
        for matcher in ["Read|Edit", "G.*"] {
            assert!(matches!(
                coverage("PreToolUse", matcher),
                Coverage::Partial { .. }
            ));
        }
    }

    #[test]
    fn posttooluse_any_matcher_is_full_and_matches_handlers_constant() {
        for matcher in [".*", "Bash", "Read", "Edit|Write|NotebookEdit"] {
            let Coverage::Full { native } = coverage("PostToolUse", matcher) else {
                panic!("expected Full for PostToolUse/{matcher}");
            };
            let expected: HashSet<&str> = POST_TOOL_USE_HOOK_NAMES.into_iter().collect();
            let got: HashSet<&str> = native.into_iter().collect();
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn sessionstart_is_partial_and_names_the_unported_hooks() {
        let Coverage::Partial { missing } = coverage("SessionStart", "") else {
            panic!("expected Partial");
        };
        for name in SESSIONSTART_HOOK_NAMES {
            assert!(!missing.contains(&name));
        }
        assert_eq!(missing.len(), SESSIONSTART_MISSING.len());
    }

    #[test]
    fn sessionend_is_partial() {
        assert_eq!(
            coverage("SessionEnd", ""),
            Coverage::Partial {
                missing: SESSIONEND_MISSING.to_vec()
            }
        );
    }

    #[test]
    fn unknown_event_is_partial_with_nothing_named() {
        assert_eq!(
            coverage("SubagentStop", ""),
            Coverage::Partial { missing: vec![] }
        );
    }
}

// ── settings manipulation: `forge hooks install --takeover` ───────────────

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// The file `--takeover` records removed Python entries into, created once
/// beside `.claude/settings.json` and merged (never overwritten blind) on
/// every later run.
pub const TAKEOVER_FILE: &str = "settings.forge-takeover.json";

pub fn takeover_path(host: &Path) -> PathBuf {
    host.join(".claude").join(TAKEOVER_FILE)
}

/// Every event a takeover pass considers, in settings.json key order.
const TAKEOVER_EVENTS: [&str; 4] = ["PreToolUse", "PostToolUse", "SessionStart", "SessionEnd"];

/// The event name a Python dispatcher command names, or `None` for anything
/// else (a forge command, an unrelated hook, junk). Recognises both the
/// script form (`.../dispatch.py <Event>`) and the module form
/// (`python -m tooling.hooks.dispatch <Event>`), whatever env-var prefix
/// precedes them, mirroring `hooks_install::parse_hook_command`'s
/// token-based approach.
fn python_dispatcher_event(command: &str) -> Option<String> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }
    let event = tokens[tokens.len() - 1];
    let rest = &tokens[..tokens.len() - 1];
    let is_script = rest
        .last()
        .and_then(|t| Path::new(t).file_name())
        .and_then(|n| n.to_str())
        == Some("dispatch.py");
    let is_module = rest
        .windows(2)
        .any(|w| w[0] == "-m" && w[1] == "tooling.hooks.dispatch");
    (is_script || is_module).then(|| event.to_string())
}

/// The index of the first entry in a `hooks.<event>` array whose command is
/// the Python dispatcher for `event`, plus that entry's `matcher` field
/// (`""` when absent, matching the hand-installed SessionStart/SessionEnd
/// shape).
fn find_python_entry(list: &[Value], event: &str) -> Option<(usize, String)> {
    list.iter().enumerate().find_map(|(i, entry)| {
        let is_python = entry
            .get("hooks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|h| h.get("command").and_then(Value::as_str))
            .any(|cmd| python_dispatcher_event(cmd).as_deref() == Some(event));
        if !is_python {
            return None;
        }
        let matcher = entry
            .get("matcher")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Some((i, matcher))
    })
}

/// Whether `hooks.<event>` already has a forge entry (any of
/// `hooks_install::entry_parsed_hooks` for it).
fn has_forge_entry(list: &[Value], event: &str) -> bool {
    list.iter().any(|entry| {
        crate::hooks_install::entry_parsed_hooks(entry)
            .iter()
            .any(|parsed| parsed.event == event)
    })
}

fn load_record(host: &Path) -> Result<Map<String, Value>> {
    let path = takeover_path(host);
    if !path.is_file() {
        return Ok(Map::new());
    }
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("{} is not valid JSON", path.display()))?;
    value
        .as_object()
        .cloned()
        .with_context(|| format!("{} must be a JSON object", path.display()))
}

fn write_record(host: &Path, record: &Map<String, Value>) -> Result<()> {
    let path = takeover_path(host);
    let text = serde_json::to_string_pretty(&Value::Object(record.clone()))
        .context("serializing the takeover record")?
        + "\n";
    crate::hooks_install::write_atomic(&path, &text)
}

/// One line of `--takeover`'s report: what happened to one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventOutcome {
    /// The Python entry was removed and recorded (first time this ran).
    TakenOver,
    /// Already taken over by an earlier run; nothing changed this time.
    AlreadyTakenOver,
    /// Coverage is `Partial` and there is no narrower matcher to give
    /// Python (`residual_python_matcher` returned `None`); the entry stays
    /// exactly as it was.
    Kept { missing: Vec<&'static str> },
    /// Coverage is `Partial` but `residual_python_matcher` named a narrower
    /// matcher than the entry's own: its `matcher` field was rewritten to
    /// that (the original recorded verbatim, first time this ran).
    Narrowed { matcher: String },
    /// Already narrowed by an earlier run; nothing changed this time.
    AlreadyNarrowed { matcher: String },
    /// No Python dispatcher entry exists for this event; nothing to do.
    NoPythonEntry,
}

/// Removes the host's Python dispatcher entry for every `Full`-coverage
/// event, records each one verbatim into the takeover file (creating it
/// once, merging into it on a later run, never overwriting an entry it
/// already recorded), and adds a forge entry for the event when the normal
/// `--standalone` install would not otherwise install one (`PostToolUse`,
/// whose standalone dispatch has no branch of its own -- see
/// `dispatch::run_hook_standalone`). `Partial` events are left completely
/// untouched. Idempotent: a second run finds no Python entry left to
/// remove and reports `AlreadyTakenOver`/`NoPythonEntry` for every event.
/// Pure in-memory when `dry_run` is set -- the caller decides whether to
/// persist `settings` and the returned record.
/// Per-event outcomes plus the (possibly updated) takeover record, as
/// `apply` returns them.
pub type ApplyResult = (Vec<(String, EventOutcome)>, Map<String, Value>);

pub fn apply(
    settings: &mut Value,
    host: &Path,
    binary: &str,
    mode: &str,
    dry_run: bool,
) -> Result<ApplyResult> {
    let mut record = load_record(host)?;
    let mut outcomes = Vec::new();
    let root = settings
        .as_object_mut()
        .context("settings.json must be a JSON object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context(r#""hooks" must be a JSON object"#)?;

    for event in TAKEOVER_EVENTS {
        let list = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!(r#""hooks.{event}" must be a JSON array"#))?;
        let Some((index, matcher)) = find_python_entry(list, event) else {
            outcomes.push((event.to_string(), EventOutcome::NoPythonEntry));
            continue;
        };
        match coverage(event, &matcher) {
            Coverage::Partial { missing } => {
                let Some(residual) = residual_python_matcher(event) else {
                    outcomes.push((event.to_string(), EventOutcome::Kept { missing }));
                    continue;
                };
                if matcher == residual {
                    outcomes.push((
                        event.to_string(),
                        EventOutcome::AlreadyNarrowed { matcher: residual },
                    ));
                    continue;
                }
                let already_recorded = record.contains_key(event);
                if !already_recorded {
                    record.insert(event.to_string(), list[index].clone());
                }
                list[index]
                    .as_object_mut()
                    .with_context(|| format!("hooks.{event}[{index}] must be a JSON object"))?
                    .insert("matcher".to_string(), Value::String(residual.clone()));
                outcomes.push((
                    event.to_string(),
                    EventOutcome::Narrowed { matcher: residual },
                ));
                continue;
            }
            Coverage::Full { .. } => {}
        }
        let already_recorded = record.contains_key(event);
        let py_entry = list[index].clone();
        if !already_recorded {
            record.insert(event.to_string(), py_entry);
        }
        if !has_forge_entry(list, event) {
            let command = crate::hooks_install::hook_command(binary, mode, false, event);
            list.push(crate::hooks_install::forge_entry(event, &command));
        }
        // Re-borrow: `has_forge_entry`/push above may have reallocated `list`.
        let list = hooks
            .get_mut(event)
            .and_then(Value::as_array_mut)
            .expect("event key just written above");
        if let Some((index, _)) = find_python_entry(list, event) {
            list.remove(index);
        }
        outcomes.push((
            event.to_string(),
            if already_recorded {
                EventOutcome::AlreadyTakenOver
            } else {
                EventOutcome::TakenOver
            },
        ));
    }

    if hooks.is_empty() {
        root.remove("hooks");
    }
    if !dry_run {
        write_record(host, &record)?;
    }
    Ok((outcomes, record))
}

/// `forge hooks uninstall`'s companion: appends every entry the takeover
/// file recorded back into `hooks.<event>` (recreating the array/object as
/// needed) and deletes the takeover file. A no-op, not an error, when no
/// takeover file exists.
pub fn restore(settings: &mut Value, host: &Path) -> Result<Vec<String>> {
    let path = takeover_path(host);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let record = load_record(host)?;
    let mut restored = Vec::new();
    let root = settings
        .as_object_mut()
        .context("settings.json must be a JSON object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context(r#""hooks" must be a JSON object"#)?;
    for (event, entry) in record.iter() {
        let list = hooks
            .entry(event.clone())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!(r#""hooks.{event}" must be a JSON array"#))?;
        match find_python_entry(list, event) {
            // Full case: the entry was removed outright; add it back.
            None => {
                list.push(entry.clone());
                restored.push(event.clone());
            }
            // Narrowed case: the entry is still present (same command) but
            // its matcher was rewritten; replace it with the recorded
            // original in place rather than duplicating the row.
            Some((index, current_matcher)) => {
                let original_matcher = entry.get("matcher").and_then(Value::as_str).unwrap_or("");
                if current_matcher != original_matcher {
                    list[index] = entry.clone();
                    restored.push(event.clone());
                }
            }
        }
    }
    fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    Ok(restored)
}

/// `forge hooks status`'s `python` column: `present` (a Python dispatcher
/// entry is in settings today, untouched), `narrowed(<matcher>)` (present
/// but its matcher was rewritten to just the tools still needing Python),
/// `taken-over` (removed outright, recorded in the takeover file) or `n/a`
/// (never had one).
pub fn python_status(settings: &Value, host: &Path, event: &str) -> String {
    let matcher = settings
        .get("hooks")
        .and_then(Value::as_object)
        .and_then(|hooks| hooks.get(event))
        .and_then(Value::as_array)
        .and_then(|list| find_python_entry(list, event));
    if let Some((_, matcher)) = matcher {
        let recorded_original = load_record(host)
            .ok()
            .and_then(|record| record.get(event).cloned());
        let was_narrowed = recorded_original
            .as_ref()
            .and_then(|entry| entry.get("matcher"))
            .and_then(Value::as_str)
            .is_some_and(|original| original != matcher);
        return if was_narrowed {
            format!("narrowed({matcher})")
        } else {
            "present".to_string()
        };
    }
    let recorded = load_record(host)
        .map(|record| record.contains_key(event))
        .unwrap_or(false);
    if recorded {
        "taken-over".to_string()
    } else {
        "n/a".to_string()
    }
}
