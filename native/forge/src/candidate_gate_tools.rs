//! Declared tool versions resolved solely through operator execution grants.
use super::{
    stage,
    types::{Phase, ToolStatus},
};
use crate::{
    candidate_execution_plan::Selection, confined_execution::Outcome, process_signals::Interrupts,
};
use anyhow::{Context, Result};
use serde_json::{json, Value};
fn string<'a>(row: &'a Value, key: &str) -> Result<&'a str> {
    row[key]
        .as_str()
        .with_context(|| format!("tool {key} missing"))
}
fn probe(
    row: &Value,
    selection: &Selection,
    interrupts: &Interrupts,
    runtime: &tokio::runtime::Runtime,
) -> Result<ToolStatus> {
    let id = string(row, "tool_id")?;
    let executable = string(row, "executable")?;
    let expected = string(row, "expected_version")?;
    let provided = string(row, "provided_by")?;
    let program = selection.grants.programs.get(executable);
    let mut version = None;
    let command = row["version_command"]
        .as_array()
        .context("tool version command missing")?;
    if program.is_some() && !command.is_empty() {
        let mut argv = vec![executable.to_owned()];
        for item in command.iter().skip(1) {
            argv.push(item.as_str().context("tool argv must be strings")?.into());
        }
        let report = stage::run(selection, interrupts, runtime, 30, false, |_| Ok(argv))?;
        if report.outcome == Outcome::Exited && report.exit_code == Some(0) {
            let combined = format!("{}{}", report.stdout, report.stderr);
            version = Some(
                combined
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(120)
                    .collect::<String>(),
            );
        }
    }
    let matches = version
        .as_deref()
        .is_some_and(|value| !value.is_empty() && value.contains(expected));
    Ok(ToolStatus {
        tool_id: id.into(),
        executable: executable.into(),
        found: program.is_some(),
        resolved_path: program.map(|path| path.to_string_lossy().into_owned()),
        version,
        expected_version: expected.into(),
        matches_expected: matches,
        provided_by: provided.into(),
    })
}
pub(super) fn run(
    selection: &Selection,
    interrupts: &Interrupts,
    runtime: &tokio::runtime::Runtime,
) -> Result<(Phase, Vec<ToolStatus>)> {
    let rows = selection.policy.document["tools"]
        .as_array()
        .context("candidate policy tools missing")?;
    anyhow::ensure!(rows.len() <= 128, "declared tool inventory exceeds 128");
    let mut statuses = Vec::new();
    for row in rows {
        let required = row["required_profiles"]
            .as_array()
            .context("tool profiles missing")?;
        if required
            .iter()
            .any(|profile| profile.as_str() == Some(selection.profile()))
        {
            statuses.push(probe(row, selection, interrupts, runtime)?);
        }
    }
    let phase = summarize(&statuses);
    Ok((phase, statuses))
}

fn summarize(statuses: &[ToolStatus]) -> Phase {
    let missing: Vec<_> = statuses
        .iter()
        .filter(|status| !status.found)
        .map(|status| status.tool_id.clone())
        .collect();
    let drifted: Vec<_> = statuses
        .iter()
        .filter(|status| {
            status.found && !status.expected_version.is_empty() && !status.matches_expected
        })
        .map(|status| status.tool_id.clone())
        .collect();
    let phase = if !missing.is_empty() {
        Phase::new(
            "tool-preflight",
            false,
            format!(
                "{} declared tool(s) unavailable or not granted",
                missing.len()
            ),
            json!({"missing":missing}),
        )
    } else if !drifted.is_empty() {
        Phase::new(
            "tool-preflight",
            false,
            format!(
                "{} declared tool(s) differ from pinned versions",
                drifted.len()
            ),
            json!({"drifted":drifted}),
        )
    } else {
        Phase::new(
            "tool-preflight",
            true,
            format!("{} declared tool(s) present", statuses.len()),
            json!({"drifted":[]}),
        )
    };
    phase
}

#[cfg(test)]
#[path = "candidate_gate_tools_tests.rs"]
mod tests;
