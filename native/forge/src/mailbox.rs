//! Native inspection and acknowledgment of existing local A2A mailboxes.

#[path = "mailbox_queue.rs"]
mod queue;
#[path = "mailbox_retention.rs"]
mod retention;
#[path = "mailbox_store.rs"]
mod store;
#[path = "mailbox_view.rs"]
mod view;

use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Args)]
pub struct MailboxArgs {
    /// Host containing .agents/a2a. Mailbox commands never initialize stores.
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
    /// Queue one outbound message durably for a later transport flush.
    Enqueue(queue::EnqueueArgs),
    /// Read bounded inbox summaries without marking anything read or presented.
    Inbox(InboxArgs),
    /// Explicitly retrieve one complete message, refusing oversized content.
    Show(ShowArgs),
    /// Atomically mark one unread inbound message read.
    Read(MessageArgs),
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
        MailboxCommand::Enqueue(args) => {
            print_json(&queue::run(args, &state_dir)?)?;
        }
        MailboxCommand::Inbox(args) => {
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
            let store = store::Store::required(&state_dir, &args.identity.as_name, false)?;
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
        }
        MailboxCommand::Show(args) => {
            ensure!(
                (1..=16_777_216).contains(&args.max_bytes),
                "--max-bytes must be between 1 and 16777216"
            );
            store::validate_message_id(&args.message.message_id)?;
            let mut store =
                store::Store::required(&state_dir, &args.message.identity.as_name, false)?;
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
        }
        MailboxCommand::Read(args) => {
            store::validate_message_id(&args.message_id)?;
            let mut store = store::Store::required(&state_dir, &args.identity.as_name, true)?;
            print_json(&store.mark_read(&args.message_id)?)?;
        }
        MailboxCommand::History(args) => {
            ensure!(
                (1..=1000).contains(&args.limit),
                "history limit must be between 1 and 1000"
            );
            if let Some(id) = &args.message_id {
                store::validate_message_id(id)?;
            }
            store::registered_identity(&state_dir, &args.identity.as_name)?;
            let payload = match store::Store::open(&state_dir, &args.identity.as_name, false)? {
                Some(store) => store.history(args.message_id.as_deref(), args.limit)?,
                None => serde_json::json!({"schema_version":1,"available":false,"events":[]}),
            };
            print_json(&payload).context("rendering delivery history")?;
        }
        MailboxCommand::Retention(args) => {
            print_json(&retention::run(args, &state_dir, &host)?)?;
        }
    }
    Ok(0)
}
