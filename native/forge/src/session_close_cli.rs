//! CLI for native session close.
use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct CloseArgs {
    #[arg(long, alias = "repo", default_value = ".")]
    host: PathBuf,
    #[arg(long)]
    owner: String,
    #[arg(long)]
    title: Option<String>,
    #[arg(long, allow_hyphen_values = true)]
    body: Option<String>,
    #[arg(long = "claim-id", conflicts_with = "all_owner_claims")]
    claim_ids: Vec<String>,
    #[arg(long)]
    all_owner_claims: bool,
    #[arg(long, conflicts_with_all=["memory_embedding","memory_store","memory_catalog"])]
    no_memory_index: bool,
    #[arg(long)]
    memory_embedding: Option<PathBuf>,
    #[arg(long)]
    memory_store: Option<PathBuf>,
    #[arg(long)]
    memory_catalog: Option<PathBuf>,
    #[arg(long)]
    json: bool,
}
pub fn run(args: CloseArgs) -> Result<u8> {
    let outcome = forge::session_close::close(forge::session_close::Request {
        host: args.host,
        owner: args.owner,
        title: args.title,
        body: args.body,
        claim_ids: args.claim_ids,
        all_owner_claims: args.all_owner_claims,
        no_memory_index: args.no_memory_index,
        memory_embedding: args.memory_embedding,
        memory_store: args.memory_store,
        memory_catalog: args.memory_catalog,
    })?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&outcome)?);
    } else {
        println!("session-close {} | owner={}", outcome.status, outcome.owner);
        println!(
            "- claims released: {} ({})",
            outcome.claims_released.len(),
            outcome.claim_release
        );
        if let Some(entry) = &outcome.handoff_entry {
            println!("- handoff: {entry}");
        }
        println!("- memory index: {}", outcome.memory_status);
        for error in &outcome.errors {
            eprintln!("session-close: {error}");
        }
    }
    Ok(outcome.exit_code())
}
