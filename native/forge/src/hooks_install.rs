//! `forge hooks install|uninstall|status`: manage forge's own entries in a
//! host project's `.claude/settings.json` (`docs/roadmap.md` Phase 4, its
//! first Rust piece). The hand edit this replaces: add a PreToolUse `.*`
//! entry and a SubagentStop entry pointing at the forge binary, keep a
//! backup at `.claude/settings.pre-forge.bak.json`. Hand edits do not
//! survive a second host, a mode flip (warn -> enforce is scheduled for
//! ~2026-09-20), or a binary path change; this module makes all three one
//! command, and undoing it one command too.
//!
//! Never replaces what it did not install: an entry is forge's iff one of
//! its hooks' `command` ends with `forge hook <Event>`. Everything else in
//! the file -- other hook entries, other top-level keys, key order -- is
//! preserved (serde_json `preserve_order` keeps the object key order both
//! on load and on write).

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The backup `install` writes once, before its first write, and that
/// `uninstall` deliberately leaves in place (rollback = restore it).
pub const BACKUP_FILE: &str = "settings.pre-forge.bak.json";

const SETTINGS_REL: &str = ".claude/settings.json";

/// Every event forge is ever installed for. A `--standalone` install uses
/// only PreToolUse and SubagentStop (`dispatch.rs::run_hook_standalone`);
/// a full install uses all five and delegates the non-native events to the
/// host's Python dispatcher (`dispatch.rs::delegate`).
pub(crate) const EVENTS: [&str; 5] = [
    "PreToolUse",
    "PostToolUse",
    "SessionStart",
    "SessionEnd",
    "SubagentStop",
];

/// This binary's own `--version` line; `status` compares it against each
/// installed hook's binary. The git rev is stamped by `build.rs`
/// (`git rev-parse --short HEAD`, `unknown` outside a checkout).
pub const VERSION_LINE: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (git ",
    env!("FORGE_GIT_REV"),
    ")"
);

#[derive(Subcommand)]
pub enum HooksCommand {
    /// Install or update forge's hook entries in HOST/.claude/settings.json.
    Install(InstallArgs),
    /// Remove exactly the entries `install` recognises; keep the backup.
    Uninstall(UninstallArgs),
    /// One line per event: installed or not, mode, binary, version match.
    Status(StatusArgs),
}

