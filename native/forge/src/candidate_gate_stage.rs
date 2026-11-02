//! Execute candidate-controlled probes only under an operator-granted Stage.
use crate::{
    candidate_command::PreparedArgv,
    candidate_execution_plan::{Selection, Stage},
    candidate_execution_workspace::Workspace,
    confined_execution::{Limits, Report},
    process_signals::Interrupts,
};
use anyhow::{ensure, Context, Result};
use serde_json::json;
use std::time::Duration;
pub(super) fn run(
    selection: &Selection,
    interrupts: &Interrupts,
    runtime: &tokio::runtime::Runtime,
    seconds: u64,
    create_empty: bool,
    build: impl FnOnce(&Workspace) -> Result<Vec<String>>,
) -> Result<Report> {
    let source = selection.source()?;
    let workspace = Workspace::new(&source, &selection.candidate.tree_oid, &selection.grants)?;
    if create_empty {
        let root = workspace.directory()?;
        match root.symlink_metadata(".gate-empty-collect") {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
            Ok(_) => anyhow::bail!("candidate occupies pytest empty-collection path"),
        }
        root.create_dir(".gate-empty-collect")?;
    }
    let argv = build(&workspace)?;
    ensure!(
        !argv.is_empty()
            && argv.len() <= 4096
            && argv.iter().all(|s| s.len() <= 4096 && !s.contains('\0')),
        "probe argv exceeds limits or contains NUL"
    );
    ensure!(
        argv.iter().map(|s| s.len()).sum::<usize>() <= 1024 * 1024,
        "probe argv exceeds byte limit"
    );
    let prepared = PreparedArgv {
        argv: argv.clone(),
        changed_files: None,
    };
    let mut plan = workspace.plan(&prepared, &selection.grants)?;
    plan.command()
        .env("PYTHONPYCACHEPREFIX", workspace.runtime().join("pycache"));
    let wall = seconds.min(selection.grants.max_wall_seconds);
    let cpu = seconds.min(selection.grants.max_cpu_seconds);
    ensure!(wall > 0 && cpu > 0, "probe has no admitted runtime budget");
    let limits = Limits {
        timeout: Duration::from_secs(wall),
        max_output_chars: 64 * 1024,
        cpu_seconds: Some(cpu),
        address_space_bytes: Some(
            selection
                .grants
                .max_address_space_mib
                .checked_mul(1024 * 1024)
                .context("memory limit overflow")?,
        ),
    };
    let stage = Stage {
        workspace,
        plan,
        limits,
        argv,
        data: json!({"stage":"gate-probe"}),
    };
    let (_, report, _) = runtime.block_on(stage.execute(&selection.grants, interrupts))?;
    ensure!(report.cleanup_complete, "probe cleanup incomplete");
    Ok(report)
}
