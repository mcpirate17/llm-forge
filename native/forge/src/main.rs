//! Native agent platform: hooks, governance evidence, coordination and costs.

use forge::active_state;
mod bash_guard;
mod bash_impact;
mod bounded_child;
mod cap_enforce;
mod claims;
use forge::civil;
mod config_store;
mod context_telemetry;
mod context_telemetry_cli;
mod context_tools;
mod crg_gate;
mod crg_refresh;
mod current_work_guard;
mod dispatch;
mod doctor;
use forge::equivalence_worker;
mod graph_context;
mod handlers;
mod handoff;
mod hook_protocol;
mod hooks_install;
use forge::identity;
use forge::instant;
mod interpreter;
use forge::json_canon;
mod land;
mod land_exec;
mod ledger;
mod legacy_hook_api;
use forge::local_ai_policy;
mod local_check;
mod local_check_receipt;
mod mailbox;
mod mcp_setup;
mod merge;
use forge::mutation_plan;
#[cfg(target_os = "linux")]
mod mutation_evidence_cli;
mod mutation_results;
mod mutation_run_cli;
use forge::mutation_receipt_verify;
use forge::notes_index;
mod obsidian_sync;
use forge::ownership;
mod performance;
mod post_edit_audit;
mod post_tool;
mod pre_edit;
mod pre_read;
mod project;
mod provider;
mod providers;
mod read_budget;
mod receipt_show;
mod route;
use forge::session_brief;
mod session_end;
use forge::session_policy;
mod session_close_cli;
mod session_preamble;
mod session_start;
mod session_status_cli;
mod status;
mod subagent_stop;
mod subagent_transcript;
mod sweep;
mod takeover;
mod task_limits;
mod task_run;
mod tasks;
mod telemetry;
mod tool_quiet;
mod workspace_hygiene;
mod workspace_runtime;
use forge::write_targets;

