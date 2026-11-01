//! Native inspection and acknowledgment of existing local A2A mailboxes.

#[path = "mailbox_supervisor_io.rs"]
mod events;
#[path = "mailbox_fleet.rs"]
mod fleet;
#[path = "mailbox_queue.rs"]
mod queue;
#[path = "mailbox_registry.rs"]
mod registry;
#[path = "mailbox_retention.rs"]
mod retention;
#[path = "mailbox_server.rs"]
mod server;
#[path = "mailbox_startup.rs"]
mod startup;
#[path = "mailbox_startup_hook.rs"]
mod startup_hook;
#[path = "mailbox_store.rs"]
mod store;
#[path = "mailbox_supervisor.rs"]
mod supervisor;
#[path = "mailbox_transport.rs"]
mod transport;
#[path = "mailbox_view.rs"]
mod view;
#[path = "mailbox_watch.rs"]
mod watch;

use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct MailboxArgs {
    /// Host containing .agents/a2a; inspection commands never initialize stores.
    #[arg(long, default_value = ".", global = true)]
    host: PathBuf,
    /// Explicit existing A2A state directory; relative paths use the current directory.
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    action: MailboxCommand,
}

#[derive(Subcommand)]
enum MailboxCommand {
    /// Initialize one registered local A2A identity.
    Init(InitArgs),
    /// Serve one local Agent Card and authenticated SendMessage endpoint.
    Serve(server::ServeArgs),
    /// Ensure one registered receiver is ready, retry its queue and show a bounded preview.
    Start(startup::StartupArgs),
    /// Native bounded A2A startup output for one installed provider hook.
    StartupHook(startup_hook::HookArgs),
    /// Retry queued delivery for a finite foreground lifetime, optionally owning a receiver.
    Supervise(supervisor::SupervisorArgs),
    /// Probe registered identities and update liveness counters.
    Peers,
    /// Read a bounded fleet report without updating mailbox, claim or liveness state.
    Fleet(fleet::FleetArgs),
    /// Remove identities after bounded consecutive failed card probes.
    Reap(ReapArgs),
    /// Send one outbound message with durable retry and a bounded peer attempt.
    Send(transport::SendArgs),
    /// Retry a bounded batch of queued or crash-left pending messages.
    Flush(transport::FlushArgs),
    /// Queue one outbound message durably for a later transport flush.
    Enqueue(queue::EnqueueArgs),
    /// Read bounded inbox summaries without marking anything read or presented.
    Inbox(InboxArgs),
    /// Present bounded inbound batches for a finite lifetime, preserving read acknowledgment.
    Watch(watch::WatchArgs),
    /// Explicitly retrieve one complete message, refusing oversized content.
    Show(ShowArgs),
    /// Atomically mark one unread inbound message read.
    Read(MessageArgs),
    /// Resolve one acknowledged inbound message without printing its content.
    Resolve(MessageArgs),
    /// Set or clear a retention hold on an inbound message.
    Hold(HoldArgs),
    /// Read bounded outbound delivery events without creating or migrating stores.
    History(HistoryArgs),
    /// Preview or explicitly apply bounded resolved-message retention.
    Retention(retention::RetentionArgs),
}

#[derive(Args)]
struct IdentityArgs {
    #[arg(long)]
    as_name: String,
}

#[derive(Args)]
struct InitArgs {
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    port: Option<u16>,
    #[arg(long)]
    renew_generation: bool,
}

#[derive(Args)]
struct ReapArgs {
    #[arg(long, default_value_t = 3)]
    consecutive_failures: u64,
}

#[derive(Args)]
struct InboxArgs {
    #[command(flatten)]
    identity: IdentityArgs,
    #[arg(long)]
    unread: bool,
    /// Explicit spelling of the default bounded view. Use show for full content.
    #[arg(long)]
    compact: bool,
    #[arg(long, default_value_t = 8)]
    max_messages: usize,
    #[arg(long, default_value_t = 140)]
    preview_chars: usize,
    #[arg(long, default_value_t = 1200)]
    max_chars: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct MessageArgs {
    #[command(flatten)]
    identity: IdentityArgs,
    message_id: String,
}

#[derive(Args)]
struct HoldArgs {
    #[command(flatten)]
    message: MessageArgs,
    /// Normalized retention reason, 1..240 characters.
    #[arg(long, conflicts_with = "clear", required_unless_present = "clear")]
    reason: Option<String>,
    #[arg(long, conflicts_with = "reason")]
    clear: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum Direction {
    Inbound,
    Outbound,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Inbound => "inbound",
            Self::Outbound => "outbound",
        }
    }
}

