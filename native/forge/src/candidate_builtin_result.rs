use super::*;
use crate::candidate_assessment::{self, CheckResult, CheckStatus, Finding};
use serde::Deserialize;
use serde_json::Map;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observations {
    findings: Vec<Finding>,
    #[serde(default)]
    metrics: Map<String, Value>,
}
pub(super) fn finish(
    id: &str,
    files: Vec<String>,
    value: Value,
    started: Instant,
) -> Result<Value> {
    let mut observations: Observations = serde_json::from_value(value)?;
    ensure!(
        observations.findings.len() <= 100_000,
        "builtin findings exceed 100000"
    );
    for finding in &mut observations.findings {
        ensure!(
            finding.check_id == id,
            "builtin returned another check's finding"
        );
        candidate_assessment::finalize_finding(finding);
    }
    let result = CheckResult {
        check_id: id.into(),
        status: if observations.findings.is_empty() {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        },
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        findings: observations.findings,
        files,
        command: Vec::new(),
        tool_version: Some(IMPLEMENTATION.into()),
        cache_key: None,
        cache_hit: false,
        exit_code: None,
        skipped_reason: None,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        metrics: observations.metrics,
    };
    let value = serde_json::to_value(result)?;
    crate::json_canon::encoded_size(&value, MAX_RESULT_BYTES)?;
    Ok(value)
}