#[derive(Args)]
pub struct InstallArgs {
    /// The host project root (the dir containing `.claude/`).
    #[arg(long)]
    pub(crate) host: PathBuf,
    /// `FORGE_MODE` for the installed commands (warn first, enforce ~2026-09-20).
    #[arg(long, default_value = "warn")]
    pub(crate) mode: String,
    /// Only PreToolUse + SubagentStop, `FORGE_HOOK_STANDALONE=1`: no Python
    /// dispatcher, no native Bash-guard branches (`docs/routing.md`).
    #[arg(long)]
    pub(crate) standalone: bool,
    /// The forge binary the hooks call; defaults to this running binary.
    #[arg(long)]
    pub(crate) binary: Option<PathBuf>,
    /// Print the unified diff and write nothing.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Args)]
pub struct UninstallArgs {
    /// The host project root (the dir containing `.claude/`).
    #[arg(long)]
    host: PathBuf,
    /// Print the unified diff and what would be removed; write nothing.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
pub struct StatusArgs {
    /// The host project root (the dir containing `.claude/`).
    #[arg(long)]
    host: PathBuf,
}

pub fn run(action: HooksCommand) -> Result<u8> {
    match action {
        HooksCommand::Install(args) => install(&args),
        HooksCommand::Uninstall(args) => uninstall(&args),
        HooksCommand::Status(args) => status(&args),
    }
}

// ── entry construction and recognition ───────────────────────────────────

fn events_for(standalone: bool) -> Vec<&'static str> {
    if standalone {
        vec!["PreToolUse", "SubagentStop"]
    } else {
        EVENTS.to_vec()
    }
}

fn timeout_for(event: &str) -> u64 {
    match event {
        "PreToolUse" => 3,
        "SubagentStop" => 5,
        _ => 6,
    }
}

/// The hook command exactly as the hand install wrote it: env prefix, then
/// the binary, then `hook <Event>`.
fn hook_command(binary: &str, mode: &str, standalone: bool, event: &str) -> String {
    let mut command = format!("FORGE_MODE={mode}");
    if standalone {
        command.push_str(" FORGE_HOOK_STANDALONE=1");
    }
    command.push_str(&format!(" {binary} hook {event}"));
    command
}

/// One settings `hooks.<Event>` entry for forge. PreToolUse gets the `.*`
/// matcher (forge sees every call, not just `Agent`); the other events have
/// no matcher field at all, matching the hand-installed shape.
fn forge_entry(event: &str, command: &str) -> Value {
    let hook = json!({
        "type": "command",
        "command": command,
        "timeout": timeout_for(event),
    });
    if event == "PreToolUse" {
        json!({ "matcher": ".*", "hooks": [hook] })
    } else {
        json!({ "hooks": [hook] })
    }
}

/// A parsed `... forge hook <Event>` command: what `status` reports and what
/// install/uninstall recognise. `None` for anything that is not a forge
/// hook command (no `hook` token, binary not named forge, junk before the
/// binary that is not `VAR=value`).
pub(crate) struct ParsedHook {
    pub(crate) event: String,
    pub(crate) mode: String,
    pub(crate) standalone: bool,
    pub(crate) binary: String,
}

fn parse_hook_command(command: &str) -> Option<ParsedHook> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let i = tokens.iter().position(|t| *t == "hook")?;
    if i == 0 || i + 1 >= tokens.len() {
        return None;
    }
    let binary = tokens[i - 1];
    if Path::new(binary).file_name()?.to_str()? != "forge" {
        return None;
    }
    for token in &tokens[..i - 1] {
        if !token.contains('=') {
            return None;
        }
    }
    let env = &tokens[..i - 1];
    let mode = env
        .iter()
        .find_map(|t| t.strip_prefix("FORGE_MODE="))
        .unwrap_or("enforce")
        .to_string();
    let standalone = env.contains(&"FORGE_HOOK_STANDALONE=1");
    Some(ParsedHook {
        event: tokens[i + 1].to_string(),
        mode,
        standalone,
        binary: binary.to_string(),
    })
}

/// Every forge hook command in one entry, parsed. An entry with no forge
/// command yields an empty vec and is never touched.
pub(crate) fn entry_parsed_hooks(entry: &Value) -> Vec<ParsedHook> {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter_map(|hook| hook.get("command").and_then(Value::as_str))
        .filter_map(parse_hook_command)
        .filter(|parsed| EVENTS.contains(&parsed.event.as_str()))
        .collect()
}

// ── settings load / save ─────────────────────────────────────────────────

pub(crate) fn settings_path(host: &Path) -> PathBuf {
    host.join(SETTINGS_REL)
}

fn backup_path(host: &Path) -> PathBuf {
    host.join(".claude").join(BACKUP_FILE)
}

/// `(raw text, parsed)`; `(None, {})` when the file does not exist yet.
/// Unparsable JSON is an error, never a silent reset to `{}`.
pub(crate) fn load_settings(host: &Path) -> Result<(Option<String>, Value)> {
    let path = settings_path(host);
    let Some(text) = std::fs::read_to_string(&path).ok() else {
        return Ok((None, json!({})));
    };
    let value: Value = serde_json::from_str(&text)
        .with_context(|| format!("parsing {} as JSON", path.display()))?;
    if !value.is_object() {
        bail!("{} is JSON but not an object", path.display());
    }
    Ok((Some(text), value))
}

fn serialize(settings: &Value) -> String {
    let mut text = serde_json::to_string_pretty(settings).expect("Value serializes");
    text.push('\n');
    text
}

