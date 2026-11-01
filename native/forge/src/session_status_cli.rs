//! Native bounded status append entry point.
use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct AppendArgs {
    #[arg(long, default_value = ".")]
    host: PathBuf,
    #[arg(long)]
    owner: String,
    #[arg(long)]
    title: String,
    #[arg(long, allow_hyphen_values = true)]
    body: String,
    #[arg(long)]
    json: bool,
}

pub fn run(args: AppendArgs) -> Result<u8> {
    let outcome =
        forge::session_status::append_status(&args.host, &args.owner, &args.title, &args.body)?;
    if args.json {
        println!("{}", serde_json::to_string(&outcome)?);
    } else {
        println!("{}", outcome.entry.lines().next().unwrap_or_default());
    }
    if let Some(error) = &outcome.refresh_error {
        eprintln!(".current_work.md written, but active state refresh failed: {error}");
    }
    Ok(outcome.exit_code())
}
