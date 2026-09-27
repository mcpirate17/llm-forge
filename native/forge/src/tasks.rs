//! Native task CLI, shared by manual agent work and supervised commands.

use crate::task_store::{self, Completion, Receipt, State, Store, Task};
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct TaskArgs {
    /// Host project root. All task state stays under .agents/forge in this host.
    #[arg(long, default_value = ".", global = true)]
    pub host: PathBuf,
    #[command(subcommand)]
    action: TaskCommand,
}

#[derive(Subcommand)]
enum TaskCommand {
    /// Persist a task; an owner makes it assigned, otherwise it is queued.
    Create(CreateArgs),
    /// List the latest tasks (JSON includes total counts and truncation).
    List {
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Show a task and its latest history entries.
    Show { id: String },
    /// Assign a queued or already assigned task to an agent.
    Assign(OwnedArgs),
    /// Begin an externally executed task and return its fenced attempt number.
    Start(LeaseArgs),
    /// Renew an external worker's current attempt lease.
    Heartbeat(HeartbeatArgs),
    /// Record external completion and optionally bind an existing receipt.
    Finish(FinishArgs),
    /// Record external failure with an explanation.
    Fail(FailArgs),
    /// Requeue a failure or expired lease within the original attempt budget.
    Retry(OwnedArgs),
    /// Cancel work; a supervised command stops on its next lease check.
    Cancel(OwnedArgs),
    /// Run the stored argv with a deadline, bounded logs, and lease heartbeats.
    Run(RunArgs),
}

#[derive(Args)]
pub struct CreateArgs {
    #[arg(long)]
    id: String,
    #[arg(long)]
    title: String,
    #[arg(long)]
    owner: Option<String>,
    #[arg(long, default_value = "operator")]
    actor: String,
    #[arg(long, default_value_t = 3)]
    max_attempts: u32,
    #[arg(long)]
    depends_on: Vec<String>,
    #[arg(long)]
    claim: Option<String>,
    #[arg(long)]
    session: Option<String>,
    #[arg(long)]
    message: Option<String>,
    /// Optional executable and literal arguments after -- (no implicit shell).
    #[arg(last = true)]
    command: Vec<String>,
}

#[derive(Args)]
pub struct OwnedArgs {
    pub id: String,
    #[arg(long)]
    pub owner: String,
}

#[derive(Args)]
pub struct LeaseArgs {
    #[command(flatten)]
    pub task: OwnedArgs,
    #[arg(long, default_value_t = 30)]
    pub lease_seconds: i64,
}

#[derive(Args)]
pub struct HeartbeatArgs {
    #[command(flatten)]
    lease: LeaseArgs,
    #[arg(long)]
    attempt: u32,
}

#[derive(Args)]
pub struct FinishArgs {
    #[command(flatten)]
    task: OwnedArgs,
    #[arg(long)]
    attempt: u32,
    #[arg(long)]
    receipt: Option<PathBuf>,
}

#[derive(Args)]
pub struct FailArgs {
    #[command(flatten)]
    task: OwnedArgs,
    #[arg(long)]
    attempt: u32,
    #[arg(long)]
    reason: String,
}

#[derive(Args)]
pub struct RunArgs {
    #[command(flatten)]
    pub lease: LeaseArgs,
    /// Wall-clock bound for this attempt, including child processes.
    #[arg(long, default_value_t = 300)]
    pub timeout_seconds: u64,
    /// Explicitly recover a failed/expired attempt and rerun its argv.
    #[arg(long)]
    pub resume: bool,
    /// Maximum retained bytes per output stream; excess is drained/discarded.
    #[arg(long, default_value_t = 1_048_576)]
    pub log_bytes: u64,
}

pub fn now() -> i64 {
    crate::instant::now() as i64
}

pub fn bind_receipt(host: &Path, path: &Path) -> Result<Receipt> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        host.join(path)
    };
    let path = path
        .canonicalize()
        .with_context(|| format!("resolving receipt {}", path.display()))?;
    let mut file = std::fs::File::open(&path)?;
    ensure!(file.metadata()?.is_file(), "receipt must be a regular file");
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(Receipt {
        path,
        sha256: format!("{:x}", hasher.finalize()),
    })
}