/// Copy the current settings to the backup, once: an existing backup is
/// never overwritten, so it always holds the pre-forge state however many
/// times install is re-run with a different mode or binary.
fn backup_once(host: &Path) -> Result<()> {
    let settings = settings_path(host);
    let backup = backup_path(host);
    if !settings.is_file() || backup.exists() {
        return Ok(());
    }
    let parent = backup
        .parent()
        .context("the backup path always has a parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    std::fs::copy(&settings, &backup)
        .with_context(|| format!("copying {} to {}", settings.display(), backup.display()))?;
    Ok(())
}

/// Temp file + rename in the target directory: a half-written
/// settings.json must never be what the harness reads.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let parent = path.parent().context("path always has a parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("settings"),
        std::process::id()
    ));
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}

// ── install ──────────────────────────────────────────────────────────────

pub(crate) fn install(args: &InstallArgs) -> Result<u8> {
    let mode = args.mode.as_str();
    if mode != "warn" && mode != "enforce" {
        bail!("--mode must be warn or enforce, got {mode:?}");
    }
    let binary = resolve_binary(&args.binary)?;
    let (old_text, mut settings) = load_settings(&args.host)?;
    let new_text = install_into(&mut settings, &binary, mode, args.standalone)?;
    if old_text.as_deref() == Some(new_text.as_str()) {
        println!(
            "forge hooks: already installed (mode={mode}, standalone={}, binary={}); nothing to do",
            args.standalone,
            binary.display()
        );
        return Ok(0);
    }
    if args.dry_run {
        print!(
            "{}",
            unified_diff(old_text.as_deref().unwrap_or(""), &new_text)
        );
        return Ok(0);
    }
    backup_once(&args.host)?;
    write_atomic(&settings_path(&args.host), &new_text)?;
    println!(
        "forge hooks: installed {} entries (mode={mode}, standalone={}, binary={}) in {}",
        events_for(args.standalone).len(),
        args.standalone,
        binary.display(),
        settings_path(&args.host).display()
    );
    Ok(0)
}

/// `--binary` if given (made absolute against the cwd), else the absolute
/// path of the running executable.
fn resolve_binary(binary: &Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = binary {
        if path.is_absolute() {
            return Ok(path.clone());
        }
        let cwd = std::env::current_dir().context("resolving --binary against the cwd")?;
        return Ok(cwd.join(path));
    }
    std::env::current_exe().context("resolving the running forge binary")
}

