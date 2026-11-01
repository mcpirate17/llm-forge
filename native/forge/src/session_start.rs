//! Native SessionStart composition. Each external stage is bounded, additive,
//! and reports failure. A host extension cannot replace the generic context.
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[path = "source_fingerprint.rs"]
mod source_fingerprint;

fn native(root: &Path, args: &[&str], timeout: u64) -> Result<Value> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(args)
        .current_dir(root)
        .env("CLAUDE_PROJECT_DIR", root);
    let out = forge::process_probe::run(&mut command, Duration::from_secs(timeout), 64 * 1024)?;
    if !out.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
    }
    ensure!(out.status.success(), "native stage exited {}", out.status);
    forge::tool_schema::parse_document(std::str::from_utf8(&out.stdout)?)
}

pub(crate) fn identity(root: &Path) -> Result<Option<String>> {
    let name = match std::env::var("A2A_AGENT_NAME") {
        Ok(name) => name,
        Err(std::env::VarError::NotUnicode(_)) => anyhow::bail!("A2A_AGENT_NAME is not UTF-8"),
        Err(std::env::VarError::NotPresent) => {
            let path = root.join(".agents/a2a/default_identity");
            match std::fs::symlink_metadata(&path) {
                Ok(_) => String::from_utf8(forge::bounded_file::read(&path, 4096)?)?
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_owned(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.into()),
            }
        }
    };
    let name = name.trim();
    conductor_native::a2a_store::validate_identity(name)?;
    Ok(Some(name.to_owned()))
}

fn freshness(root: &Path) -> Result<Option<String>> {
    if !root.join("native/forge/Cargo.toml").is_file() {
        return Ok(None);
    }
    let files = source_fingerprint::files(root)?;
    let actual = source_fingerprint::fingerprint(root, &files)?;
    Ok((actual != env!("FORGE_SOURCE_SHA256")).then(|| format!(
        "Native Forge binary differs from this checkout (built {}, current {}). Rebuild and reinstall Forge before relying on source changes.",
        env!("FORGE_SOURCE_SHA256"), actual)))
}

fn extension(root: &Path, input: &[u8]) -> Result<String> {
    let directory = std::env::var_os("PROJECT_HOOK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join(".claude/hooks/project"));
    let directory = if directory.is_absolute() {
        directory
    } else {
        root.join(directory)
    };
    // Prefer a native executable name; retain the established host extension
    // filename so existing operator-owned hooks remain additive during migration.
    let mut path = directory.join("session-start");
    if !path.try_exists()? {
        path = directory.join("session-start.sh");
    }
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e.into()),
    };
    use std::os::unix::fs::PermissionsExt;
    ensure!(
        meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        "project extension is not executable"
    );
    let mut command = Command::new(&path);
    command
        .current_dir(root)
        .env("REPO_ROOT", root)
        .env("PROJECT_DIR", root)
        .env("CLAUDE_PROJECT_DIR", root)
        .env("HOOK_DIR", root.join(".claude/hooks"));
    let out = forge::process_probe::run_with_input(
        &mut command,
        input,
        Duration::from_secs(2),
        16 * 1024,
    )?;
    if !out.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
    }
    ensure!(
        out.status.success(),
        "project extension {} exited {}",
        path.display(),
        out.status
    );
    Ok(String::from_utf8(out.stdout)?)
}

fn problem(lines: &mut Vec<String>, stage: &str, error: impl std::fmt::Display) {
    let text: String = format!("[session-start] {stage} failed: {error}")
        .chars()
        .take(1000)
        .collect();
    eprintln!("{text}");
    lines.push(text);
}

fn context(lines: &mut Vec<String>, value: &Value) {
    if let Some(text) = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        lines.push(text.chars().take(6000).collect());
    } else if let Some(text) = value["systemMessage"].as_str() {
        lines.push(text.chars().take(6000).collect());
    }
}

fn mailbox(root: &Path, name: &str, lines: &mut Vec<String>) -> String {
    match native(root, &["mailbox", "start", "--identity", name], 8) {
        Ok(result) => {
            if let Some(error) = result["flush_error"].as_str() {
                problem(lines, "mailbox retry", error);
            }
            if result["flush_status"] == 3 {
                lines.push("A2A: outbound messages remain queued for retry.".into());
            }
            let mut preview = result["preview"].clone();
            preview.sort_all_objects();
            preview.to_string()
        }
        Err(error) => {
            problem(lines, "mailbox startup", error);
            String::new()
        }
    }
}

pub fn run(root: &Path, input: &str) -> Result<Value> {
    let root = root.canonicalize()?;
    let payload = forge::tool_schema::parse_document(input)?;
    ensure!(
        payload.is_object(),
        "session start payload must be an object"
    );
    let mut lines = Vec::new();
    if let Err(error) = crate::crg_gate::start(&payload) {
        problem(&mut lines, "graph gate", error);
    }
    context(
        &mut lines,
        &crate::crg_refresh::failure_output("SessionStart", &root),
    );
    let (name, identity_valid) = match identity(&root) {
        Ok(name) => (name, true),
        Err(error) => {
            problem(&mut lines, "identity", error);
            (None, false)
        }
    };
    let summary = name
        .as_ref()
        .map(|name| mailbox(&root, name, &mut lines))
        .unwrap_or_default();
    let host = root.to_str().context("host path is not UTF-8")?;
    match native(
        &root,
        &[
            "session",
            "preamble",
            "--host",
            host,
            "--a2a-name",
            name.as_deref().unwrap_or(""),
            "--a2a-summary",
            &summary,
        ],
        2,
    ) {
        Ok(value) => context(&mut lines, &value),
        Err(error) => problem(&mut lines, "preamble", error),
    }
    if identity_valid {
        match native(
            &root,
            &[
                "handoff",
                "--host",
                host,
                "hook",
                "--identity",
                name.as_deref().unwrap_or("claude"),
            ],
            2,
        ) {
            Ok(value) => context(&mut lines, &value),
            Err(error) => problem(&mut lines, "handoff", error),
        }
    }
    match freshness(&root) {
        Ok(Some(text)) => lines.push(text),
        Ok(None) => (),
        Err(error) => problem(&mut lines, "native freshness", error),
    }
    match extension(&root, input.as_bytes()) {
        Ok(text) if !text.trim().is_empty() => lines.push(text),
        Ok(_) => (),
        Err(error) => problem(&mut lines, "project extension", error),
    }
    let answer = json!({"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":lines.join("\n\n")}});
    crate::context_telemetry::record_hook_context(
        "session-start",
        &answer,
        payload["session_id"].as_str().unwrap_or(""),
    );
    Ok(answer)
}
