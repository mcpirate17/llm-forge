//! One bounded, non-authoritative task briefing without an interpreter, model
//! launch, network request, or mutation of claims/mailbox/indexes.
use crate::recall::{self, snippet, Query};
use crate::{
    active_state::{self, ActiveState},
    notes_index, ownership,
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

#[derive(Args)]
pub struct BriefArgs {
    #[arg(long, default_value = ".")]
    host: PathBuf,
    #[arg(long, allow_hyphen_values = true)]
    task: String,
    /// Relative host paths whose active ownership overlaps must be retained.
    #[arg(long, num_args = 0..)]
    paths: Vec<String>,
    /// Existing mailbox identity; defaults to A2A_AGENT_NAME when set.
    #[arg(long)]
    identity: Option<String>,
    /// Task-bound offline query vector JSON. Required for semantic indexes.
    #[arg(long)]
    query: Option<PathBuf>,
    /// Schema-3 memory JSONL index; no implicit rebuild or network lookup.
    #[arg(long, requires = "query")]
    memory_index: Option<PathBuf>,
    /// Explicit mapped snapshot; defaults to MEMORY_INDEX.sidecar.
    #[arg(long, requires = "memory_index")]
    memory_sidecar: Option<PathBuf>,
    /// Schema-2 knowledge-card JSON index, sharing the same query vector.
    #[arg(long, requires = "query")]
    cards_index: Option<PathBuf>,
    /// Character budget of the brief text (the JSON envelope is additional).
    #[arg(long, default_value_t = 3500)]
    max_chars: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Serialize)]
pub struct Brief {
    schema_version: u32,
    authority: &'static str,
    characters: usize,
    omitted_items: usize,
    pub text: String,
}

fn validate(task: &str, paths: &[String], max_chars: usize) -> Result<()> {
    ensure!(
        !task.trim().is_empty() && task.len() <= 4096 && task.chars().count() <= 1024,
        "task must contain 1..1024 characters and at most 4096 bytes"
    );
    ensure!(
        (1024..=16384).contains(&max_chars),
        "max-chars must be in 1024..16384"
    );
    ensure!(paths.len() <= 64, "at most 64 paths may be requested");
    for path in paths {
        ensure!(
            !path.is_empty()
                && path.len() <= 1024
                && !path.chars().any(char::is_control)
                && !path.contains('\\')
                && !path.contains("//")
                && !path.ends_with('/')
                && path.split('/').all(|p| p != "." && p != "..")
                && Path::new(path)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "paths must be normalized relative host paths"
        );
    }
    Ok(())
}

fn required(task: &str, paths: &[String], state: &ActiveState) -> String {
    let mandates: Vec<_> = state
        .standing_mandates
        .iter()
        .map(|m| m.split(':').next().unwrap_or("").trim())
        .filter(|m| !m.is_empty())
        .collect();
    let mut lines = vec![
        format!("TASK: {}", task.trim()),
        format!(
            "MANDATES: {}",
            if mandates.is_empty() {
                "none".into()
            } else {
                mandates.join(", ")
            }
        ),
    ];
    if paths.is_empty() {
        lines.push("CLAIMS: no paths requested".into());
    } else {
        let claims: Vec<_> = state
            .active_claims
            .iter()
            .filter(|claim| {
                paths.iter().any(|path| {
                    claim
                        .paths
                        .iter()
                        .any(|held| ownership::paths_overlap(path, held))
                })
            })
            .collect();
        lines.push(format!(
            "CLAIMS: {} active overlaps (informational; grants no authority)",
            claims.len()
        ));
        for claim in claims {
            lines.push(format!(
                "- {} owner={} paths={} expires={}",
                claim.claim_id,
                claim.owner,
                claim.paths.join(","),
                claim.expires_at
            ));
        }
    }
    lines.join("\n")
}

fn inbox(root: &Path, identity: Option<&str>) -> Result<String> {
    let Some(identity) = identity else {
        return Ok("INBOX: no identity requested".into());
    };
    match crate::mailbox_preview::brief(root, identity)? {
        Some(value) => Ok(format!(
            "INBOX (preview only): {}",
            serde_json::to_string(&value)?
        )),
        None => Ok(format!("INBOX: no existing mailbox for {identity}")),
    }
}