/// Upserts exactly one forge entry per event into `settings`, preserving
/// everything else, and returns the serialized result.
fn install_into(
    settings: &mut Value,
    binary: &Path,
    mode: &str,
    standalone: bool,
) -> Result<String> {
    let binary = binary.display().to_string();
    let root = settings
        .as_object_mut()
        .context("settings.json must be a JSON object")?;
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context(r#""hooks" must be a JSON object"#)?;
    for event in events_for(standalone) {
        let command = hook_command(&binary, mode, standalone, event);
        let list = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| format!(r#""hooks.{event}" must be a JSON array"#))?;
        // Replace the first recognised entry in place (key order preserved),
        // drop any duplicates a hand edit left, add one if none exists.
        let mut replaced = false;
        let mut i = 0;
        while i < list.len() {
            let is_ours = entry_parsed_hooks(&list[i])
                .iter()
                .any(|parsed| parsed.event == event);
            if !is_ours {
                i += 1;
            } else if replaced {
                list.remove(i);
            } else {
                list[i] = forge_entry(event, &command);
                replaced = true;
                i += 1;
            }
        }
        if !replaced {
            list.push(forge_entry(event, &command));
        }
    }
    Ok(serialize(settings))
}

// ── uninstall ────────────────────────────────────────────────────────────

fn uninstall(args: &UninstallArgs) -> Result<u8> {
    let (old_text, mut settings) = load_settings(&args.host)?;
    let removed = uninstall_from(&mut settings)?;
    if removed.is_empty() {
        println!("forge hooks: no forge hook entries found; nothing to do");
        return Ok(0);
    }
    let new_text = serialize(&settings);
    if args.dry_run {
        print!(
            "{}",
            unified_diff(old_text.as_deref().unwrap_or(""), &new_text)
        );
        for line in &removed {
            println!("would remove {line}");
        }
        return Ok(0);
    }
    write_atomic(&settings_path(&args.host), &new_text)?;
    for line in &removed {
        println!("removed {line}");
    }
    println!(
        "forge hooks: backup (if any) left in place at {}",
        backup_path(&args.host).display()
    );
    Ok(0)
}

/// Removes every forge entry (all five events, whatever the install flags
/// were), dropping event arrays and the `hooks` object when they empty out,
/// so a host without prior hooks returns to its exact pre-install shape.
fn uninstall_from(settings: &mut Value) -> Result<Vec<String>> {
    let mut removed = Vec::new();
    let Some(root) = settings.as_object_mut() else {
        bail!("settings.json must be a JSON object");
    };
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return Ok(removed);
    };
    for event in EVENTS {
        let Some(list) = hooks.get_mut(event).and_then(Value::as_array_mut) else {
            continue;
        };
        let mut i = 0;
        while i < list.len() {
            let ours: Vec<String> = entry_parsed_hooks(&list[i])
                .iter()
                .map(|parsed| parsed.event.clone())
                .collect();
            if ours.iter().any(|e| e == event) {
                let commands: Vec<String> = list[i]
                    .get("hooks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|h| h.get("command").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect();
                removed.push(format!("{event} entry ({})", commands.join("; ")));
                list.remove(i);
            } else {
                i += 1;
            }
        }
        if list.is_empty() {
            hooks.remove(event);
        }
    }
    if hooks.is_empty() {
        root.remove("hooks");
    }
    Ok(removed)
}

// ── status ───────────────────────────────────────────────────────────────

fn status(args: &StatusArgs) -> Result<u8> {
    let (_, settings) = load_settings(&args.host)?;
    let mut missing_binary = false;
    for event in EVENTS {
        let entries = settings
            .get("hooks")
            .and_then(Value::as_object)
            .and_then(|hooks| hooks.get(event))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .flat_map(entry_parsed_hooks)
                    .filter(|parsed| parsed.event == event)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        status_line(event, &entries, &mut missing_binary);
    }
    if missing_binary {
        eprintln!("forge hooks status: an installed entry points at a missing binary");
        return Ok(1);
    }
    Ok(0)
}

/// One line per event; flips `missing_binary` when an installed hook's
/// binary is gone (that is the one condition worth failing on -- the hook
/// would silently do nothing).
fn status_line(event: &str, installed: &[ParsedHook], missing_binary: &mut bool) {
    let Some(parsed) = installed.first() else {
        println!("{event}: not installed");
        return;
    };
    let exists = Path::new(&parsed.binary).is_file();
    if !exists {
        *missing_binary = true;
    }
    let version = if exists {
        installed_binary_version(&parsed.binary)
    } else {
        "n/a (binary missing)".to_string()
    };
    println!(
        "{event}: installed mode={} standalone={} binary={} exists={} version={}",
        parsed.mode, parsed.standalone, parsed.binary, exists, version
    );
    if installed.len() > 1 {
        println!(
            "{event}: note: {} extra forge entr{} (a re-run of install collapses them)",
            installed.len() - 1,
            if installed.len() > 2 { "ies" } else { "y" }
        );
    }
}

/// Runs the installed binary's `--version` and compares it with this
/// running binary's line. A mismatch is reported, not fatal -- the hook
/// still works; the human decides whether to re-install or re-deploy.
fn installed_binary_version(binary: &str) -> String {
    match std::process::Command::new(binary).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if versions_match(&line) {
                "match".to_string()
            } else {
                format!("MISMATCH (installed: {line}, running: {VERSION_LINE})")
            }
        }
        Ok(output) => format!("unreadable (exit {})", output.status),
        Err(err) => format!("unreadable ({err})"),
    }
}

