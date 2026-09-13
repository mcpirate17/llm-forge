//! The compact session inject every Claude-family SessionStart attaches,
//! ported from `conductor.session_preamble` (LLM's copy is the spec: the
//! EXPOSED line still rides in `compact_state` here). The body is policy
//! preamble lines, MANDATES ids, CLAIMS count, the EXPOSED line, HEADINGS,
//! then the optional A2A compact block -- clipped to 2200 chars so the
//! inject can never crowd out the session's real context.

use crate::active_state::ActiveState;
use crate::session_policy;
use crate::workspace_hygiene;
use anyhow::Result;
use clap::Args;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const MAX_INJECT_CHARS: usize = 2200;
const MAX_A2A_CHARS: usize = 1200;
const MAX_HEADINGS: usize = 4;

pub fn compact_state(state: &ActiveState, repo: &Path) -> Result<String> {
    let policy =
        session_policy::load_session_policy(repo).map_err(|err| anyhow::anyhow!("{err}"))?;
    let mandate_ids: Vec<String> = state
        .standing_mandates
        .iter()
        .filter(|item| !item.is_empty())
        .map(|item| item.split(':').next().unwrap_or("").trim().to_owned())
        .collect();
    let heading_lines: Vec<String> = state
        .active_headings
        .iter()
        .take(MAX_HEADINGS)
        .filter_map(|heading| {
            let trimmed = heading.trim();
            (!trimmed.is_empty()).then(|| format!("- {trimmed}"))
        })
        .collect();
    let mut lines = policy.preamble;
    lines.push(format!(
        "MANDATES: {}",
        if mandate_ids.is_empty() {
            "none".to_string()
        } else {
            mandate_ids.join(", ")
        }
    ));
    lines.push(format!(
        "CLAIMS: {} active. Inspect with `make governance-claims`.",
        state.active_claims.len()
    ));
    lines.push(workspace_hygiene::exposure_line(repo));
    if !heading_lines.is_empty() {
        lines.push("HEADINGS:".to_string());
        lines.extend(heading_lines);
    }
    Ok(lines.join("\n"))
}

pub fn render_text(
    state: &ActiveState,
    a2a_name: &str,
    a2a_summary: &str,
    repo: &Path,
) -> Result<String> {
    let mut body = compact_state(state, repo)?;
    let name = a2a_name.trim();
    let summary = a2a_summary.trim();
    if !name.is_empty() && !summary.is_empty() {
        let clipped: String = summary.chars().take(MAX_A2A_CHARS).collect();
        body.push_str(&format!(
            "\nA2A compact ({name}); retrieve only when needed: \
             `python -m conductor.agent_a2a show --as-name {name} <id>`; ack: \
             `python -m conductor.agent_a2a read --as-name {name} <id>`\n{clipped}"
        ));
    }
    if body.chars().count() > MAX_INJECT_CHARS {
        let mut clipped: String = body.chars().take(MAX_INJECT_CHARS - 1).collect();
        clipped = clipped.trim_end().to_string();
        clipped.push('…');
        body = clipped;
    }
    Ok(body)
}

/// `{"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": text}}`
/// -- the Claude Code SessionStart contract.
pub fn hook_payload(text: &str) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": text,
        }
    })
}

// ── CLI ────────────────────────────────────────────────────────────────────

#[derive(Args)]
pub struct SessionStateArgs {
    /// The host project root (the dir with pyproject.toml/.current_work.md).
    #[arg(long)]
    pub host: PathBuf,
    /// Print the JSON instead of writing conductor/active_state.json.
    #[arg(long)]
    pub dump: bool,
}

#[derive(Args)]
pub struct SessionPreambleArgs {
    /// The host project root (the dir with pyproject.toml/.current_work.md).
    #[arg(long)]
    pub host: PathBuf,
    /// A2A identity name; the A2A_AGENT_NAME env var when absent.
    #[arg(long)]
    pub a2a_name: Option<String>,
    /// A2A unread preview (clipped to 1200 chars); the A2A_SUMMARY env var
    /// when absent -- session-start.sh exports it either way.
    #[arg(long)]
    pub a2a_summary: Option<String>,
    /// Print the inject body instead of the hook-payload JSON.
    #[arg(long)]
    pub text: bool,
}

pub fn run_state(args: &SessionStateArgs) -> Result<u8> {
    let state = crate::active_state::generate_active_state(&args.host)?;
    if args.dump {
        println!(
            "{}",
            serde_json::to_string_pretty(&state).expect("plain data always serializes")
        );
    } else {
        crate::active_state::save_active_state(&args.host)?;
    }
    Ok(0)
}