use clap::{Parser, Subcommand};
use forge::tool_schema;
use forge::{task_resources, task_store};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "forge",
    // The git rev (stamped by build.rs, `unknown` outside a checkout) is
    // what `forge hooks status` compares an installed hook's binary by.
    version = concat!(env!("CARGO_PKG_VERSION"), " (git ", env!("FORGE_GIT_REV"), ")"),
    about = "Native hooks, task coordination, governance evidence and agent cost tooling"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Relocate exact console-script shebangs between two existing virtualenvs.
    #[cfg(target_os = "linux")]
    WorktreeRuntime(forge::worktree_runtime_cli::RuntimeArgs),
    /// Preserve dirty layers while fast-forwarding, or conditionally recover a journal.
    #[cfg(target_os = "linux")]
    CheckoutSync(forge::checkout_sync_cli::CheckoutSyncArgs),
    /// Full read-only workspace policy, ownership and checkout exposure report.
    #[cfg(target_os = "linux")]
    WorkspaceHygiene(forge::workspace_hygiene_report::HygieneArgs),
    /// Check native workspace runtime behavior with bounded, opt-in live probes.
    WorkspaceRuntime(workspace_runtime::RuntimeArgs),
    #[command(hide = true)]
    EquivalenceWorker(equivalence_worker::EquivalenceWorkerArgs),
    /// Create, release, touch and inspect structured repository ownership claims.
    Claim(claims::ClaimArgs),
    /// Inspect Git candidates, materialize snapshots and verify receipt digests.
    Candidate(forge::candidate_cli::CandidateArgs),
    /// Compile and query immutable native workspace-memory snapshots.
    Memory(forge::recall::cli::MemoryArgs),
    /// Build and search knowledge cards through pinned native embeddings.
    Cards(forge::cards_cli::CardsArgs),
    /// Inventory, draft and validate bounded local clerical summaries.
    Clerk(forge::local_clerk::cli::ClerkArgs),
    /// Serve bounded, provider-neutral embedding routes without an interpreter.
    EmbeddingBroker(forge::embedding_broker::cli::BrokerArgs),
    /// Bind, check or remove a project MCP connection to an existing native harness.
    Mcp(mcp_setup::McpArgs),
    /// Plan, initialize, inspect or roll back native host configuration.
    Project(project::ProjectArgs),
    /// Plan, install, check or roll back native provider hooks.
    Providers(providers::ProvidersArgs),
    #[command(hide = true)]
    GraphRefreshWorker {
        #[arg(long)]
        host: std::path::PathBuf,
        #[arg(long)]
        lock_fd: Option<i32>,
    },
    /// Prepare, stage and activate verified native session handoffs.
    Handoff(handoff::HandoffArgs),
    /// Persistent native tools with operator capabilities and confined terminals.
    Harness(forge::harness::cli::HarnessArgs),
    /// Build and query native immutable BM25 and vector code indexes.
    CodeIndex(forge::code_index::cli::CodeIndexArgs),
    /// Exact Radon-compatible Python complexity report and baseline ratchet.
    #[cfg(target_os = "linux")]
    Complexity(forge::complexity_cli::ComplexityArgs),
    /// Native source definitions, references, scopes and body folding.
    Code(forge::code_tools::CodeArgs),
    /// Native source guardrails and read-only ablation proposals.
    Analyze(forge::analysis::AnalysisArgs),
    /// Native structured-output validation, GBNF and persistent call checking.
    Schema(tool_schema::cli::SchemaArgs),
    /// Internal compatibility boundary for installed legacy hook entrypoints.
    #[command(hide = true)]
    LegacyHook { operation: String },
    #[command(hide = true)]
    TaskLimitExec(task_limits::LimitExecArgs),
    /// Durable assignment, execution, receipts, retries and lease recovery.
    Task(tasks::TaskArgs),
    /// Read-only health across tasks, claims, messaging, hooks and ledger paths.
    Status(status::StatusArgs),
    /// Inspect, acknowledge and queue messages in existing local mailboxes.
    Mailbox(mailbox::MailboxArgs),
    /// Retrieve bounded code relationships and message references from the local graph.
    Graph(graph_context::GraphArgs),
    /// Native branch naming, atomic claim bindings and read-only exposure reports.
    #[cfg(target_os = "linux")]
    BranchPolicy(forge::branch_policy_cli::BranchArgs),
    /// Advisory graph-selected pytest, native crate and compatibility tests.
    #[cfg(target_os = "linux")]
    GraphTestSelect(forge::graph_test_select::SelectArgs),
    #[cfg(target_os = "linux")]
    #[command(hide = true)]
    GraphTestWorker(forge::graph_test_select::WorkerArgs),
    /// Land a branch on the integration line from a persistent local clone:
    /// rebase, run the host's `.forge/land.toml` checks bounded, fast-forward
    /// push, delete the branch. No CI service involved.
    Land(land::LandArgs),
    /// Run the host's scheduled checks (`.forge/sweep.toml`) from a persistent
    /// clone of the integration line, or install the systemd timer that does.
    Sweep(sweep::SweepArgs),
    /// Run repository checks against the clean, committed current checkout.
    Check(local_check::CheckArgs),
    /// Plan or execute fast edit-loop checks; never a landing PASS receipt.
    Preview(local_check::PreviewArgs),
    /// Measure, compare, and profile source-bound workloads.
    Performance(performance::PerformanceArgs),
    /// Compose bounded context, discover tools, and report actual model usage.
    Context(context_tools::ContextArgs),
    /// Validate a local check receipt against the current checkout and policy.
    Verify(local_check::VerifyArgs),
    /// Run a hook event with native guards, context and lifecycle handlers.
    Hook {
        /// The event name, e.g. PreToolUse, PostToolUse, SessionStart, SessionEnd.
        event: String,
        #[arg(long, value_enum, default_value = "claude")]
        protocol: hook_protocol::Protocol,
        /// Pin the working directory and hook project environment before dispatch.
        #[arg(long, alias = "project-dir")]
        host: Option<std::path::PathBuf>,
    },
    /// Probe committed modules with explicit grants or fold existing equivalence evidence.
    #[cfg(target_os = "linux")]
    Equivalence(forge::equivalence_backlog::EquivalenceArgs),
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
    /// Verify a native host install end to end: settings, binaries,
    /// ledger, policy, and a live hook roundtrip (`docs/roadmap.md`
    /// Phase 4 item 2c).
    Doctor(doctor::DoctorArgs),
    /// The SessionStart pair ported from `conductor.active_state` and
    /// `conductor.session_preamble` (`docs/roadmap.md` Phase 4 item 2f).
    Session {
        #[command(subcommand)]
        action: SessionCommand,
    },
    /// FTS5 note index: `forge notes index|search` (`docs/roadmap.md`
    /// Phase 4 item 2h).
    Notes {
        #[command(subcommand)]
        action: notes_index::NotesCommand,
    },
}