pub fn validate_claim(host: &Path, task: &Task, owner: &str) -> Result<()> {
    let Some(id) = &task.claim_id else {
        return Ok(());
    };
    let claims = crate::active_state::parse_active_claims(host)?;
    ensure!(
        claims
            .iter()
            .any(|claim| claim.claim_id == *id && claim.owner == owner),
        "task claim {id} is not active for owner {owner}"
    );
    Ok(())
}

fn create(args: CreateArgs, store: &mut Store) -> Result<Task> {
    let at = now();
    let task = Task {
        id: args.id,
        title: args.title,
        state: if args.owner.is_some() {
            State::Assigned
        } else {
            State::Queued
        },
        owner: args.owner,
        command: args.command,
        depends_on: args.depends_on,
        claim_id: args.claim,
        session_id: args.session,
        message_id: args.message,
        attempt: 0,
        max_attempts: args.max_attempts,
        lease_until: None,
        created_at: at,
        updated_at: at,
        exit_code: None,
        detail: None,
        receipt: None,
    };
    store.create(task, &args.actor)
}

fn readonly(action: &TaskCommand, host: &Path) -> Result<Option<serde_json::Value>> {
    match action {
        TaskCommand::List { limit } => {
            ensure!((1..=1000).contains(limit), "limit must be 1..1000");
            match Store::open(host, false)? {
                Some(store) => {
                    let rows = store.list(*limit)?;
                    let counts = store.counts()?;
                    let total: i64 = counts.values().sum();
                    Ok(Some(
                        json!({"tasks":rows,"counts":counts,"total":total,"truncated":total > rows.len() as i64}),
                    ))
                }
                None => Ok(Some(
                    json!({"tasks":[],"counts":{},"total":0,"truncated":false}),
                )),
            }
        }
        TaskCommand::Show { id } => {
            let store = task_store::required(host)?;
            Ok(Some(
                json!({"task":store.get(id)?,"events":store.events(id,100)?,"event_limit":100}),
            ))
        }
        _ => Ok(None),
    }
}

pub fn run(args: TaskArgs) -> Result<u8> {
    let host = args.host.canonicalize().context("resolving --host")?;
    ensure!(host.is_dir(), "--host must be a directory");
    if let Some(output) = readonly(&args.action, &host)? {
        println!("{}", serde_json::to_string(&output)?);
        return Ok(0);
    }
    let mut store = Store::open(&host, true)?.context("opening task store")?;
    if let TaskCommand::Run(run) = args.action {
        return crate::task_run::run(&host, &mut store, run);
    }
    let task = change(args.action, &host, &mut store)?;
    println!("{}", serde_json::to_string(&task)?);
    Ok(0)
}

fn change(action: TaskCommand, host: &Path, store: &mut Store) -> Result<Task> {
    match action {
        TaskCommand::Create(args) => create(args, store),
        TaskCommand::Assign(args) => store.assign(&args.id, &args.owner, now()),
        TaskCommand::Start(args) => {
            validate_claim(host, &store.get(&args.task.id)?, &args.task.owner)?;
            store.start(&args.task.id, &args.task.owner, args.lease_seconds, now())
        }
        TaskCommand::Heartbeat(args) => {
            let owned = &args.lease.task;
            validate_claim(host, &store.get(&owned.id)?, &owned.owner)?;
            store.heartbeat(
                &owned.id,
                &owned.owner,
                args.attempt,
                args.lease.lease_seconds,
                now(),
            )
        }
        TaskCommand::Finish(args) => {
            let receipt = args
                .receipt
                .map(|path| bind_receipt(host, &path))
                .transpose()?;
            store.finish(
                &args.task.id,
                &args.task.owner,
                args.attempt,
                Completion {
                    state: State::Succeeded,
                    exit_code: None,
                    detail: Some("reported by external worker".into()),
                    receipt,
                },
                now(),
            )
        }
        TaskCommand::Fail(args) => {
            ensure!(
                !args.reason.trim().is_empty(),
                "failure reason must not be empty"
            );
            store.finish(
                &args.task.id,
                &args.task.owner,
                args.attempt,
                Completion {
                    state: State::Failed,
                    exit_code: None,
                    detail: Some(args.reason),
                    receipt: None,
                },
                now(),
            )
        }
        TaskCommand::Retry(args) => store.retry(&args.id, &args.owner, now()),
        TaskCommand::Cancel(args) => store.cancel(&args.id, &args.owner, now()),
        _ => anyhow::bail!("expected task mutation command"),
    }
}
