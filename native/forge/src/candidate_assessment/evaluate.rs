//! Recompute exceptions, attribution, and threshold decision for admitted results.
use super::{
    admission::{ContextData, Exception},
    model::StaleException,
    Assessment, CheckResult, CheckStatus, Finding,
};
use anyhow::{ensure, Context, Result};
use conductor_native::candidate_policy::GlobWorkspace;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const MAX_GLOB_WORK: u64 = 100_000_000;

fn infrastructure(rule: &str) -> bool {
    matches!(
        rule,
        "policy-engine-crash"
            | "analyzer-crash"
            | "analyzer-timeout"
            | "required-analyzer-unavailable"
            | "targeted-test-crash"
            | "targeted-test-timeout"
            | "targeted-test-killed"
            | "slop-core-unavailable"
    )
}
pub(super) fn fingerprint(finding: &Finding) -> String {
    let stable = json!({"check_id": finding.check_id, "rule_id": finding.rule_id,
        "path": finding.path, "line": finding.line, "message": finding.message});
    let text = crate::json_canon::canonical_json(&stable);
    format!("{:x}", Sha256::digest(text.as_bytes()))[..24].to_owned()
}
#[derive(Default)]
struct Work {
    spent: u64,
    workspace: GlobWorkspace,
}
impl Work {
    fn matches(&mut self, path: &str, exception: &Exception<'_>) -> Result<bool> {
        let cost = (path.chars().count() as u64 + 1)
            .checked_mul(exception.pattern_chars as u64 + 1)
            .context("assessment glob cost overflow")?;
        self.spent = self
            .spent
            .checked_add(cost)
            .context("assessment glob work overflow")?;
        ensure!(
            self.spent <= MAX_GLOB_WORK,
            "assessment exception matching exceeds glob work limit"
        );
        Ok(self.workspace.matches(path, &exception.glob))
    }
}
fn exception_matches(
    exception: &Exception<'_>,
    finding: &Finding,
    work: &mut Work,
) -> Result<bool> {
    if exception.rule.is_some_and(|rule| rule != finding.rule_id) {
        return Ok(false);
    }
    if let Some(hash) = exception.fingerprint {
        return Ok(hash == finding.fingerprint);
    }
    match &finding.path {
        Some(path) => work.matches(path, exception),
        None => Ok(false),
    }
}
fn apply_one(finding: &mut Finding, context: &ContextData<'_>, work: &mut Work) -> Result<()> {
    let mut matched = None;
    if let Some(indexes) = context.groups.get(finding.check_id.as_str()) {
        for index in indexes {
            let exception = &context.exceptions[*index];
            if exception_matches(exception, finding, work)? {
                ensure!(
                    matched.is_none(),
                    "finding {} matches multiple exceptions",
                    finding.fingerprint
                );
                matched = Some(exception.id.to_owned());
            }
        }
    }
    finding.exception_id = matched;
    finding.inherited = context
        .checks
        .get(finding.check_id.as_str())
        .copied()
        .unwrap_or(false)
        && finding
            .path
            .as_deref()
            .is_none_or(|path| !context.changed.contains(path));
    Ok(())
}
fn stale(
    context: &ContextData<'_>,
    findings: &[Finding],
    work: &mut Work,
) -> Result<Vec<StaleException>> {
    let matched: BTreeSet<_> = findings
        .iter()
        .filter_map(|finding| finding.exception_id.as_deref())
        .collect();
    let mut stale = Vec::new();
    // Retain original policy order even though finding comparisons are grouped by check.
    for exception in &context.exceptions {
        if matched.contains(exception.id) {
            continue;
        }
        let mut seen = false;
        if let Some(paths) = context.examined.get(exception.check) {
            for path in paths {
                if work.matches(path, exception)? {
                    seen = true;
                    break;
                }
            }
        }
        if seen {
            stale.push(StaleException {
                id: exception.id.into(),
                check: exception.check.into(),
                rule: exception.rule.unwrap_or("").into(),
                path: exception.path.into(),
                owner: exception.owner.into(),
                expires: exception.expires.into(),
            });
        }
    }
    Ok(stale)
}
pub(super) fn assess(context: ContextData<'_>, results: &[CheckResult]) -> Result<Assessment> {
    let mut findings = Vec::new();
    let mut work = Work::default();
    let mut blocked = false;
    for result in results {
        let failed_engine = result.check_id == "engine-integrity" && !result.findings.is_empty();
        if result.status == CheckStatus::Error || failed_engine {
            blocked = true;
        }
        for original in &result.findings {
            let mut finding = original.clone();
            finding.exception_id = None;
            finding.inherited = false;
            if finding.fingerprint.is_empty() {
                finding.fingerprint = fingerprint(&finding);
            }
            let forced = result.status == CheckStatus::Error
                || failed_engine
                || infrastructure(&finding.rule_id);
            apply_one(&mut finding, &context, &mut work)?;
            if forced {
                finding.exception_id = None;
                finding.inherited = false;
                blocked = true;
            } else if finding.exception_id.is_none()
                && !finding.inherited
                && finding.severity >= context.threshold
            {
                blocked = true;
            }
            findings.push(finding);
        }
    }
    let stale_exceptions = stale(&context, &findings, &mut work)?;
    Ok(Assessment {
        decision: if blocked { "fail" } else { "pass" },
        findings,
        stale_exceptions,
        scope: "supplied_results",
        review_complete: false,
        execution_verified: false,
    })
}