#[derive(Subcommand)]
enum SessionCommand {
    /// Retire a session with explicit claim release and native memory refresh.
    Close(session_close_cli::CloseArgs),
    /// Append one bounded coordination entry and refresh active state.
    Append(session_status_cli::AppendArgs),
    /// Read a bounded task brief from policy, claims, inbox and explicit indexes.
    Brief(session_brief::BriefArgs),
    /// Append a native journal from a SessionEnd JSON payload on stdin.
    Journal {
        #[arg(long, default_value = ".")]
        host: std::path::PathBuf,
    },
    /// Write (or with --dump, print) conductor/active_state.json: standing
    /// mandates, the top .current_work.md headings, active claims.
    State(session_preamble::SessionStateArgs),
    /// Refresh the state, then print the session inject -- the hook-payload
    /// JSON, or the bare body with --text.
    Preamble(session_preamble::SessionPreambleArgs),
}

#[derive(Subcommand)]
enum LedgerCommand {
    /// Host cost budget defaults, reports and gate evidence.
    Budget(ledger::budget::BudgetArgs),
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
    /// Verify existing receipts for explicit test paths or changed tests; runs nothing.
    #[cfg(target_os = "linux")]
    Evidence(mutation_evidence_cli::EvidenceArgs),
    /// Inventory all test files against existing receipts; runs nothing.
    #[cfg(target_os = "linux")]
    Coverage(mutation_evidence_cli::ReportArgs),
    /// Check receipt readability across the repository, independently of coverage debt.
    #[cfg(target_os = "linux")]
    Canary(mutation_evidence_cli::ReportArgs),
    /// Describe this clean native executable for explicit operator pinning; grants no authority.
    #[cfg(target_os = "linux")]
    Identity,
    /// Copy a lock-verified offline crates.io seed into a new private directory.
    #[cfg(target_os = "linux")]
    CargoSeed(forge::mutation_cargo_seed_cli::SeedArgs),
    /// Export an inventory of a private Fest guest without executing it.
    #[cfg(target_os = "linux")]
    FestGuest(forge::mutation_fest_guest_cli::FestGuestArgs),
    /// Observe a bounded Mull runtime directory digest without granting execution.
    #[cfg(target_os = "linux")]
    MullInventory(forge::mutation_mull_inventory_cli::MullInventoryArgs),
    /// Preview refreshed pins for one confined Mull manifest; --apply publishes them.
    #[cfg(target_os = "linux")]
    Refresh(forge::mutation_mull_refresh_cli::RefreshArgs),
    /// Report the campaigns `forge mutation write` would emit; writes nothing.
    Plan(Box<mutation_plan::PlanArgs>),
    /// Write one manifest and registry fragment per planned subject.
    Write(Box<mutation_plan::WriteArgs>),
    /// Parse a test report into ordered attribution JSON without running tests.
    Results(mutation_results::ResultsArgs),
    /// Report whether a generated campaign could run now; runs nothing.
    Inspect(mutation_run_cli::InspectArgs),
    /// Generate, run and score a campaign's mutants in a disposable snapshot.
    Run(Box<mutation_run_cli::RunArgs>),
}