/// `forge --version` prints `<name> <version line>`; accept either the
/// prefixed form (a real forge) or the bare line, so the comparison is on
/// the version content, not clap's output format.
pub(crate) fn versions_match(installed_output: &str) -> bool {
    installed_output.strip_prefix("forge ") == Some(VERSION_LINE)
        || installed_output == VERSION_LINE
}

// ── unified diff (for --dry-run) ─────────────────────────────────────────

/// A small line-based unified diff: LCS over lines, 3 context lines per
/// hunk. Settings files are tens of lines, so the quadratic table is fine
/// and no diff dependency enters Cargo.toml. Cosmetic tooling only -- the
/// installer's correctness never depends on the diff.
fn unified_diff(old: &str, new: &str) -> String {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    // lcs[i][j]: LCS length of a[i..] and b[j..].
    let mut lcs = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut ops: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            ops.push((' ', a[i]));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            ops.push(('-', a[i]));
            i += 1;
        } else {
            ops.push(('+', b[j]));
            j += 1;
        }
    }
    while i < a.len() {
        ops.push(('-', a[i]));
        i += 1;
    }
    while j < b.len() {
        ops.push(('+', b[j]));
        j += 1;
    }
    render_hunks(&ops)
}

/// Groups the ops into hunks of `CONTEXT` unchanged lines around each run
/// of changes and renders `@@ -a,b +c,d @@` headers with ` `/`-`/`+` lines.
fn render_hunks(ops: &[(char, &str)]) -> String {
    const CONTEXT: usize = 3;
    let is_change = |op: &(char, &str)| op.0 != ' ';
    let changes: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| is_change(op))
        .map(|(k, _)| k)
        .collect();
    let mut out = String::new();
    let mut group = 0;
    while group < changes.len() {
        let mut last = group;
        while last + 1 < changes.len() && changes[last + 1] - changes[last] <= 2 * CONTEXT + 1 {
            last += 1;
        }
        let first_idx = changes[group].saturating_sub(CONTEXT);
        let end_idx = (changes[last] + 1 + CONTEXT).min(ops.len());
        let old_start = ops[..first_idx].iter().filter(|op| op.0 != '+').count();
        let new_start = ops[..first_idx].iter().filter(|op| op.0 != '-').count();
        let old_count = ops[first_idx..end_idx]
            .iter()
            .filter(|op| op.0 != '+')
            .count();
        let new_count = ops[first_idx..end_idx]
            .iter()
            .filter(|op| op.0 != '-')
            .count();
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            hunk_range(old_start, old_count),
            hunk_range(new_start, new_count)
        ));
        for (marker, line) in &ops[first_idx..end_idx] {
            out.push_str(&format!("{marker}{line}\n"));
        }
        group = last + 1;
    }
    out
}

