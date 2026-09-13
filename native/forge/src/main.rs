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
mod bounded_child;
mod cap_enforce;
mod civil;
mod context_telemetry;
mod crg_gate;
mod crg_refresh;
mod current_work_guard;
mod dispatch;
mod doctor;
mod handlers;
mod hooks_install;
mod identity;
mod instant;
mod interpreter;
mod json_canon;
mod ledger;
mod local_ai_policy;
mod merge;
mod mutation_plan;
mod notes_index;
mod obsidian_sync;
mod ownership;
mod post_edit_audit;
mod post_tool;
mod read_budget;
mod receipt_show;
mod route;
mod session_end;
mod subagent_stop;
mod subagent_transcript;
mod takeover;
mod telemetry;
mod tool_quiet;
mod workspace_hygiene;
mod write_targets;

use clap::{Parser, Subcommand};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "forge",
    // The git rev (stamped by build.rs, `unknown` outside a checkout) is
    // what `forge hooks status` compares an installed hook's binary by.
    version = concat!(env!("CARGO_PKG_VERSION"), " (git ", env!("FORGE_GIT_REV"), ")"),
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
    /// Receipt inspection: the human window into the slim-receipt format.
    Receipt {
        #[command(subcommand)]
        action: ReceiptCommand,
    },
    /// Cost ledger commands (`docs/design/cost_ledger.md`).
    Ledger {
        #[command(subcommand)]
        action: LedgerCommand,
    },
    /// Routing-policy decision for one `Agent` dispatch (`docs/roadmap.md`
    /// Phase 3 step 2).
    Route(route::RouteArgs),
    /// Install/uninstall/inspect forge's entries in a host's Claude Code
    /// settings (`docs/roadmap.md` Phase 4).
    Hooks {
        #[command(subcommand)]
        action: hooks_install::HooksCommand,
    },
    /// Verify a host install end to end: settings, binaries, Python,
    /// ledger, policy, and a live hook roundtrip (`docs/roadmap.md`
    /// Phase 4 item 2c).
    Doctor(doctor::DoctorArgs),
    /// FTS5 note index: `forge notes index|search` (`docs/roadmap.md`
    /// Phase 4 item 2h).
    Notes {
        #[command(subcommand)]
        action: notes_index::NotesCommand,
    },
}

#[derive(Subcommand)]
enum LedgerCommand {
    /// Parse transcript or telemetry JSONL file(s) into ledger summaries.
    Read(ledger::ReadArgs),
    /// Roll transcript/telemetry JSONL up into `turn_attribution`,
    /// `session_rollup` and `hook_rollup` (design step 2), and -- when
    /// `--repo` is given -- `agent_rollup` (design step 4).
    Rollup(ledger::rollup::RollupArgs),
    /// Scan a repo's `git log --first-parent main` into one JSONL row per
    /// landed commit (design step 4).
    Landed(ledger::landed::LandedArgs),
    /// Compute the three budget-ratchet metrics over a trailing window and
    /// compare (or, with `--record`, replace) the baseline receipt (design
    /// step 5).
    Audit(ledger::audit::AuditArgs),
    /// Retire day files older than the retention window: archive the
    /// aggregates, drop the raw tables (design section 2 "Storage").
    Prune(ledger::prune::PruneArgs),
    /// Calibration harness (design step 3): sample turns for the Python
    /// shim that measures the byte-proportional split's error bound.
    Calibrate {
        #[command(subcommand)]
        action: ledger::calibrate::CalibrateCommand,
    },
    /// `SubagentStop`-triggered narrow upsert of one `task_dispatch` row
    /// from a single subagent transcript (Phase 3 step 3, item 1).
    RollupAgent(ledger::agent_upsert::AgentUpsertArgs),
    /// Per-tier aggregation over `task_dispatch` (Phase 3 step 3, item 4).
    Report(ledger::report::ReportArgs),
}

#[derive(Subcommand)]
enum MutationCommand {
    /// Report what `conductor.mutation_campaign_generate write` would emit,
    /// without writing anything. Same computation as the Python `plan()`
    /// function's native path (`CONDUCTOR_PLAN_IMPL` unset).
    Plan(mutation_plan::PlanArgs),
}

#[derive(Subcommand)]
enum ReceiptCommand {
    /// Print a receipt with its slim detail block expanded back to full rows.
    Show(receipt_show::ShowArgs),
}