#[derive(Subcommand)]
enum ReceiptCommand {
    /// Check mutation receipt byte bindings against an immutable target Git tree.
    Verify(mutation_receipt_verify::VerifyArgs),
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

fn command_exit(label: &str, result: anyhow::Result<u8>, failure: u8) -> ExitCode {
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            if let Some(signal) = error.downcast_ref::<forge::candidate_commit::ChildSignal>() {
                propagate_child_signal(signal.0);
            }
            eprintln!("forge {label}: {error:#}");
            ExitCode::from(failure)
        }
    }
}

// After the command returns its lease has been dropped. Reflect a Git signal as
// a signal, not 128+signal or a generic wrapper failure.
fn propagate_child_signal(signal: i32) {
    unsafe {
        if signal == libc::SIGKILL {
            libc::raise(signal);
            return;
        }
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
            return;
        }
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, signal);
        if libc::sigprocmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut()) != 0 {
            return;
        }
        libc::raise(signal);
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse_from(mutation_receipt_verify::normalize_args(std::env::args_os()));
    match cli.command {
        #[cfg(target_os = "linux")]
        Command::WorktreeRuntime(args) => run_worktree_runtime(args),
        #[cfg(target_os = "linux")]
        Command::CheckoutSync(args) => run_checkout_sync(args),
        #[cfg(target_os = "linux")]
        Command::WorkspaceHygiene(args) => run_workspace_hygiene(args),
        Command::EquivalenceWorker(args) => command_exit(
            "equivalence-worker",
            equivalence_worker::run(args).map(|()| 0),
            2,
        ),
        Command::WorkspaceRuntime(args) => {
            command_exit("workspace-runtime", workspace_runtime::run(args), 2)
        }
        Command::Claim(args) => command_exit("claim", claims::run(args).map(|()| 0), 2),
        Command::Candidate(args) => run_candidate(args),
        Command::Memory(args) => command_exit("memory", forge::recall::cli::run(args), 2),
        Command::Cards(args) => command_exit("cards", forge::cards_cli::run(args), 2),
        Command::Clerk(args) => command_exit("clerk", forge::local_clerk::cli::run(args), 2),
        Command::EmbeddingBroker(args) => command_exit(
            "embedding-broker",
            forge::embedding_broker::cli::run(args),
            2,
        ),
        Command::Mcp(args) => command_exit("MCP configuration", mcp_setup::run(args), 2),
        Command::Project(args) => command_exit("project", project::run(args), 2),
        Command::Providers(args) => command_exit("providers", providers::run(args), 2),
        Command::GraphRefreshWorker { host, lock_fd } => {
            command_exit("graph refresh", crg_refresh::run_worker(&host, lock_fd), 2)
        }
        Command::Handoff(args) => command_exit("handoff", handoff::run(args), 2),
        Command::Analyze(args) => command_exit("analyze", forge::analysis::run(args), 2),
        Command::Code(args) => command_exit("code", forge::code_tools::run(args), 2),
        Command::Harness(args) => command_exit("harness", forge::harness::cli::run(args), 2),
        Command::CodeIndex(args) => {
            command_exit("code-index", forge::code_index::cli::run(args), 2)
        }
        #[cfg(target_os = "linux")]
        Command::Complexity(args) => {
            command_exit("complexity", forge::complexity_cli::run(args), 1)
        }
        Command::Schema(args) => command_exit("schema", tool_schema::cli::run(args), 2),
        Command::LegacyHook { operation } => {
            command_exit("legacy-hook", legacy_hook_api::run(&operation), 2)
        }
        Command::TaskLimitExec(args) => {
            command_exit("task limits", task_limits::run_guard(args), 1)
        }
        Command::Task(args) => command_exit("task", tasks::run(args), 1),
        Command::Land(args) => command_exit("land", land::run(args), 2),
        Command::Sweep(args) => command_exit("sweep", sweep::run(args), 2),
        Command::Check(args) => command_exit("check", local_check::run(args), 2),
        Command::Preview(args) => command_exit("preview", local_check::preview(args), 2),
        Command::Performance(args) => command_exit("performance", performance::run(args), 2),
        Command::Context(args) => command_exit("context", context_tools::run(args), 2),
        Command::Verify(args) => command_exit("verify", local_check::verify(args), 2),
        Command::Status(args) => command_exit("status", status::run(args), 1),
        Command::Mailbox(args) => command_exit("mailbox", mailbox::run(args), 2),
        Command::Graph(args) => command_exit("graph", graph_context::run(args), 2),
        #[cfg(target_os = "linux")]
        Command::BranchPolicy(args) => {
            command_exit("branch-policy", forge::branch_policy_cli::run(args), 2)
        }
        #[cfg(target_os = "linux")]
        Command::GraphTestSelect(args) => {
            command_exit("graph-test-select", forge::graph_test_select::run(args), 2)
        }
        #[cfg(target_os = "linux")]
        Command::GraphTestWorker(args) => command_exit(
            "graph-test-worker",
            forge::graph_test_select::worker(args),
            2,
        ),
        Command::Hook {
            event,
            protocol,
            host,
        } => match dispatch::run_hook(&event, protocol, host.as_deref()) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("forge hook {event}: {err:#}");
                ExitCode::from(1)
            }
        },
        #[cfg(target_os = "linux")]
        Command::Equivalence(args) => {
            command_exit("equivalence", forge::equivalence_backlog::run(args), 4)
        }
        Command::Mutation { action } => run_mutation(action),
        Command::Receipt { action } => match action {
            ReceiptCommand::Verify(args) => ExitCode::from(mutation_receipt_verify::run(args)),
            ReceiptCommand::Show(args) => command_exit("receipt show", receipt_show::run(&args), 1),
        },
        Command::Route(args) => command_exit("route", route::run(args).map(|code| code as u8), 2),
        Command::Hooks { action } => command_exit("hooks", hooks_install::run(action), 1),
        Command::Session { action } => run_session(action),
        Command::Doctor(args) => command_exit("doctor", doctor::run(&args), 1),
        Command::Notes { action } => command_exit("notes", notes_index::run(action), 1),
        Command::Ledger { action } => run_ledger(action),
    }
}

