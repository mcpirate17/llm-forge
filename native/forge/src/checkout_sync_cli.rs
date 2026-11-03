//! Public native checkout transaction and conditional recovery entrypoints.
use crate::{
    candidate_source::Source,
    checkout_sync_forward::{self, Outcome},
    checkout_sync_plan::Blocked,
    checkout_sync_preflight::Preflight,
    checkout_sync_preview,
    checkout_sync_recovery::{self, RecoveryOutcome},
    process_signals::Interrupts,
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Args)]
pub struct CheckoutSyncArgs {
    /// Checkout root (defaults to the current directory).
    #[arg(long)]
    repo: Option<PathBuf>,
    /// Existing named integration ref; resolved locally without fetch/network.
    #[arg(long)]
    target: Option<String>,
    /// Read and reconcile without writing snapshots, objects, refs or checkout bytes.
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    json: bool,
    /// Existing trusted, owned mode-0700 parent for retained recovery data.
    #[arg(long, conflicts_with = "dry_run")]
    recovery_dir: Option<PathBuf>,
    /// Exact existing recovery directory containing journal.jsonl.
    #[arg(long, conflicts_with_all = ["repo", "target", "dry_run", "recovery_dir"])]
    recover: Option<PathBuf>,
}

pub fn run(args: CheckoutSyncArgs) -> Result<u8> {
    let json_output = args.json;
    match run_inner(args) {
        Ok((report, code)) => emit(&report, json_output, code),
        Err(error) => {
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "schema_version": 1, "interface": "forge.checkout-sync",
                        "outcome": "error", "error": format!("{error:#}")
                    }))?
                );
            }
            // command_exit propagates signals before its generic diagnostic.
            // Preserve retained-recovery evidence for both text and JSON callers.
            if error
                .downcast_ref::<crate::candidate_commit::ChildSignal>()
                .is_some()
            {
                eprintln!("forge checkout-sync: {error:#}");
            }
            Err(error)
        }
    }
}
fn run_inner(args: CheckoutSyncArgs) -> Result<(Value, u8)> {
    // The offline integration resolver uses bounded Git read probes. Resolve
    // before installing the signal supervisor so no child inherits its mask.
    let target = if args.recover.is_none() {
        let root = args.repo.as_deref().unwrap_or(Path::new("."));
        let requested = match &args.target {
            Some(value) => value.clone(),
            None => crate::workspace_integration::default_integration_ref(root, false)?,
        };
        Some(resolve_target(root, &requested)?)
    } else {
        None
    };
    let interrupts = Interrupts::install()?;
    let mut cancel = || -> Result<()> {
        if let Some(signal) = interrupts.signal()? {
            return Err(anyhow::Error::new(crate::candidate_commit::ChildSignal(
                signal,
            )));
        }
        Ok(())
    };
    cancel()?;
    let result = match &args.recover {
        Some(directory) => recover(directory, &mut cancel),
        None => sync(
            &args,
            target.as_deref().context("integration ref missing")?,
            &mut cancel,
        ),
    };
    // Forward failure cleanup deliberately ignores cancellation while restoring
    // safety. Reflect the original signal only after every actor/lease drops.
    if let Some(signal) = interrupts.signal()? {
        let error = anyhow::Error::new(crate::candidate_commit::ChildSignal(signal));
        let context = match result {
            Err(actor_error) => format!("interrupted; actor diagnostic: {actor_error:#}"),
            Ok((report, _)) => format!("interrupted after actor result: {report}"),
        };
        return Err(error.context(context));
    }
    result
}
fn resolve_target(root: &Path, requested: &str) -> Result<String> {
    let source = Source::open(root)?;
    let repo = source.repository();
    let reference = if requested.starts_with("refs/") {
        repo.find_reference(requested)?
    } else {
        repo.resolve_reference_from_short_name(requested)?
    };
    let direct = reference.resolve()?;
    let name = direct.name().context("integration ref is not UTF-8")?;
    ensure!(
        name.starts_with("refs/") && direct.target().is_some(),
        "integration ref is not direct"
    );
    Ok(name.to_owned())
}
fn recovery_parent(path: Option<&Path>) -> Result<&Path> {
    let path =
        path.context("normal checkout-sync requires --recovery-dir EXISTING_PRIVATE_PARENT")?;
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o777 == 0o700,
        "--recovery-dir must be an existing owned mode-0700 directory"
    );
    Ok(path)
}
fn sync(
    args: &CheckoutSyncArgs,
    target: &str,
    cancel: &mut dyn FnMut() -> Result<()>,
) -> Result<(Value, u8)> {
    let root = args.repo.as_deref().unwrap_or(Path::new("."));
    let mut report = report(root, target, args.dry_run)?;
    if report["ahead"].as_u64().context("ahead count missing")? > 0 {
        report["outcome"] = json!("blocked");
        report["fast_forwardable"] = json!(false);
        report["blocked_by"] = json!(["checkout has commits not on integration line"]);
        return Ok((report, 3));
    }
    let preflight = Preflight::open(root, target)?;
    ensure!(
        report["old"].as_str() == Some(preflight.old.to_string().as_str())
            && report["new"].as_str() == Some(preflight.new.to_string().as_str())
            && report["branch"].as_str() == Some(preflight.branch.as_str())
            && report["behind"].as_u64() == Some(preflight.behind as u64),
        "checkout authority changed while preparing report"
    );
    if preflight.behind == 0 {
        cancel()?;
        preflight.verify()?;
        report["outcome"] = json!("even");
        report["fast_forwardable"] = json!(true);
        return Ok((report, 0));
    }
    let recovery = if args.dry_run {
        None
    } else {
        Some(recovery_parent(args.recovery_dir.as_deref())?)
    };
    let plan = match checkout_sync_preview::inspect(&preflight, cancel) {
        Ok(plan) => plan,
        Err(error) if error.downcast_ref::<Blocked>().is_some() => {
            report["outcome"] = json!("blocked");
            report["fast_forwardable"] = json!(false);
            report["blocked_by"] = json!([format!("{error:#}")]);
            return Ok((report, 3));
        }
        Err(error) => return Err(error),
    };
    report["planned_index_paths"] = json!(plan.index.len());
    report["planned_worktree_operations"] = json!(plan.operations.len());
    drop(plan);
    report["fast_forwardable"] = json!(true);
    if args.dry_run {
        report["outcome"] = json!("would-fast-forward");
    } else {
        cancel()?;
        match checkout_sync_forward::execute(
            &preflight,
            recovery.context("recovery parent missing")?,
            cancel,
        )? {
            Outcome::Even => report["outcome"] = json!("even"),
            Outcome::Published { recovery, snapshot } => {
                report["outcome"] = json!("fast-forwarded");
                report["recovery_directory"] = json!(recovery);
                report["snapshot"] = json!(snapshot);
            }
        }
    }
    Ok((report, 0))
}
fn report(root: &Path, target: &str, dry_run: bool) -> Result<Value> {
    let source = Source::open(root)?;
    let repo = source.repository();
    let head = repo.find_reference("HEAD")?;
    let branch = head
        .symbolic_target()?
        .context("detached HEAD prevents checkout sync")?;
    let old = repo
        .find_reference(branch)?
        .target()
        .context("branch is not direct")?;
    let new = repo
        .find_reference(target)?
        .target()
        .context("integration ref is not direct")?;
    let (ahead, behind) = repo.graph_ahead_behind(old, new)?;
    Ok(json!({
        "schema_version": 1, "interface": "forge.checkout-sync", "outcome": "pending",
        "dry_run": dry_run, "branch": branch, "target": target,
        "old": old.to_string(), "new": new.to_string(), "ahead": ahead, "behind": behind,
        "fast_forwardable": null, "blocked_by": [], "snapshot": null,
        "recovery_directory": null, "planned_index_paths": null, "planned_worktree_operations": null
    }))
}
fn recover(directory: &Path, cancel: &mut dyn FnMut() -> Result<()>) -> Result<(Value, u8)> {
    let outcome = checkout_sync_recovery::recover(directory, cancel)?;
    let (status, depot, slots) = match outcome {
        RecoveryOutcome::JournalMarkedComplete => ("journal-marked-complete", None, Vec::new()),
        RecoveryOutcome::JournalMarkedRecovered => ("journal-marked-recovered", None, Vec::new()),
        RecoveryOutcome::Recovered {
            depot,
            recorded_slots,
        } => ("recovered", depot, recorded_slots),
    };
    Ok((
        json!({
            "schema_version": 1, "interface": "forge.checkout-sync", "outcome": status,
            "recovery_directory": directory, "retained_depot": depot, "recorded_slots": slots
        }),
        0,
    ))
}
fn emit(report: &Value, json_output: bool, code: u8) -> Result<u8> {
    if json_output {
        println!("{}", serde_json::to_string(report)?);
    } else {
        println!(
            "checkout-sync: {}",
            report["outcome"].as_str().context("outcome missing")?
        );
        if let Some(reasons) = report["blocked_by"].as_array() {
            for reason in reasons {
                println!("  {}", reason.as_str().context("blocked reason missing")?);
            }
        }
        for field in ["snapshot", "recovery_directory", "retained_depot"] {
            if let Some(value) = report[field].as_str() {
                println!("  {field}: {value}");
            }
        }
    }
    Ok(code)
}