/// `forge route`'s cap and routing verdict for one subagent_type, for
/// `ledger rollup-agent` (Phase 3 step 3, items 1 and 3): lives here, not
/// in `ledger::agent_upsert`, because `route` is a top-level module the
/// `ledger` tree cannot `use` when compiled standalone by the
/// `tests/ledger_*.rs` integration binaries -- see `agent_upsert::run`'s
/// doc comment. Falls back to the crate's `DEFAULT_CAP` and a bare
/// `"allow"`/no-model verdict if the embedded policy ever fails to parse,
/// same as `dispatch.rs`/`cap_enforce.rs` do for the live `PreToolUse`
/// seam -- an unparseable policy must never manufacture a false `"deny"`
/// on this row any more than it would on the live cap check.
fn resolve_agent_route(subagent_type: Option<&str>) -> ledger::agent_upsert::AgentRouteResolution {
    let policy = match route::Policy::embedded() {
        Ok(policy) => policy,
        Err(err) => {
            eprintln!(
                "forge ledger rollup-agent: embedded routing policy failed to parse ({err:#}); over_cap/decision left unresolved against the crate default"
            );
            return ledger::agent_upsert::AgentRouteResolution {
                cap_tokens: ledger::agent::DEFAULT_CAP,
                decision: "allow".to_string(),
                would_assign_model: false,
            };
        }
    };
    let input = route::AgentInput {
        subagent_type: subagent_type.map(str::to_string),
        requested_model: None,
        description: None,
    };
    let decision = route::route(&policy, &input);
    ledger::agent_upsert::AgentRouteResolution {
        cap_tokens: decision.cap_tokens,
        decision: match decision.decision {
            route::Verdict::Allow => "allow".to_string(),
            route::Verdict::Deny => "deny".to_string(),
        },
        would_assign_model: decision.model.is_some(),
    }
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
        Command::Receipt { action } => match action {
            ReceiptCommand::Show(args) => match receipt_show::run(&args) {
                Ok(code) => ExitCode::from(code),
                Err(err) => {
                    eprintln!("forge receipt show: {err:#}");
                    ExitCode::from(1)
                }
            },
        },
        Command::Route(args) => match route::run(args) {
            Ok(code) => ExitCode::from(code as u8),
            Err(err) => {
                eprintln!("forge route: {err:#}");
                ExitCode::from(2)
            }
        },
        Command::Hooks { action } => match hooks_install::run(action) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("forge hooks: {err:#}");
                ExitCode::from(1)
            }
        },
        Command::Doctor(args) => match doctor::run(&args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("forge doctor: {err:#}");
                ExitCode::from(1)
            }
        },
        Command::Notes { action } => match notes_index::run(action) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("forge notes: {err:#}");
                ExitCode::from(1)
            }
        },
        Command::Ledger { action } => match action {
            LedgerCommand::Read(args) => match ledger::run(args) {
                Ok(code) => ExitCode::from(code as u8),
                Err(err) => {
                    eprintln!("forge ledger read: {err:#}");
                    ExitCode::from(1)
                }
            },
            LedgerCommand::Rollup(args) => match ledger::rollup::run(args) {
                Ok(code) => ExitCode::from(code as u8),
                Err(err) => {
                    eprintln!("forge ledger rollup: {err:#}");
                    ExitCode::from(1)
                }
            },
            LedgerCommand::Landed(args) => match ledger::landed::run(args) {
                Ok(code) => ExitCode::from(code as u8),
                Err(err) => {
                    eprintln!("forge ledger landed: {err:#}");
                    ExitCode::from(1)
                }
            },
            LedgerCommand::Audit(args) => match ledger::audit::run(args) {
                Ok(code) => ExitCode::from(code as u8),
                Err(err) => {
                    eprintln!("forge ledger audit: {err:#}");
                    ExitCode::from(1)
                }
            },
            LedgerCommand::Prune(args) => match ledger::prune::run(args) {
                Ok(code) => ExitCode::from(code as u8),
                Err(err) => {
                    eprintln!("forge ledger prune: {err:#}");
                    ExitCode::from(1)
                }
            },
            LedgerCommand::Calibrate { action } => match action {
                ledger::calibrate::CalibrateCommand::Sample(args) => {
                    match ledger::calibrate::run_sample(args) {
                        Ok(code) => ExitCode::from(code as u8),
                        Err(err) => {
                            eprintln!("forge ledger calibrate sample: {err:#}");
                            ExitCode::from(1)
                        }
                    }
                }
            },
            LedgerCommand::RollupAgent(args) => {
                match ledger::agent_upsert::run(args, resolve_agent_route) {
                    Ok(code) => ExitCode::from(code as u8),
                    Err(err) => {
                        eprintln!("forge ledger rollup-agent: {err:#}");
                        ExitCode::from(1)
                    }
                }
            }
            LedgerCommand::Report(args) => match ledger::report::run(args) {
                Ok(code) => ExitCode::from(code as u8),
                Err(err) => {
                    eprintln!("forge ledger report: {err:#}");
                    ExitCode::from(1)
                }
            },
        },
    }
}