#[derive(Args)]
struct ShowArgs {
    #[command(flatten)]
    message: MessageArgs,
    #[arg(long, value_enum, default_value = "inbound")]
    direction: Direction,
    /// Maximum stored row bytes and rendered output bytes (1..16777216).
    #[arg(long, default_value_t = 1_048_576)]
    max_bytes: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct HistoryArgs {
    #[command(flatten)]
    identity: IdentityArgs,
    #[arg(long)]
    message_id: Option<String>,
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

fn print_json(value: &Value) -> Result<()> {
    println!("{}", view::compact_json(value)?);
    Ok(())
}

pub fn run(args: MailboxArgs) -> Result<u8> {
    let host = args.host;
    let state_dir = args.state_dir.unwrap_or_else(|| host.join(".agents/a2a"));
    match args.action {
        MailboxCommand::Init(args) => {
            let records = registry::init(
                &state_dir,
                args.name.as_deref(),
                args.port,
                args.renew_generation,
            )?;
            print_json(&registry::summary(&state_dir, &records))?;
        }
        MailboxCommand::Serve(args) => server::serve(args, state_dir)?,
        MailboxCommand::Start(args) => print_json(&startup::run(args, &state_dir)?)?,
        MailboxCommand::Supervise(args) => {
            let (state, exit) = supervisor::run(args, &state_dir)?;
            print_json(&state)?;
            return Ok(exit);
        }
        MailboxCommand::StartupHook(args) => {
            if let Some(output) = startup_hook::run(args, &host, &state_dir)? {
                print_json(&output)?;
            }
        }
        MailboxCommand::Peers => print_json(&serde_json::json!(registry::peers(&state_dir)?))?,
        MailboxCommand::Fleet(args) => fleet::run(args, &host, &state_dir)?,
        MailboxCommand::Reap(args) => {
            print_json(&registry::reap(&state_dir, args.consecutive_failures)?)?
        }
        MailboxCommand::Send(args) => {
            let (receipt, exit) = transport::send(args, &state_dir)?;
            print_json(&receipt)?;
            return Ok(exit);
        }
        MailboxCommand::Flush(args) => {
            let (results, exit) = transport::flush(args, &state_dir)?;
            print_json(&results)?;
            return Ok(exit);
        }
        MailboxCommand::Enqueue(args) => {
            print_json(&queue::run(args, &state_dir)?)?;
        }
        MailboxCommand::Inbox(args) => inbox(args, &state_dir)?,
        MailboxCommand::Watch(args) => return watch::run(args, &state_dir),
        MailboxCommand::Show(args) => show(args, &state_dir)?,
        MailboxCommand::Read(args) => {
            store::validate_message_id(&args.message_id)?;
            let mut store = store::Store::required(&state_dir, &args.identity.as_name, true)?;
            print_json(&store.mark_read(&args.message_id)?)?;
        }
        MailboxCommand::Resolve(args) => {
            store::validate_message_id(&args.message_id)?;
            let mut store = store::Store::required(&state_dir, &args.identity.as_name, true)?;
            print_json(&store.resolve(&args.message_id)?)?;
        }
        MailboxCommand::Hold(args) => {
            store::validate_message_id(&args.message.message_id)?;
            let mut store =
                store::Store::required(&state_dir, &args.message.identity.as_name, true)?;
            print_json(&store.hold(&args.message.message_id, args.reason.as_deref())?)?;
        }
        MailboxCommand::History(args) => history(args, &state_dir)?,
        MailboxCommand::Retention(args) => {
            print_json(&retention::run(args, &state_dir, &host)?)?;
        }
    }
    Ok(0)
}

fn inbox(args: InboxArgs, state_dir: &Path) -> Result<()> {
    ensure!(
        (1..=8).contains(&args.max_messages),
        "--max-messages must be between 1 and 8"
    );
    ensure!(
        (32..=320).contains(&args.preview_chars),
        "--preview-chars must be between 32 and 320"
    );
    ensure!(
        (256..=10_000).contains(&args.max_chars),
        "--max-chars must be between 256 and 10000"
    );
    let store = store::Store::required(state_dir, &args.identity.as_name, false)?;
    let payload = view::inbox(
        &store,
        &args.identity.as_name,
        args.unread,
        args.max_messages,
        args.preview_chars,
        args.max_chars,
    )?;
    if args.json {
        print_json(&payload)?;
    } else {
        println!("{}", view::render_inbox(&payload)?);
    }
    Ok(())
}

fn show(args: ShowArgs, state_dir: &Path) -> Result<()> {
    ensure!(
        (1..=16_777_216).contains(&args.max_bytes),
        "--max-bytes must be between 1 and 16777216"
    );
    store::validate_message_id(&args.message.message_id)?;
    let mut store = store::Store::required(state_dir, &args.message.identity.as_name, false)?;
    let row = store.message(
        &args.message.message_id,
        args.direction.as_str(),
        args.max_bytes,
    )?;
    let output = if args.json {
        view::compact_json(&row)?
    } else {
        view::render_message(&row)?
    };
    ensure!(
        output.len() <= args.max_bytes,
        "rendered message is {} bytes, exceeding --max-bytes {}; content was not printed",
        output.len(),
        args.max_bytes
    );
    println!("{output}");
    Ok(())
}

fn history(args: HistoryArgs, state_dir: &Path) -> Result<()> {
    ensure!(
        (1..=1000).contains(&args.limit),
        "history limit must be between 1 and 1000"
    );
    if let Some(id) = &args.message_id {
        store::validate_message_id(id)?;
    }
    store::registered_identity(state_dir, &args.identity.as_name)?;
    let payload = match store::Store::open(state_dir, &args.identity.as_name, false)? {
        Some(store) => store.history(args.message_id.as_deref(), args.limit)?,
        None => serde_json::json!({"schema_version":1,"available":false,"events":[]}),
    };
    print_json(&payload).context("rendering delivery history")?;
    Ok(())
}
