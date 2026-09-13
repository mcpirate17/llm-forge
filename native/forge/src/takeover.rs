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
//! `PreToolUse` is the one that surprises, twice over. First:
//! `dispatch::run_pre_tool_use` (the non-standalone path) only
//! special-cases `tool_name == "Bash"` and `tool_name == "Agent"`, so a host
//! settings entry with matcher `".*"` (the real shape in every project seen
//! so far -- Python's own per-tool filtering happens inside the dispatcher,
//! not in settings.json) still needs Python for `Read`, `Edit`, `Write`,
//! `NotebookEdit` and the `mcp__code_review_graph__*` tools. That makes
//! `PreToolUse` `Partial` at the `".*"` matcher even though the `Bash` and
//! `Agent` cases it is built from would each look `Full` in isolation.
//!
//! Second, and more subtle: `--takeover` always implies `--standalone`
//! (`FORGE_HOOK_STANDALONE=1`), and the standalone path is a *different*
//! function -- `dispatch::run_pre_tool_use_standalone` -- whose own doc
//! comment says plainly that "anything else -- Bash included -- has nothing
//! native to say under standalone and prints nothing." Measured directly
//! (item 3(b) of the takeover slice): piping a `Bash` `rm -rf /home/x` or
//! `git push --force origin main` payload through
//! `FORGE_MODE=warn FORGE_HOOK_STANDALONE=1 forge hook PreToolUse` produces
//! **no deny at all** -- the same payload only denies under the
//! non-standalone `forge hook PreToolUse`. So even a hypothetical host
//! entry with the literal matcher `"Bash"` is not safe to take over: the
//! guard denials `BASH_PRETOOLUSE_HOOK_NAMES` promises are not live in the
//! one mode `--takeover` will ever run forge in. `"Bash"` is therefore
//! `Partial` here too, naming its own native names as missing -- not
//! because they lack a Rust implementation, but because the standalone
//! entrypoint never calls it. `"Agent"` is unaffected:
//! `run_pre_tool_use_standalone` does special-case `tool_name == "Agent"`
//! and its routing verdict is confirmed live under standalone.

use crate::handlers::{
    AGENT_PRETOOLUSE_HOOK_NAMES, BASH_PRETOOLUSE_HOOK_NAMES, POST_TOOL_USE_HOOK_NAMES,
};

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

/// `registry.py` `PreToolUse` hooks that match only `Read`
/// (`current_work_guard_read`, `pre_read_skeleton`), only
/// `Edit|Write|NotebookEdit` (`crg_gate_verify`, `current_work_guard_edit`)
/// or only the graph-tool matcher (`crg_gate_mark`, `crg_refresh_wait`) --
/// none has a native twin (absent from every `*_HOOK_NAMES` list in
/// `handlers.rs`), and `dispatch::run_pre_tool_use` never special-cases
/// their tools, so any `PreToolUse` matcher other than exactly `"Bash"` or
/// exactly `"Agent"` is missing all six.
const PRETOOLUSE_NON_BASH_NON_AGENT_ONLY: [&str; 6] = [
    "current_work_guard_read",
    "pre_read_skeleton",
    "crg_gate_verify",
    "current_work_guard_edit",
    "crg_gate_mark",
    "crg_refresh_wait",
];

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
            // Not Full: `run_pre_tool_use_standalone` -- the only path
            // `--takeover` (implies `--standalone`) ever runs -- never
            // calls the Bash guard logic; see the module doc comment and
            // item 3(b)'s measured `rm -rf`/`git push --force` non-denies.
            "Bash" => Coverage::Partial {
                missing: BASH_PRETOOLUSE_HOOK_NAMES.to_vec(),
            },
            "Agent" => Coverage::Full {
                native: AGENT_PRETOOLUSE_HOOK_NAMES.to_vec(),
            },
            _ => Coverage::Partial {
                missing: PRETOOLUSE_NON_BASH_NON_AGENT_ONLY.to_vec(),
            },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::SESSIONSTART_HOOK_NAMES;
    use std::collections::HashSet;

    #[test]
    fn pretooluse_bash_is_partial_because_standalone_never_runs_the_guard() {
        // `--takeover` implies `--standalone`, and
        // `run_pre_tool_use_standalone` has nothing to say for Bash --
        // confirmed live (item 3(b)): no deny for `rm -rf` or
        // `git push --force` under
        // `FORGE_MODE=warn FORGE_HOOK_STANDALONE=1 forge hook PreToolUse`.
        let Coverage::Partial { missing } = coverage("PreToolUse", "Bash") else {
            panic!("expected Partial");
        };
        let expected: HashSet<&str> = BASH_PRETOOLUSE_HOOK_NAMES.into_iter().collect();
        let got: HashSet<&str> = missing.into_iter().collect();
        assert_eq!(got, expected);
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
    fn pretooluse_catchall_matcher_is_partial() {
        let Coverage::Partial { missing } = coverage("PreToolUse", ".*") else {
            panic!("expected Partial for the real host matcher shape");
        };
        assert!(missing.contains(&"current_work_guard_read"));
        assert!(missing.contains(&"crg_gate_verify"));
        assert!(missing.contains(&"crg_gate_mark"));
        // Never claim Bash/Agent's own native names are missing.
        for name in BASH_PRETOOLUSE_HOOK_NAMES {
            assert!(!missing.contains(&name));
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
    /// Coverage is `Partial`; the Python entry stays.
    Kept { missing: Vec<&'static str> },
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
                outcomes.push((event.to_string(), EventOutcome::Kept { missing }));
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
        if find_python_entry(list, event).is_none() {
            list.push(entry.clone());
            restored.push(event.clone());
        }
    }
    fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    Ok(restored)
}

/// `forge hooks status`'s `python` column: `present` (a Python dispatcher
/// entry is in settings today), `taken-over` (removed, recorded in the
/// takeover file) or `n/a` (never had one).
pub fn python_status(settings: &Value, host: &Path, event: &str) -> &'static str {
    let present = settings
        .get("hooks")
        .and_then(Value::as_object)
        .and_then(|hooks| hooks.get(event))
        .and_then(Value::as_array)
        .map(|list| find_python_entry(list, event).is_some())
        .unwrap_or(false);
    if present {
        return "present";
    }
    let recorded = load_record(host)
        .map(|record| record.contains_key(event))
        .unwrap_or(false);
    if recorded {
        "taken-over"
    } else {
        "n/a"
    }
}
