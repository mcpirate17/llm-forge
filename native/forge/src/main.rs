//! `forge`: native launcher for llm-forge's Claude Code hook path.
//!
//! Step 1 of the Rust hook port (see `research/rust_port_plan.md` section 4): this
//! binary owns nothing yet but the process boundary. `forge hook <Event>` reads the
//! hook JSON on stdin, times the call, and delegates whole to the existing Python
//! dispatcher (`python -m tooling.hooks.dispatch <Event>`), forwarding stdin,
//! stdout, stderr and the exit code unchanged. Later PRs move one hook body at a
//! time into `handlers::registry()` and `dispatch::run_hook` stops shelling out for
//! whatever is covered.

mod bash_guard;
mod bash_impact;
mod civil;
mod crg_gate;
mod crg_refresh;
mod current_work_guard;
mod dispatch;
mod handlers;
mod identity;
mod instant;
mod interpreter;
mod local_ai_policy;
mod merge;
mod ownership;
mod telemetry;
mod write_targets;

use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "forge",
    version,
    about = "Native launcher for llm-forge's Claude Code hook path"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run one Claude Code hook event: read the payload on stdin, delegate to the
    /// Python dispatcher, forward its stdout/stderr/exit code unchanged.
    Hook {
        /// The event name, e.g. PreToolUse, PostToolUse, SessionStart, SessionEnd.
        event: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Hook { event } => match dispatch::run_hook(&event) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("forge hook {event}: {err:#}");
                ExitCode::from(1)
            }
        },
    }
}