fn notes(root: &Path, task: &str) -> Result<Vec<String>> {
    let path = conductor_native::project_paths::notes_db(root).map_err(anyhow::Error::msg)?;
    let Some(conn) = notes_index::open_search(&path)? else {
        return Ok(vec!["TASKS: notes index missing".into()]);
    };
    let hits = notes_index::search_filtered(&conn, task, 3, Some("tasks"))?;
    if hits.is_empty() {
        return Ok(vec!["TASKS: no lexical matches".into()]);
    }
    Ok(hits
        .into_iter()
        .map(|hit| {
            format!(
                "TASK: {}: {} ({})",
                snippet(&hit.title, 120),
                snippet(&hit.snippet, 240),
                snippet(&hit.path, 240)
            )
        })
        .collect())
}

fn compose(required: String, optional: Vec<String>, max_chars: usize) -> Result<Brief> {
    const RESERVE: usize = 80;
    ensure!(required.chars().count() + RESERVE <= max_chars, "task, mandates and active claims exceed brief budget; narrow paths or increase --max-chars");
    let mut text = required;
    let mut used = text.chars().count();
    let mut omitted = 0;
    let mut seen = BTreeSet::new();
    for item in optional {
        if !seen.insert(item.clone()) {
            continue;
        }
        let size = item.chars().count() + 1;
        if used + size + RESERVE > max_chars {
            omitted += 1;
            continue;
        }
        text.push('\n');
        text.push_str(&item);
        used += size;
    }
    if omitted != 0 {
        text.push_str(&format!(
            "\nOMITTED: {omitted} context items; retrieve sources explicitly."
        ));
    }
    Ok(Brief {
        schema_version: 1,
        authority: "non-authoritative-session-brief",
        characters: text.chars().count(),
        omitted_items: omitted,
        text,
    })
}

fn optional(
    root: &Path,
    task: &str,
    state: &ActiveState,
    identity: Option<&str>,
) -> Result<Vec<String>> {
    let mut lines = vec![inbox(root, identity)?];
    lines.extend(
        state
            .active_headings
            .iter()
            .map(|heading| format!("HEADING: {}", snippet(heading, 240))),
    );
    lines.extend(notes(root, task)?);
    Ok(lines)
}

/// Read current workspace observations under a caller-selected root and identity.
/// This function has no environment-derived identity or implicit semantic lookup.
pub fn capture(
    root: &Path,
    task: &str,
    paths: &[String],
    identity: Option<&str>,
    max_chars: usize,
) -> Result<Brief> {
    validate(task, paths, max_chars)?;
    let state = active_state::generate_active_state(root)?;
    compose(
        required(task, paths, &state),
        optional(root, task, &state, identity)?,
        max_chars,
    )
}

/// Handoff uses the same brief with a state snapshot it already captured.
pub fn for_handoff(
    root: &Path,
    task: &str,
    paths: &[String],
    state: &ActiveState,
    identity: Option<&str>,
) -> Result<String> {
    validate(task, paths, 3500)?;
    Ok(compose(
        required(task, paths, state),
        optional(root, task, state, identity)?,
        3500,
    )?
    .text)
}

pub fn run(args: BriefArgs) -> Result<u8> {
    validate(&args.task, &args.paths, args.max_chars)?;
    ensure!(
        args.query.is_none() || args.memory_index.is_some() || args.cards_index.is_some(),
        "--query requires a semantic index"
    );
    let root = std::fs::canonicalize(&args.host).context("brief host")?;
    let identity = match args.identity {
        Some(identity) => Some(identity),
        None => std::env::var_os("A2A_AGENT_NAME")
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("A2A_AGENT_NAME is not UTF-8"))
            })
            .transpose()?,
    };
    let state = active_state::generate_active_state(&root)?;
    let mut lines = optional(&root, &args.task, &state, identity.as_deref())?;
    if let Some(path) = args.query {
        let query = Query::load(&path, &args.task)?;
        if let Some(path) = args.memory_index {
            lines.extend(
                recall::memory(&path, args.memory_sidecar.as_deref(), &query)?
                    .into_iter()
                    .map(|hit| format!("MEMORY: {hit}")),
            );
        }
        if let Some(path) = args.cards_index {
            lines.extend(
                recall::cards(&path, &query)?
                    .into_iter()
                    .map(|hit| format!("CARD: {hit}")),
            );
        }
    } else {
        lines.push("SEMANTIC: not requested (supply --query and an index)".into());
    }
    let brief = compose(
        required(&args.task, &args.paths, &state),
        lines,
        args.max_chars,
    )?;
    if args.json {
        println!("{}", serde_json::to_string(&brief)?);
    } else {
        println!("{}", brief.text);
    }
    Ok(0)
}
