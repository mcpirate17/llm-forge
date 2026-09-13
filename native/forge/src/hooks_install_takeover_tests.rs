// `forge hooks install --takeover` tests, split out of `hooks_install.rs`'s
// `mod tests` to keep that file under the file-size ceiling. Textually
// `include!`d into `hooks_install::tests` (not a nested `mod`, since a
// `#[path]` module here would need a real `src/hooks_install/tests/`
// directory that does not exist), so it shares `tests`'s own scope
// (`ScratchDir`, `install`, `run`, `Value`, `takeover`, ...) already
// brought in by that module's own `use super::*`.

/// A settings shape with the host's Python dispatcher wired for all
/// four events, matcher `.*` for Pre/PostToolUse and no matcher for
/// Session events -- the real shape (see `docs/routing.md`'s coverage
/// discussion), not any project's actual file.
fn dispatcher_settings() -> String {
    r#"{
  "hooks": {
"PreToolUse": [
  { "matcher": ".*", "hooks": [ { "type": "command", "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/dispatch.py PreToolUse", "timeout": 30 } ] }
],
"PostToolUse": [
  { "matcher": ".*", "hooks": [ { "type": "command", "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/dispatch.py PostToolUse", "timeout": 30 } ] }
],
"SessionStart": [
  { "hooks": [ { "type": "command", "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/dispatch.py SessionStart", "timeout": 10 } ] }
],
"SessionEnd": [
  { "hooks": [ { "type": "command", "command": "$CLAUDE_PROJECT_DIR/.claude/hooks/dispatch.py SessionEnd", "timeout": 10 } ] }
],
"SomeoneElsesHook": [
  { "matcher": "Bash", "hooks": [ { "type": "command", "command": "/opt/other/tool.sh", "timeout": 5 } ] }
]
  },
  "unrelatedTopLevelKey": true
}"#
    .to_string()
}

#[test]
fn takeover_removes_only_full_events_python_entries() {
    let scratch = ScratchDir::new("takeover-full-only");
    scratch.write_settings(&dispatcher_settings());
    install(&takeover_args(scratch.path(), false)).unwrap();
    let settings = scratch.settings_value();

    // PostToolUse (Full): Python entry gone, forge entry present.
    let post = commands_for(&settings, "PostToolUse");
    assert!(!post.iter().any(|c| c.contains("dispatch.py")));
    assert!(post
        .iter()
        .any(|c| c.contains("forge") && c.contains("hook PostToolUse")));

    // PreToolUse (Partial: Read/Edit/mcp graph tools still need Python):
    // the Python entry must survive, narrowed to just those tools, and
    // the forge entry must keep matcher `.*` (it still needs to see Bash).
    let pre_list = &settings["hooks"]["PreToolUse"];
    let pre = commands_for(&settings, "PreToolUse");
    assert!(pre.iter().any(|c| c.contains("dispatch.py PreToolUse")));
    let python_entry = pre_list
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["hooks"][0]["command"]
                .as_str()
                .is_some_and(|c| c.contains("dispatch.py"))
        })
        .unwrap();
    assert_eq!(
        python_entry["matcher"],
        "Read|Edit|Write|NotebookEdit|mcp__code[-_]review[-_]graph__.*"
    );
    let forge_entry = pre_list
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["hooks"][0]["command"]
                .as_str()
                .is_some_and(|c| c.contains("forge"))
        })
        .unwrap();
    assert_eq!(forge_entry["matcher"], ".*");

    // SessionStart/SessionEnd (Partial, no narrower matcher to give):
    // untouched.
    for event in ["SessionStart", "SessionEnd"] {
        let cmds = commands_for(&settings, event);
        assert!(cmds
            .iter()
            .any(|c| c.contains(&format!("dispatch.py {event}"))));
    }

    // Unrelated entries preserved byte-for-byte in shape.
    assert_eq!(settings["hooks"]["SomeoneElsesHook"][0]["matcher"], "Bash");
    assert_eq!(settings["unrelatedTopLevelKey"], true);

    // The narrowed PreToolUse entry's ORIGINAL (`.*`) shape was
    // recorded verbatim, same mechanism as the removed PostToolUse one.
    let record: Value = serde_json::from_str(
        &std::fs::read_to_string(takeover::takeover_path(scratch.path())).unwrap(),
    )
    .unwrap();
    assert_eq!(record["PreToolUse"]["matcher"], ".*");
    assert_eq!(
        record["PreToolUse"]["hooks"][0]["command"],
        "$CLAUDE_PROJECT_DIR/.claude/hooks/dispatch.py PreToolUse"
    );
    assert!(record.get("SessionStart").is_none());
    let recorded_post = &record["PostToolUse"];
    assert_eq!(recorded_post["matcher"], ".*");
    assert_eq!(
        recorded_post["hooks"][0]["command"],
        "$CLAUDE_PROJECT_DIR/.claude/hooks/dispatch.py PostToolUse"
    );
}

