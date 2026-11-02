//! Ownership observations from live operator host state, not candidate JSON.
#[path = "candidate_review_ownership_state.rs"]
mod state;
#[cfg(test)]
#[path = "candidate_review_ownership_tests.rs"]
mod tests;
use crate::{
    candidate_assessment::{self, CheckResult, CheckStatus, Finding},
    candidate_execution_plan::Surface,
    candidate_policy::Policy,
    candidate_snapshot::Snapshot,
    candidate_source::{Candidate, Source},
    candidate_test_selection::{self, inputs::Inputs},
    ownership,
};
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use state::{HostState, Observation};
use std::time::Instant;

/// The coordinator resolves owner identity; an empty owner preserves legacy
/// coverage-only checks. No model output is accepted as authority here.
pub(crate) struct Context<'a> {
    pub source: &'a Source,
    pub snapshot: &'a Snapshot,
    pub candidate: &'a Candidate,
    pub policy: &'a Policy,
    pub today: &'a str,
    pub surface: Surface,
    pub owner: &'a str,
}
pub(crate) struct Evidence<'a> {
    inputs: Inputs<'a>,
    candidate: &'a Candidate,
    candidate_digest: String,
    host: Option<HostState>,
    observation: Value,
    observed_at: f64,
    output_digest: String,
    pub result: CheckResult,
    pub evidence: Value,
}
impl Evidence<'_> {
    pub fn verify(&self) -> Result<()> {
        self.verify_at(crate::instant::now())
    }
    fn verify_at(&self, now: f64) -> Result<()> {
        ensure!(
            now.is_finite() && now >= self.observed_at,
            "ownership observation clock moved backwards"
        );
        self.inputs.verify()?;
        ensure!(
            candidate_test_selection::digest_json(self.candidate)? == self.candidate_digest,
            "ownership candidate changed"
        );
        ensure!(
            candidate_test_selection::digest_json(&(&self.result, &self.evidence))?
                == self.output_digest,
            "ownership observation output changed"
        );
        if let Some(host) = &self.host {
            ensure!(
                host.observe(now)?.binding() == self.observation,
                "ownership state changed or an observed claim expired"
            );
        }
        Ok(())
    }
}
pub(crate) fn collect<'a>(context: Context<'a>) -> Result<Evidence<'a>> {
    collect_at(context, crate::instant::now())
}
fn collect_at<'a>(context: Context<'a>, now: f64) -> Result<Evidence<'a>> {
    let started = Instant::now();
    ensure!(now.is_finite() && now > 0.0, "ownership clock unavailable");
    ensure!(
        context.owner.len() <= 256 && !context.owner.chars().any(char::is_control),
        "ownership lane identity exceeds bounds or contains controls"
    );
    ensure!(
        context.candidate.changes.len() <= 4096,
        "ownership exceeds 4096 changes"
    );
    crate::candidate_review_evidence::validate_changes(context.source, context.candidate)?;
    let mut classified = context.candidate.clone();
    context.policy.classify_candidate(&mut classified)?;
    let inputs = Inputs::new(
        context.source,
        context.snapshot,
        &classified,
        context.policy,
        context.today,
    )?;
    let relevant: Vec<_> = classified
        .changes
        .iter()
        .filter(|change| {
            !change.deleted()
                && change.path != ".current_work.md"
                && (change.classes.is_empty()
                    || !change
                        .classes
                        .iter()
                        .all(|class| matches!(class.as_str(), "docs" | "config")))
        })
        .map(|change| change.path.clone())
        .collect();
    let host = if matches!(context.surface, Surface::PreCommit) {
        Some(HostState::capture(context.source, context.snapshot)?)
    } else {
        None
    };
    let (findings, metrics, observation) = match &host {
        Some(host) => {
            let observed = host.observe(now)?;
            let (findings, metrics) = findings(&observed, &relevant, context.owner)?;
            (findings, metrics, observed.binding())
        }
        None => (
            Vec::new(),
            json!({"status":"local-precommit-only"}),
            Value::Null,
        ),
    };
    let result = finish(findings, relevant, metrics, started)?;
    let evidence = json!({"schema_version":1,"surface":context.surface,"owner":context.owner,
        "candidate_tree_oid":classified.tree_oid,"base_tree_oid":classified.base_tree_oid,
        "policy_path":context.policy.path,"policy_sha256":context.policy.digest,
        "observed_at":crate::instant::isoformat_utc(now),"host_files":host.as_ref().map(|host|&host.files),
        "host_state":observation,"host_state_scope":"live operator Git common directory; not captured candidate",
        "execution_verified":false,"review_complete":false,"approval":false});
    let output_digest = candidate_test_selection::digest_json(&(&result, &evidence))?;
    inputs.verify()?;
    let held = Evidence {
        inputs,
        candidate: context.candidate,
        candidate_digest: candidate_test_selection::digest_json(context.candidate)?,
        host,
        observation,
        observed_at: now,
        output_digest,
        result,
        evidence,
    };
    held.verify_at(now)?;
    Ok(held)
}
fn findings(
    observed: &Observation,
    relevant: &[String],
    owner: &str,
) -> Result<(Vec<Finding>, Value)> {
    let (claims, digest, active) = match observed {
        Observation::Invalid(error) => {
            let findings = if relevant.is_empty() {
                Vec::new()
            } else {
                vec![finding(
                    json!({"rule_id":"malformed-claim-store","severity":"high",
                    "message":format!("ownership claim evidence is invalid: {error}")}),
                )?]
            };
            return Ok((findings, json!({})));
        }
        Observation::Valid {
            claims,
            digest,
            active,
        } => (claims, digest, active),
    };
    ensure!(claims.len() <= 4096, "ownership exceeds 4096 claims");
    let paths: usize = claims.iter().map(|claim| claim.paths.len()).sum();
    ensure!(
        paths <= 65536 && paths.saturating_mul(relevant.len()) <= 10_000_000,
        "ownership path matching exceeds admission budget"
    );
    let folded = conductor_native::candidate_unicode::casefold(owner)?;
    let mut findings = Vec::new();
    for claim in claims
        .iter()
        .filter(|claim| !active.contains(&claim.claim_id))
    {
        findings.push(finding(json!({"rule_id":"stale-claim","severity":"medium",
            "message":format!("ownership claim {} for {} expired",claim.claim_id,claim.owner),
            "evidence":{"claim_id":claim.claim_id,"owner":claim.owner,"expires_at":claim.expires_at}}))?);
    }
    for path in relevant {
        let matching: Vec<_> = claims
            .iter()
            .filter(|claim| {
                active.contains(&claim.claim_id)
                    && claim
                        .paths
                        .iter()
                        .any(|item| path == item || path.starts_with(&format!("{item}/")))
            })
            .collect();
        if matching.is_empty() {
            findings.push(finding(json!({"rule_id":"unclaimed-path","severity":"high","path":path,
                "message":"candidate path is not covered by an active exact ownership claim",
                "help":"Create a narrow claim with candidate-review claim before editing or commit from an isolated worktree."}))?);
        } else if !owner.is_empty()
            && !matching
                .iter()
                .map(|claim| conductor_native::candidate_unicode::casefold(&claim.owner))
                .collect::<Result<Vec<_>>>()?
                .contains(&folded)
        {
            findings.push(finding(json!({"rule_id":"claim-owner-mismatch","severity":"high","path":path,
                "message":format!("candidate owner {} does not own the active path claim",conductor_native::candidate_unicode::python_repr(owner)?),
                "evidence":{"claim_owners":matching.iter().map(|claim|&claim.owner).collect::<Vec<_>>()}}))?);
        }
    }
    Ok((
        findings,
        json!({"active_claims":active.len(),"claim_ids":active,"state_sha256":digest}),
    ))
}
fn finding(mut value: Value) -> Result<Finding> {
    value["check_id"] = json!("ownership");
    let mut finding: Finding = serde_json::from_value(value)?;
    candidate_assessment::finalize_finding(&mut finding);
    Ok(finding)
}
fn finish(
    findings: Vec<Finding>,
    files: Vec<String>,
    metrics: Value,
    started: Instant,
) -> Result<CheckResult> {
    ensure!(findings.len() <= 4096, "ownership exceeds 4096 findings");
    Ok(CheckResult {
        check_id: "ownership".into(),
        status: if findings.is_empty() {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        },
        findings,
        files,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        command: Vec::new(),
        tool_version: Some("forge-review-ownership/1".into()),
        cache_key: None,
        cache_hit: false,
        exit_code: None,
        skipped_reason: None,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        metrics: serde_json::from_value(metrics)?,
    })
}
