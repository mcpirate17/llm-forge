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
mod context_telemetry;
mod crg_gate;
mod crg_refresh;
mod current_work_guard;
mod dispatch;
mod handlers;
mod identity;
mod instant;
mod interpreter;
mod ledger;
mod local_ai_policy;
mod merge;
mod mutation_plan;
mod obsidian_sync;
mod ownership;
mod post_edit_audit;
mod post_tool;
mod read_budget;
mod telemetry;
mod tool_quiet;
mod workspace_hygiene;
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
    /// Mutation-campaign commands that run natively, no Python interpreter.
    Mutation {
        #[command(subcommand)]
        action: MutationCommand,
    },
    /// Cost ledger commands (`docs/design/cost_ledger.md`).
    Ledger {
        #[command(subcommand)]
        action: LedgerCommand,
    },
}

#[derive(Subcommand)]
enum LedgerCommand {
    /// Parse transcript or telemetry JSONL file(s) into ledger summaries.
    Read(ledger::ReadArgs),
}

#[derive(Subcommand)]
enum MutationCommand {
    /// Report what `conductor.mutation_campaign_generate write` would emit,
    /// without writing anything. Same computation as the Python `plan()`
    /// function's native path (`CONDUCTOR_PLAN_IMPL` unset).
    Plan(mutation_plan::PlanArgs),
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
        Command::Mutation { action } => match action {
            MutationCommand::Plan(args) => match mutation_plan::run(args) {
                Ok(code) => ExitCode::from(code as u8),
                Err(err) => {
                    eprintln!("forge mutation plan: {err:#}");
                    ExitCode::from(1)
                }
            },
        },
        Command::Ledger { action } => match action {
            LedgerCommand::Read(args) => match ledger::run(args) {
                Ok(code) => ExitCode::from(code as u8),
                Err(err) => {
                    eprintln!("forge ledger read: {err:#}");
                    ExitCode::from(1)
                }
            },
        },
    }
}
