//! Native CLI for console-launcher repair in existing virtualenvs.
use anyhow::Result;
use clap::Args;
use serde_json::{json, Value};
use std::{os::unix::ffi::OsStrExt, path::PathBuf};

#[derive(Args)]
pub struct RuntimeArgs {
    /// Existing source virtualenv used by exact original shebangs.
    source_venv: PathBuf,
    /// Existing guest virtualenv in which eligible launchers are replaced.
    destination_venv: PathBuf,
    #[arg(long)]
    json: bool,
}
pub fn run(args: RuntimeArgs) -> Result<u8> {
    match execute(&args) {
        Ok(report) => {
            if args.json {
                println!("{}", serde_json::to_string(&report)?);
            } else {
                println!(
                    "relocated {} Python console script(s)",
                    report["relocated_count"]
                );
            }
            Ok(0)
        }
        Err(error) => {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "schema_version": 1, "interface": "forge.worktree-runtime",
                        "outcome": "error", "error": format!("{error:#}")
                    }))?
                );
            }
            // main propagates ChildSignal before its generic error diagnostic.
            if error
                .downcast_ref::<crate::candidate_commit::ChildSignal>()
                .is_some()
            {
                eprintln!("forge worktree-runtime: {error:#}");
            }
            Err(error)
        }
    }
}
fn execute(args: &RuntimeArgs) -> Result<Value> {
    let interrupts = crate::process_signals::Interrupts::install()?;
    let mut cancel = || -> Result<()> {
        if let Some(signal) = interrupts.signal()? {
            return Err(anyhow::Error::new(crate::candidate_commit::ChildSignal(
                signal,
            )));
        }
        Ok(())
    };
    let result = crate::worktree_runtime::relocate_console_scripts(
        &args.source_venv,
        &args.destination_venv,
        &mut cancel,
    );
    // Core publication suppresses cancellation across exchange/validation/cleanup.
    // Decide the final signal outcome before emitting exactly one report.
    if let Some(signal) = interrupts.signal()? {
        let diagnostic = match result {
            Ok(paths) => format!(
                "interrupted after completing {} script(s): {paths:?}",
                paths.len()
            ),
            Err(error) => format!("interrupted during relocation: {error:#}"),
        };
        return Err(
            anyhow::Error::new(crate::candidate_commit::ChildSignal(signal)).context(diagnostic),
        );
    }
    let relocated = result?;
    let paths = relocated
        .iter()
        .map(|path| {
            json!({
                "path": path.to_str(), "path_bytes": path.as_os_str().as_bytes()
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "schema_version": 1, "interface": "forge.worktree-runtime", "outcome": "relocated",
        "relocated_count": relocated.len(), "relocated": paths
    }))
}