#[cfg(target_os = "linux")]
fn run_checkout_sync(args: forge::checkout_sync_cli::CheckoutSyncArgs) -> ExitCode {
    command_exit("checkout-sync", forge::checkout_sync_cli::run(args), 2)
}

#[cfg(target_os = "linux")]
fn run_workspace_hygiene(args: forge::workspace_hygiene_report::HygieneArgs) -> ExitCode {
    command_exit(
        "workspace-hygiene",
        forge::workspace_hygiene_report::run(args),
        2,
    )
}

#[cfg(target_os = "linux")]
fn run_worktree_runtime(args: forge::worktree_runtime_cli::RuntimeArgs) -> ExitCode {
    command_exit(
        "worktree-runtime",
        forge::worktree_runtime_cli::run(args),
        2,
    )
}

fn run_mutation(action: MutationCommand) -> ExitCode {
    match action {
        #[cfg(target_os = "linux")]
        MutationCommand::Evidence(args) => command_exit(
            "mutation evidence",
            mutation_evidence_cli::evidence(args, &build_stamp()),
            4,
        ),
        #[cfg(target_os = "linux")]
        MutationCommand::Coverage(args) => command_exit(
            "mutation coverage",
            mutation_evidence_cli::coverage(args, &build_stamp()),
            4,
        ),
        #[cfg(target_os = "linux")]
        MutationCommand::Canary(args) => command_exit(
            "mutation canary",
            mutation_evidence_cli::canary(args, &build_stamp()),
            4,
        ),
        #[cfg(target_os = "linux")]
        MutationCommand::Identity => command_exit("mutation identity", native_runner_identity(), 1),
        #[cfg(target_os = "linux")]
        MutationCommand::CargoSeed(args) => command_exit(
            "mutation cargo-seed",
            forge::mutation_cargo_seed_cli::run(args),
            1,
        ),
        #[cfg(target_os = "linux")]
        MutationCommand::FestGuest(args) => command_exit(
            "mutation fest-guest",
            forge::mutation_fest_guest_cli::run(args),
            1,
        ),
        #[cfg(target_os = "linux")]
        MutationCommand::MullInventory(args) => command_exit(
            "mutation mull-inventory",
            forge::mutation_mull_inventory_cli::run(args),
            1,
        ),
        #[cfg(target_os = "linux")]
        MutationCommand::Refresh(args) => {
            command_exit("mutation refresh", run_mutation_refresh(args), 4)
        }
        MutationCommand::Results(args) => {
            command_exit("mutation results", mutation_results::run(args), 2)
        }
        MutationCommand::Plan(args) => command_exit(
            "mutation plan",
            mutation_plan::run(*args).map(|code| code as u8),
            1,
        ),
        MutationCommand::Write(args) => command_exit(
            "mutation write",
            mutation_plan::write(*args).map(|code| code as u8),
            1,
        ),
        MutationCommand::Inspect(args) => command_exit(
            "mutation inspect",
            mutation_run_cli::inspect(args).map(|code| code as u8),
            1,
        ),
        MutationCommand::Run(args) => command_exit(
            "mutation run",
            mutation_run_cli::run(*args).map(|code| code as u8),
            1,
        ),
    }
}

