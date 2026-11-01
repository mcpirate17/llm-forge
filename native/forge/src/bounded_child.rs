//! Bounded native child invocation shared by ledger completion hooks.
use anyhow::{Context, Result};
use std::process::Command;
use std::time::Duration;

pub fn run_bounded(args: &[String], timeout: Duration, action: &str) -> Result<()> {
    let exe = std::env::current_exe().context("locating the forge binary")?;
    let mut command = Command::new(exe);
    command.args(args);
    let output = forge::process_probe::run(&mut command, timeout, 256 * 1024)
        .with_context(|| format!("running {action}"))?;
    anyhow::ensure!(
        output.status.success(),
        "{action} exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}