#[test]
fn second_takeover_run_is_a_no_op() {
    let scratch = ScratchDir::new("takeover-idempotent");
    scratch.write_settings(&dispatcher_settings());
    install(&takeover_args(scratch.path(), false)).unwrap();
    let after_first = scratch.settings_text();
    let record_after_first =
        std::fs::read_to_string(takeover::takeover_path(scratch.path())).unwrap();

    install(&takeover_args(scratch.path(), false)).unwrap();
    assert_eq!(scratch.settings_text(), after_first);
    assert_eq!(
        std::fs::read_to_string(takeover::takeover_path(scratch.path())).unwrap(),
        record_after_first
    );
}

#[test]
fn uninstall_restores_the_exact_pre_takeover_python_entry() {
    let scratch = ScratchDir::new("takeover-uninstall");
    let original: Value = serde_json::from_str(&dispatcher_settings()).unwrap();
    scratch.write_settings(&dispatcher_settings());
    install(&takeover_args(scratch.path(), false)).unwrap();

    run(HooksCommand::Uninstall(UninstallArgs {
        host: scratch.path().to_path_buf(),
        dry_run: false,
    }))
    .unwrap();

    let restored = scratch.settings_value();
    assert_eq!(
        restored["hooks"]["PostToolUse"][0],
        original["hooks"]["PostToolUse"][0]
    );
    // The narrowed PreToolUse Python entry's matcher goes back to `.*`,
    // in place (no duplicate row), and the forge entry stays alongside it.
    let pre = restored["hooks"]["PreToolUse"].as_array().unwrap();
    let python_entries: Vec<&Value> = pre
        .iter()
        .filter(|entry| {
            entry["hooks"][0]["command"]
                .as_str()
                .is_some_and(|c| c.contains("dispatch.py"))
        })
        .collect();
    assert_eq!(python_entries.len(), 1, "no duplicate python row: {pre:?}");
    assert_eq!(python_entries[0]["matcher"], ".*");
    assert!(!takeover::takeover_path(scratch.path()).is_file());
}

#[test]
fn a_second_narrowing_run_changes_nothing_more() {
    let scratch = ScratchDir::new("takeover-narrow-idempotent");
    scratch.write_settings(&dispatcher_settings());
    install(&takeover_args(scratch.path(), false)).unwrap();
    let pre_after_first = scratch.settings_value()["hooks"]["PreToolUse"].clone();

    install(&takeover_args(scratch.path(), false)).unwrap();
    let pre_after_second = scratch.settings_value()["hooks"]["PreToolUse"].clone();
    assert_eq!(pre_after_first, pre_after_second);
}

#[test]
fn narrowing_dry_run_writes_nothing() {
    let scratch = ScratchDir::new("takeover-narrow-dry-run");
    scratch.write_settings(&dispatcher_settings());
    let before = scratch.settings_text();

    install(&takeover_args(scratch.path(), true)).unwrap();

    assert_eq!(
        scratch.settings_text(),
        before,
        "dry-run must write nothing"
    );
    assert!(!takeover::takeover_path(scratch.path()).is_file());
}

#[test]
fn status_reports_the_narrowed_matcher() {
    let scratch = ScratchDir::new("takeover-narrow-status");
    scratch.write_settings(&dispatcher_settings());
    install(&takeover_args(scratch.path(), false)).unwrap();
    let (_, settings) = load_settings(scratch.path()).unwrap();

    let python = takeover::python_status(&settings, scratch.path(), "PreToolUse");
    assert_eq!(
        python,
        "narrowed(Read|Edit|Write|NotebookEdit|mcp__code[-_]review[-_]graph__.*)"
    );
}

#[test]
fn takeover_dry_run_writes_nothing() {
    let scratch = ScratchDir::new("takeover-dry-run");
    scratch.write_settings(&dispatcher_settings());
    let before = scratch.settings_text();
    install(&takeover_args(scratch.path(), true)).unwrap();
    assert_eq!(scratch.settings_text(), before);
    assert!(!takeover::takeover_path(scratch.path()).is_file());
}