#[cfg(target_os = "linux")]
fn run_mutation_refresh(args: forge::mutation_mull_refresh_cli::RefreshArgs) -> anyhow::Result<u8> {
    let root = mutation_plan::resolve_repo_root(args.repo_root.clone())?;
    forge::mutation_mull_refresh_cli::run(&root, &args)
}

fn build_stamp() -> forge::candidate_engine_identity::BuildStamp {
    forge::candidate_engine_identity::BuildStamp {
        source_sha256: env!("FORGE_SOURCE_SHA256"),
        git_commit: env!("FORGE_GIT_FULL_REV"),
        source_matches_git: env!("FORGE_SOURCE_MATCHES_GIT"),
    }
}

#[cfg(target_os = "linux")]
fn native_runner_identity() -> anyhow::Result<u8> {
    let identity = forge::candidate_native_runner_pin::describe(&build_stamp())?;
    println!("{}", serde_json::to_string(&identity)?);
    Ok(0)
}

fn run_candidate(args: forge::candidate_cli::CandidateArgs) -> ExitCode {
    command_exit(
        "candidate",
        forge::candidate_cli::run_with_stamp(args, Some(&build_stamp())),
        1,
    )
}

fn run_session(action: SessionCommand) -> ExitCode {
    match action {
        SessionCommand::Close(args) => {
            command_exit("session close", session_close_cli::run(args), 2)
        }
        SessionCommand::Append(args) => {
            command_exit("session append", session_status_cli::run(args), 2)
        }
        SessionCommand::Brief(args) => command_exit("session brief", session_brief::run(args), 2),
        SessionCommand::Journal { host } => {
            command_exit("session journal", obsidian_sync::run_journal(&host), 2)
        }
        SessionCommand::State(args) => match session_preamble::run_state(&args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("forge session state: {err:#}");
                ExitCode::from(1)
            }
        },
        SessionCommand::Preamble(args) => match session_preamble::run_preamble(&args) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("forge session preamble: {err:#}");
                ExitCode::from(1)
            }
        },
    }
}

fn run_ledger(action: LedgerCommand) -> ExitCode {
    match action {
        LedgerCommand::Budget(args) => command_exit(
            "ledger budget",
            ledger::budget::run(args).map(|code| code as u8),
            2,
        ),
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
    }
}