/// The `start,count` half of a hunk header; `,1` is omitted for one-line
/// ranges and a zero-count range prints the line *before* it, per diff
/// convention.
fn hunk_range(start: usize, count: usize) -> String {
    match count {
        0 => format!("{},0", start),
        1 => format!("{}", start + 1),
        n => format!("{},{}", start + 1, n),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScratchDir(PathBuf);
    impl ScratchDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "forge-hooks-install-test-{tag}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            ScratchDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        fn write_settings(&self, text: &str) {
            let dir = self.0.join(".claude");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("settings.json"), text).unwrap();
        }
        fn settings_text(&self) -> String {
            std::fs::read_to_string(self.0.join(".claude").join("settings.json")).unwrap()
        }
        fn settings_value(&self) -> Value {
            serde_json::from_str(&self.settings_text()).unwrap()
        }
    }
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The REAL LLM settings shape (from the install snippet that lived in
    /// `docs/routing.md` until this PR replaced it with the command): a
    /// matcher-`Agent` PreToolUse entry and a bare SubagentStop entry, both
    /// with bare `forge hook ...` commands -- what the hand install wrote.
    const LLM_SETTINGS_SHAPE: &str = r#"{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Agent",
        "hooks": [{"type": "command", "command": "forge hook PreToolUse"}]
      }
    ],
    "SubagentStop": [
      {
        "hooks": [{"type": "command", "command": "forge hook SubagentStop"}]
      }
    ]
  }
}"#;

    const BIN: &str = "/opt/forge/bin/forge";

    fn install_args(host: &Path, mode: &str, standalone: bool) -> InstallArgs {
        InstallArgs {
            host: host.to_path_buf(),
            mode: mode.to_string(),
            standalone,
            binary: Some(PathBuf::from(BIN)),
            dry_run: false,
        }
    }

    fn commands_for(settings: &Value, event: &str) -> Vec<String> {
        settings["hooks"][event]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|entry| {
                entry["hooks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|h| h["command"].as_str().unwrap().to_string())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn install_into_an_empty_dir_creates_the_two_standalone_entries() {
        let scratch = ScratchDir::new("empty");
        install(&install_args(scratch.path(), "warn", true)).unwrap();
        let settings = scratch.settings_value();
        let mut hook_events: Vec<&String> = settings["hooks"].as_object().unwrap().keys().collect();
        hook_events.sort();
        assert_eq!(hook_events, vec!["PreToolUse", "SubagentStop"]);
        let pre = &settings["hooks"]["PreToolUse"][0];
        assert_eq!(pre["matcher"], ".*");
        assert_eq!(pre["hooks"][0]["timeout"], 3);
        assert_eq!(
            pre["hooks"][0]["command"],
            format!("FORGE_MODE=warn FORGE_HOOK_STANDALONE=1 {BIN} hook PreToolUse")
        );
        let sub = &settings["hooks"]["SubagentStop"][0];
        assert!(sub.get("matcher").is_none());
        assert_eq!(sub["hooks"][0]["timeout"], 5);
        assert_eq!(
            sub["hooks"][0]["command"],
            format!("FORGE_MODE=warn FORGE_HOOK_STANDALONE=1 {BIN} hook SubagentStop")
        );
        // Nothing existed to back up.
        assert!(!backup_path(scratch.path()).exists());
    }

    #[test]
    fn full_install_covers_five_events_without_standalone_env() {
        let scratch = ScratchDir::new("full");
        install(&install_args(scratch.path(), "enforce", false)).unwrap();
        let settings = scratch.settings_value();
        let mut events: Vec<&String> = settings["hooks"].as_object().unwrap().keys().collect();
        events.sort();
        assert_eq!(
            events,
            vec![
                "PostToolUse",
                "PreToolUse",
                "SessionEnd",
                "SessionStart",
                "SubagentStop"
            ]
        );
        for event in EVENTS {
            assert_eq!(
                commands_for(&settings, event),
                vec![format!("FORGE_MODE=enforce {BIN} hook {event}")],
                "{event}"
            );
        }
    }

    #[test]
    fn install_merges_beside_unrelated_entries_and_preserves_them() {
        let scratch = ScratchDir::new("merge");
        scratch.write_settings(
            &serde_json::to_string_pretty(&json!({
                "model": "opus",
                "hooks": {
                    "PreToolUse": [
                        {"matcher": "Bash", "hooks": [{"type": "command", "command": "lint.sh"}]}
                    ],
                    "PostToolUse": [
                        {"hooks": [{"type": "command", "command": "notify.sh"}]}
                    ]
                }
            }))
            .unwrap(),
        );
        install(&install_args(scratch.path(), "warn", true)).unwrap();
        let settings = scratch.settings_value();
        // Unrelated entries untouched, forge appended after them.
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "lint.sh"
        );
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
        assert_eq!(
            settings["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            "notify.sh"
        );
        // Top-level keys preserved, order included.
        let keys: Vec<&String> = settings.as_object().unwrap().keys().collect();
        assert_eq!(keys.first().copied(), Some(&"model".to_string()));
    }

    #[test]
    fn second_install_is_a_byte_for_byte_no_op() {
        let scratch = ScratchDir::new("noop");
        install(&install_args(scratch.path(), "warn", true)).unwrap();
        let first = scratch.settings_text();
        install(&install_args(scratch.path(), "warn", true)).unwrap();
        assert_eq!(scratch.settings_text(), first);
    }

    #[test]
    fn mode_flip_rewrites_only_the_forge_entries() {
        let scratch = ScratchDir::new("flip");
        scratch.write_settings(
            &serde_json::to_string_pretty(&json!({
                "hooks": {
                    "PreToolUse": [
                        {"matcher": "Bash", "hooks": [{"type": "command", "command": "lint.sh"}]}
                    ]
                }
            }))
            .unwrap(),
        );
        install(&install_args(scratch.path(), "warn", true)).unwrap();
        install(&install_args(scratch.path(), "enforce", true)).unwrap();
        let settings = scratch.settings_value();
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "lint.sh",
            "the unrelated entry keeps its place and its text"
        );
        assert_eq!(
            commands_for(&settings, "PreToolUse"),
            vec![
                "lint.sh".to_string(),
                format!("FORGE_MODE=enforce FORGE_HOOK_STANDALONE=1 {BIN} hook PreToolUse")
            ]
        );
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn uninstall_restores_the_pre_forge_entry_set_exactly() {
        let scratch = ScratchDir::new("uninstall");
        let original = serde_json::to_string_pretty(&json!({
            "model": "opus",
            "hooks": {
                "PreToolUse": [
                    {"matcher": "Bash", "hooks": [{"type": "command", "command": "lint.sh"}]}
                ]
            }
        }))
        .unwrap()
            + "\n";
        scratch.write_settings(&original);
        install(&install_args(scratch.path(), "warn", false)).unwrap();
        uninstall(&UninstallArgs {
            host: scratch.path().to_path_buf(),
            dry_run: false,
        })
        .unwrap();
        assert_eq!(scratch.settings_text(), original);
    }

    /// Uninstall never deletes the settings file itself (a host's explicit
    /// `{}` is not forge's to remove); a fresh install's uninstall leaves
    /// exactly the empty object.
    #[test]
    fn uninstall_of_a_fresh_install_leaves_an_empty_object() {
        let scratch = ScratchDir::new("uninstall-fresh");
        install(&install_args(scratch.path(), "warn", true)).unwrap();
        uninstall(&UninstallArgs {
            host: scratch.path().to_path_buf(),
            dry_run: false,
        })
        .unwrap();
        assert_eq!(scratch.settings_text(), "{}\n");
        assert!(!backup_path(scratch.path()).exists());
    }

    #[test]
    fn backup_is_written_once_and_never_overwritten() {
        let scratch = ScratchDir::new("backup");
        scratch.write_settings(r#"{"model": "opus"}"#);
        install(&install_args(scratch.path(), "warn", true)).unwrap();
        let backup = backup_path(scratch.path());
        assert!(backup.exists());
        let first_backup = std::fs::read_to_string(&backup).unwrap();
        assert_eq!(first_backup, "{\"model\": \"opus\"}");
        // Re-install with a different mode: the settings change, the first
        // backup does not.
        scratch.write_settings(r#"{"model": "sonnet"}"#);
        install(&install_args(scratch.path(), "enforce", true)).unwrap();
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), first_backup);
    }

    #[test]
    fn dry_run_writes_nothing_not_even_a_backup() {
        let scratch = ScratchDir::new("dry");
        scratch.write_settings(r#"{"model": "opus"}"#);
        install(&InstallArgs {
            dry_run: true,
            ..install_args(scratch.path(), "warn", true)
        })
        .unwrap();
        assert_eq!(scratch.settings_text(), "{\"model\": \"opus\"}");
        assert!(!backup_path(scratch.path()).exists());
    }

    #[test]
    fn malformed_json_is_an_error_not_a_reset() {
        let scratch = ScratchDir::new("malformed");
        scratch.write_settings("{ not json");
        assert!(install(&install_args(scratch.path(), "warn", true)).is_err());
        assert_eq!(scratch.settings_text(), "{ not json", "nothing was written");
    }

    #[test]
    fn a_bad_mode_is_rejected_before_anything_is_read() {
        let scratch = ScratchDir::new("badmode");
        assert!(install(&install_args(scratch.path(), "sometimes", true)).is_err());
        assert!(!settings_path(scratch.path()).exists());
    }

    /// The real LLM settings shape: matcher-`Agent` PreToolUse and bare
    /// `forge hook` commands. Install must recognise both as forge's own
    /// (replace in place, no duplicates), not add beside them.
    #[test]
    fn install_over_the_real_llm_settings_shape_replaces_in_place() {
        let scratch = ScratchDir::new("llm-shape");
        scratch.write_settings(LLM_SETTINGS_SHAPE);
        install(&install_args(scratch.path(), "warn", true)).unwrap();
        let settings = scratch.settings_value();
        assert_eq!(settings["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(
            settings["hooks"]["SubagentStop"].as_array().unwrap().len(),
            1
        );
        assert_eq!(settings["hooks"]["PreToolUse"][0]["matcher"], ".*");
        assert_eq!(
            commands_for(&settings, "PreToolUse"),
            vec![format!(
                "FORGE_MODE=warn FORGE_HOOK_STANDALONE=1 {BIN} hook PreToolUse"
            )]
        );
    }

    #[test]
    fn status_reports_installed_and_missing_binary_as_exit_one() {
        let scratch = ScratchDir::new("status");
        install(&install_args(scratch.path(), "warn", true)).unwrap();
        // The fake binary path does not exist: status must say so and fail.
        let code = status(&StatusArgs {
            host: scratch.path().to_path_buf(),
        })
        .unwrap();
        assert_eq!(code, 1);
    }

    #[test]
    fn status_on_a_host_without_settings_reports_not_installed() {
        let scratch = ScratchDir::new("status-empty");
        let code = status(&StatusArgs {
            host: scratch.path().to_path_buf(),
        })
        .unwrap();
        assert_eq!(code, 0);
    }

    #[test]
    fn parse_hook_command_accepts_the_hand_install_and_bare_forms() {
        let hand = parse_hook_command(
            "FORGE_MODE=warn FORGE_HOOK_STANDALONE=1 /home/tim/.cargo/bin/forge hook PreToolUse",
        )
        .unwrap();
        assert_eq!(hand.event, "PreToolUse");
        assert_eq!(hand.mode, "warn");
        assert!(hand.standalone);
        assert_eq!(hand.binary, "/home/tim/.cargo/bin/forge");
        let bare = parse_hook_command("forge hook SubagentStop").unwrap();
        assert_eq!(bare.mode, "enforce", "hand installs omitted FORGE_MODE");
        assert!(!bare.standalone);
        assert!(parse_hook_command("lint.sh hook PreToolUse").is_none());
        assert!(parse_hook_command("forge hook").is_none());
    }

    #[test]
    fn unified_diff_renders_hunks() {
        let diff = unified_diff("a\nb\nc\n", "a\nx\nc\n");
        assert_eq!(diff, "@@ -1,3 +1,3 @@\n a\n-b\n+x\n c\n");
        assert_eq!(unified_diff("", ""), "");
    }

    #[test]
    fn versions_match_accepts_the_clap_prefixed_line() {
        assert!(versions_match(&format!("forge {VERSION_LINE}")));
        assert!(versions_match(VERSION_LINE));
        assert!(!versions_match("forge 0.0.0 (git deadbee)"));
    }
}
