//! Provider hook envelopes around native startup. No identity is inferred from
//! the provider. Once-per-session state records successful startup before output.
use super::{registry, startup, view};
use crate::provider::Provider;
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::io::Read;
use std::path::Path;

#[path = "mailbox_startup_once.rs"]
mod once;

#[derive(Args)]
pub(super) struct HookArgs {
    #[arg(long, value_enum)]
    provider: Provider,
    /// Explicit A2A identity, otherwise A2A_AGENT_NAME or HOST's default_identity.
    #[arg(long)]
    identity: Option<String>,
    /// Suppress repeat successful startups for this provider and session for 24h.
    /// Always enabled for Grok's UserPromptSubmit hook.
    #[arg(long)]
    once_per_session: bool,
    #[arg(long, default_value_t = 3, value_parser=clap::value_parser!(u64).range(1..=10))]
    serve_timeout_seconds: u64,
    #[arg(long, default_value_t = 86400, value_parser=clap::value_parser!(u64).range(1..=86400))]
    lifetime_seconds: u64,
}

fn payload() -> Result<Value> {
    let mut text = String::new();
    std::io::stdin()
        .take(1024 * 1024 + 1)
        .read_to_string(&mut text)?;
    ensure!(text.len() <= 1024 * 1024, "hook input exceeds 1 MiB");
    let value = forge::tool_schema::parse_document(&text)?;
    ensure!(value.is_object(), "hook input must be a JSON object");
    Ok(value)
}

pub(super) fn run(args: HookArgs, host: &Path, root: &Path) -> Result<Option<Value>> {
    let input = payload()?;
    let identity = match args.identity {
        Some(value) => value.trim().to_owned(),
        None => crate::session_start::identity(host)?.context(
            "A2A identity missing; pass --identity, set A2A_AGENT_NAME or configure default_identity",
        )?,
    };
    super::store::validate_identity(&identity)?;
    let root = root.canonicalize()?;
    ensure!(
        registry::load(&root)?.contains_key(&identity),
        "A2A identity is not registered"
    );
    let mut once = if args.once_per_session || args.provider == Provider::Grok {
        let session = input["session_id"]
            .as_str()
            .context("once-per-session hook requires session_id")?;
        Some(once::State::open(
            &root.join(&identity),
            args.provider.name(),
            session,
        )?)
    } else {
        None
    };
    if once.as_ref().is_some_and(once::State::already_done) {
        return Ok(None);
    }
    let result = startup::run(
        startup::StartupArgs {
            identity,
            serve_timeout_seconds: args.serve_timeout_seconds,
            lifetime_seconds: args.lifetime_seconds,
        },
        &root,
    )?;
    ensure!(
        result["flush_status"].as_u64().is_some(),
        "mailbox flush failed: {}",
        result["flush_error"]
    );
    let preview = view::compact_json(&result["preview"])?;
    ensure!(
        preview.chars().count() <= 1200,
        "startup preview exceeds 1200 characters"
    );
    let output = json!({"hookSpecificOutput": {
        "hookEventName":args.provider.startup_event(), "additionalContext":preview
    }});
    if let Some(state) = once.as_mut() {
        state.commit()?;
    }
    Ok(Some(output))
}
