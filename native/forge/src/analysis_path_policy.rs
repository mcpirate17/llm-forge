//! Lexical repository path policies; deliberately never opens the selected files.
use anyhow::{ensure, Context, Result};
use clap::{Args, ValueEnum};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone, Copy, ValueEnum)]
pub(super) enum Check {
    RootJunk,
    JsonInNotes,
}

impl Check {
    fn id(self) -> &'static str {
        match self {
            Self::RootJunk => "root-junk",
            Self::JsonInNotes => "json-in-notes",
        }
    }
}

#[derive(Args)]
pub(super) struct PathArgs {
    #[arg(long, value_enum)]
    check: Check,
    /// Report the native protocol version without reading any project configuration.
    #[arg(long)]
    tool_version: bool,
    #[arg(num_args = 0..=4096)]
    files: Vec<String>,
}

// PurePosixPath removes repeated separators and '.' but preserves '..' and
// backslashes. Absolute inputs never have a relative root or notes parent.
fn parts(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect()
}

fn root_junk(path: &str) -> bool {
    let parts = parts(path);
    if path.starts_with('/') || parts.len() != 1 {
        return false;
    }
    let name = parts[0];
    if name.contains("DO_NOT_DELETE") {
        return false;
    }
    (name.ends_with(".md") && (name.contains("PLAN") || name.contains("HANDOFF")))
        || name.ends_with(".log")
        || name == "metrics.jsonl"
        || name.starts_with("unused.")
}

fn notes_data(path: &str, notes: &str) -> bool {
    let parts = parts(path);
    if path.starts_with('/') || parts.len() < 2 {
        return false;
    }
    let name = parts[parts.len() - 1];
    parts[..parts.len() - 1] == self::parts(notes)
        && [".json", ".jsonl", ".csv"]
            .iter()
            .any(|suffix| name.len() > suffix.len() && name.ends_with(suffix))
}

fn admit(files: &[String]) -> Result<()> {
    ensure!(
        files.len() <= 4096,
        "path policy accepts at most 4096 paths"
    );
    let mut total = 0;
    for path in files {
        ensure!(path.len() <= 4096, "path policy path exceeds 4096 bytes");
        ensure!(!path.contains('\0'), "path policy path contains NUL");
        total += path.len();
        ensure!(total <= 1024 * 1024, "path policy input exceeds 1 MiB");
    }
    Ok(())
}

fn configured_notes(root: &Path) -> Result<String> {
    let notes = conductor_native::project_paths::notes_root(root).map_err(anyhow::Error::msg)?;
    let relative = notes
        .strip_prefix(root)
        .context("notes root must remain inside selected root")?;
    Ok(relative
        .to_str()
        .context("notes root must be UTF-8")?
        .to_owned())
}

pub(super) fn evaluate(root: &Path, args: PathArgs) -> Result<(Value, u8)> {
    admit(&args.files)?;
    if args.tool_version {
        ensure!(args.files.is_empty(), "tool-version does not accept paths");
        return Ok((
            json!({"tool_version":format!("forge-native-path-policy 1 {} {}",
            env!("CARGO_PKG_VERSION"), args.check.id())}),
            0,
        ));
    }
    let notes = match args.check {
        Check::RootJunk => String::new(),
        Check::JsonInNotes => configured_notes(root)?,
    };
    let mut findings = Vec::new();
    for path in &args.files {
        let message = match args.check {
            Check::RootJunk if root_junk(path) => format!(
                "forbidden at repo root: {path} — move to tasks/ or research/reports/"),
            Check::JsonInNotes if notes_data(path, &notes) => format!(
                "data file at the top level of {notes}/: {path} — research/data/ for persistent inputs, research/reports/ or tasks/audit/ for tool output"),
            _ => continue,
        };
        findings.push(json!({"rule":args.check.id(),"path":path,"message":message}));
    }
    let code = u8::from(!findings.is_empty());
    Ok((
        json!({"schema_version":1,"authority":"native-path-policy","check":args.check.id(),
        "files":args.files,"findings":findings,"finding_count":findings.len(),"truncated":false}),
        code,
    ))
}
