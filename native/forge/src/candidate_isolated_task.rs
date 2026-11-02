//! Explicit isolated Git-tree task execution, independent of the task journal.
#[cfg(target_os = "linux")]
use crate::{
    candidate_execution_workspace::{isolated_task::Prepared, Grants},
    candidate_source::Source,
    confined_execution::Outcome,
    process_signals::Interrupts,
};
use anyhow::Result;
#[cfg(target_os = "linux")]
use anyhow::{ensure, Context};
use clap::Args;
#[cfg(target_os = "linux")]
use serde_json::json;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Args)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub struct IsolatedArgs {
    /// Original Git source, used only for bounded read-only snapshot input.
    #[arg(long)]
    repo: PathBuf,
    /// Complete Git tree OID describing the immutable preimage.
    #[arg(long)]
    tree: String,
    /// Explicit operator-owned execution and delegated-cgroup grants JSON.
    #[arg(long)]
    grants: PathBuf,
    /// Bounded regular output file to hash; not a write restriction.
    #[arg(long = "artifact")]
    artifacts: Vec<String>,
    /// Launch the command. Omit for a non-executing admission preview.
    #[arg(long)]
    execute: bool,
    /// Operator-granted program alias followed by literal arguments.
    #[arg(last = true, required = true)]
    argv: Vec<String>,
}

#[cfg(not(target_os = "linux"))]
pub fn run(_args: IsolatedArgs) -> Result<(Value, u8)> {
    anyhow::bail!("isolated task execution requires Linux sandbox and cgroup support")
}

#[cfg(target_os = "linux")]
pub fn run(args: IsolatedArgs) -> Result<(Value, u8)> {
    // Signal ownership must precede creation of the Tokio worker threads.
    let interrupts = if args.execute {
        Some(Interrupts::install()?)
    } else {
        None
    };
    let source = Source::open(&args.repo)?;
    let grants = Grants::load(&args.grants, &source)?;
    let prepared = Prepared::new(&source, &args.tree, &grants, args.argv, args.artifacts)?;
    if !args.execute {
        let mut output = prepared.preview()?;
        prepared.dry_run()?;
        output["disposition"] = json!("preflight_only");
        output["source_published"] = json!(false);
        return Ok((output, 0));
    }
    let interrupts = interrupts.context("signal owner missing")?;
    ensure!(
        !interrupts.interrupted()?,
        "interrupted before isolated task launch"
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(2)
        .build()?;
    runtime.block_on(execute(prepared, &grants, &interrupts))
}

#[cfg(target_os = "linux")]
async fn execute(
    prepared: Prepared,
    grants: &Grants,
    interrupts: &Interrupts,
) -> Result<(Value, u8)> {
    let signal = interrupts.register_reactor()?;
    let task = prepared.start(grants, Some(interrupts.child_mask()))?;
    let receipt_path = task.receipt_path().to_owned();
    let cancel = task.cancellation();
    let wait = task.wait();
    tokio::pin!(wait);
    let mut signal_errors = Vec::new();
    let mut signal_seen = false;
    let (report, receipt) = tokio::select! {
        result = &mut wait => result?,
        observed = async {
            let mut bytes = [0u8; std::mem::size_of::<libc::signalfd_siginfo>()];
            match signal.read(&mut bytes).await {
                Ok(0) => Err(anyhow::anyhow!("interrupt descriptor closed")),
                Ok(_) => Ok(()),
                Err(error) => Err(error.into()),
            }
        } => {
            signal_seen = true;
            if let Err(error) = observed { signal_errors.push(format!("signal observation: {error:#}")); }
            if let Err(error) = cancel.cancel() { signal_errors.push(format!("cancellation: {error:#}")); }
            wait.await?
        }
    };
    let success = !signal_seen
        && signal_errors.is_empty()
        && report.cleanup_complete
        && report.outcome == Outcome::Exited
        && report.exit_code == Some(0)
        && receipt.retained();
    Ok((
        json!({"executed":true,"report":report,"disposition":receipt,"receipt_path":receipt_path,"signal_seen":signal_seen,"supervisor_errors":signal_errors,"source_published":false}),
        u8::from(!success),
    ))
}