/// Refresh the state first (the Python CLI's `load_state(refresh=True)`),
/// then render: `active_state update` is never a separate step on this path.
pub fn run_preamble(args: &SessionPreambleArgs) -> Result<u8> {
    let state = crate::active_state::save_active_state(&args.host)?;
    let name = args
        .a2a_name
        .clone()
        .unwrap_or_else(|| std::env::var("A2A_AGENT_NAME").unwrap_or_default());
    let summary = args
        .a2a_summary
        .clone()
        .unwrap_or_else(|| std::env::var("A2A_SUMMARY").unwrap_or_default());
    let text = render_text(&state, &name, &summary, &args.host)?;
    if args.text {
        // The Python `text` command appends the newline only when no A2A
        // summary was resolved, so shell `$()` captures stay line-shaped.
        if summary.is_empty() {
            println!("{text}");
        } else {
            print!("{text}");
        }
    } else {
        println!(
            "{}",
            serde_json::to_string(&hook_payload(&text)).expect("plain data always serializes")
        );
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::active_state::ActiveClaim;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct ScratchRepo(PathBuf);
    impl ScratchRepo {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "forge-session-preamble-test-{label}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            ScratchRepo(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for ScratchRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture_repo(label: &str) -> ScratchRepo {
        let scratch = ScratchRepo::new(label);
        std::fs::write(
            scratch.path().join("pyproject.toml"),
            "[tool.conductor.session]\npreamble = [\"PRE-LINE-1\", \"PRE-LINE-2\"]\nstanding_mandates = [\"KB-1: first mandate\", \"KB-2: second\"]\n",
        )
        .unwrap();
        std::fs::write(
            scratch.path().join(".current_work.md"),
            "## Active Coordination\nignored\n\n## Heading A\n\n## Heading B\n",
        )
        .unwrap();
        scratch
    }

    fn empty_state() -> ActiveState {
        ActiveState {
            schema_version: 1,
            last_updated: "2026-09-13T12:00:00+00:00".to_string(),
            standing_mandates: vec![
                "KB-1: first mandate".to_string(),
                "KB-2: second".to_string(),
            ],
            active_headings: vec!["Heading A".to_string(), "Heading B".to_string()],
            active_claims: vec![],
        }
    }

    #[test]
    fn golden_render_on_a_fixture_repo() {
        let scratch = fixture_repo("golden");
        let body = render_text(&empty_state(), "", "", scratch.path()).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(&lines[..2], &["PRE-LINE-1", "PRE-LINE-2"]);
        assert_eq!(lines[2], "MANDATES: KB-1, KB-2");
        assert_eq!(
            lines[3],
            "CLAIMS: 0 active. Inspect with `make governance-claims`."
        );
        assert!(
            lines[4].starts_with("EXPOSED: unavailable ("),
            "the fixture repo is not a git checkout, so the line degrades: {}",
            lines[4]
        );
        assert_eq!(lines[5], "HEADINGS:");
        assert_eq!(&lines[6..], &["- Heading A", "- Heading B"]);
        let payload = hook_payload(&body);
        assert_eq!(
            payload["hookSpecificOutput"]["hookEventName"], "SessionStart"
        );
        assert_eq!(payload["hookSpecificOutput"]["additionalContext"], body);
    }

    #[test]
    fn the_body_clips_at_2200_chars_with_a_single_ellipsis() {
        let scratch = ScratchRepo::new("clip");
        std::fs::write(
            scratch.path().join("pyproject.toml"),
            format!(
                "[tool.conductor.session]\npreamble = [\"{}\"]\nstanding_mandates = []\n",
                "L".repeat(3000)
            ),
        )
        .unwrap();
        let body = render_text(&empty_state(), "", "", scratch.path()).unwrap();
        assert_eq!(body.chars().count(), MAX_INJECT_CHARS);
        assert!(body.ends_with('…'));
        let before_ellipsis = &body[..body.len() - '…'.len_utf8()];
        assert_eq!(before_ellipsis.trim_end().len(), before_ellipsis.len());
    }

    #[test]
    fn the_a2a_block_needs_both_parts_and_clips_the_summary() {
        let scratch = fixture_repo("a2a");
        let both = render_text(
            &empty_state(),
            "fable-5",
            &"s".repeat(1300),
            scratch.path(),
        )
        .unwrap();
        assert!(both.contains("A2A compact (fable-5); retrieve only when needed"));
        let summary_line = both.lines().last().unwrap();
        assert_eq!(summary_line.chars().count(), MAX_A2A_CHARS);

        let name_only = render_text(&empty_state(), "fable-5", "", scratch.path()).unwrap();
        assert!(!name_only.contains("A2A compact"));
        let summary_only =
            render_text(&empty_state(), "", "some summary", scratch.path()).unwrap();
        assert!(!summary_only.contains("A2A compact"));

        // A claim in the state is counted, not just headings and mandates.
        let mut with_claim = empty_state();
        with_claim.active_claims = vec![ActiveClaim {
            claim_id: "claim-x".to_string(),
            owner: "llm-b0".to_string(),
            paths: vec!["src/foo.py".to_string()],
            justification: "because".to_string(),
            expires_at: "2026-09-13T13:00:00+00:00".to_string(),
        }];
        let body = render_text(&with_claim, "", "", scratch.path()).unwrap();
        assert!(body.contains("CLAIMS: 1 active."), "{body}");
    }
}
