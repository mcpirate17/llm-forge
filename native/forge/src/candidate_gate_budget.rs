//! Reuse Forge's native ledger computation over the captured candidate baseline.
use super::types::Phase;
use crate::{
    candidate_inputs::BoundFile, candidate_snapshot::Snapshot, candidate_source::Source,
    process_probe, process_signals::Interrupts,
};
use anyhow::{ensure, Context, Result};
use std::{path::Path, process::Command, time::Duration};
const BASELINE: &str = "ledger/cost_budget_baseline.json";
pub(super) struct Budget<'a> {
    snapshot: &'a Snapshot,
    baseline: Option<BoundFile>,
    pub phase: Phase,
    ledger: super::ledger_inputs::LedgerInputs,
}
impl Budget<'_> {
    pub fn verify(&self) -> Result<()> {
        self.ledger.verify(&self.snapshot.path())?;
        let dir = self.snapshot.directory()?;
        if let Some(file) = &self.baseline {
            file.verify()?;
        } else {
            match dir.symlink_metadata(BASELINE) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
                Ok(_) => anyhow::bail!("previously absent cost baseline appeared"),
            }
        }
        Ok(())
    }
}
fn parse_output(output: &std::process::Output) -> Result<Phase> {
    ensure!(
        output.status.success(),
        "native cost budget audit did not run (exit {:?})",
        output.status.code()
    );
    let payload = crate::tool_schema::parse_document(std::str::from_utf8(&output.stdout)?)?;
    let name = payload["name"]
        .as_str()
        .context("cost phase name missing")?;
    ensure!(name == "cost-budget-audit", "unexpected native cost phase");
    let ok = payload["ok"]
        .as_bool()
        .context("cost phase status missing")?;
    let detail = payload["detail"]
        .as_str()
        .context("cost phase detail missing")?
        .to_owned();
    Ok(Phase::new(name, ok, detail, payload["evidence"].clone()))
}
pub(super) fn collect<'a>(
    source: &Source,
    snapshot: &'a Snapshot,
    interrupts: &Interrupts,
) -> Result<Budget<'a>> {
    let entries = source.list_tree(&snapshot.report().tree_oid)?;
    let baseline = if let Some(entry) = entries.iter().find(|entry| entry.path == BASELINE) {
        ensure!(
            matches!(entry.mode.as_str(), "100644" | "100755"),
            "cost baseline must be a regular candidate blob"
        );
        Some(
            crate::candidate_inputs::snapshot(
                &snapshot.path(),
                BASELINE,
                &entry.oid,
                2 * 1024 * 1024,
            )?
            .0,
        )
    } else {
        None
    };
    let ledger = super::ledger_inputs::LedgerInputs::capture(&snapshot.path())?;
    let mut command = Command::new(Path::new("/proc/self/exe"));
    command.current_dir(snapshot.path());
    command
        .args(["ledger", "budget", "--host"])
        .arg(snapshot.path())
        .arg("gate");
    let output = process_probe::run_interruptible(
        &mut command,
        Duration::from_secs(60),
        1024 * 1024,
        interrupts,
    )?;
    let phase = parse_output(&output)?;
    let result = Budget {
        snapshot,
        baseline,
        ledger,
        phase,
    };
    result.verify()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    #[test]
    fn native_budget_phase_retains_actual_evidence_and_rejects_failed_process() {
        let stdout=br#"{"name":"cost-budget-audit","ok":false,"detail":"REGRESSION","evidence":{"status":"REGRESSION"}}"#.to_vec();
        let output = std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout,
            stderr: vec![],
        };
        let phase = parse_output(&output).unwrap();
        assert!(!phase.ok);
        assert_eq!(phase.evidence["status"], "REGRESSION");
        let failed = std::process::Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: output.stdout,
            stderr: vec![],
        };
        assert!(parse_output(&failed).is_err());
    }
}
