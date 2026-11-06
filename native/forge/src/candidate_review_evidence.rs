//! Held native review evidence observations, never execution proof or approval.
#[path = "candidate_review_evidence_checks.rs"]
mod checks;
#[cfg(test)]
#[path = "candidate_review_evidence_tests.rs"]
mod tests;
use crate::{
    candidate_assessment::{self, CheckResult, CheckStatus, Finding},
    candidate_policy::Policy,
    candidate_snapshot::Snapshot,
    candidate_source::{Candidate, Change, Source},
    candidate_test_selection::{self, inputs::Inputs, Selection},
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Instant};

/// Keep this observation, its selection and snapshot alive until coordinator
/// disposition. Public result edits are rejected by verify; clone for assessment.
pub(crate) struct Evidence<'a, 's> {
    inputs: Inputs<'a>,
    selection: &'s Selection<'a>,
    output_digest: String,
    pub results: Vec<CheckResult>,
    pub evidence: Value,
}
impl Evidence<'_, '_> {
    pub fn verify(&self) -> Result<()> {
        self.inputs.verify()?;
        self.selection.verify()?;
        ensure!(
            candidate_test_selection::digest_json(&(&self.results, &self.evidence))?
                == self.output_digest,
            "review evidence output changed"
        );
        Ok(())
    }
}

pub(crate) fn collect<'a, 's>(
    source: &Source,
    snapshot: &'a Snapshot,
    candidate: &'a Candidate,
    policy: &'a Policy,
    today: &'a str,
    selection: &'s Selection<'a>,
) -> Result<Evidence<'a, 's>> {
    ensure!(
        candidate.changes.len() <= 4096,
        "review evidence exceeds 4096 changes"
    );
    let tests = selection.selected_evidence_paths(source, snapshot, candidate, policy)?;
    validate_changes(source, candidate)?;
    let mut classified = candidate.clone();
    policy.classify_candidate(&mut classified)?;
    let mut inputs = Inputs::new(source, snapshot, &classified, policy, today)?;
    let mut ledger = BTreeMap::new();
    let performance = checks::performance(source, &classified, policy, &mut inputs, &mut ledger)?;
    let (research, ranges) =
        checks::research(source, &classified, &tests, &mut inputs, &mut ledger)?;
    let results = vec![performance, research];
    let evidence = json!({"schema_version":1,"base_tree_oid":candidate.base_tree_oid,
        "candidate_tree_oid":candidate.tree_oid,"policy_path":policy.path,"policy_sha256":policy.digest,
        "selected_test_paths":tests,"inputs":ledger.into_values().collect::<Vec<_>>(),
        "changed_line_ranges":ranges,"receipt_scope":"source-bound numerical self-consistency; no execution authentication",
        "test_execution_verified":false,"execution_verified":false,"review_complete":false,"approval":false});
    crate::json_canon::encoded_size(&evidence, 32 * 1024 * 1024)?;
    let output_digest = candidate_test_selection::digest_json(&(&results, &evidence))?;
    inputs.verify()?;
    selection.verify()?;
    Ok(Evidence {
        inputs,
        selection,
        output_digest,
        results,
        evidence,
    })
}
pub(crate) fn validate_changes(source: &Source, candidate: &Candidate) -> Result<()> {
    let expected = source.changes(&candidate.base_tree_oid, &candidate.tree_oid)?;
    ensure!(
        expected.len() == candidate.changes.len(),
        "candidate change inventory is incomplete"
    );
    for (actual, supplied) in expected.iter().zip(&candidate.changes) {
        ensure!(
            identity(actual) == identity(supplied),
            "candidate change differs from immutable Git trees"
        );
    }
    if let Some(base) = &candidate.base_commit_oid {
        ensure!(
            source.commit_tree(base)? == candidate.base_tree_oid,
            "candidate base commit/tree mismatch"
        );
    }
    if let Some(commit) = &candidate.commit_oid {
        ensure!(
            source.commit_tree(commit)? == candidate.tree_oid,
            "candidate commit/tree mismatch"
        );
    }
    Ok(())
}
fn identity(change: &Change) -> (&str, &str, &Option<String>, &str, &str, &str, &str) {
    (
        &change.status,
        &change.path,
        &change.old_path,
        &change.old_mode,
        &change.new_mode,
        &change.old_oid,
        &change.new_oid,
    )
}
fn read(
    inputs: &mut Inputs<'_>,
    path: &str,
    limit: usize,
    ledger: &mut BTreeMap<String, Value>,
) -> Result<Vec<u8>> {
    let bytes = inputs.read_bytes_with_limit(path, limit)?.to_vec();
    let oid = &inputs
        .entries()
        .get(path)
        .context("captured evidence entry missing")?
        .oid;
    ledger.entry(path.into()).or_insert_with(|| {
        json!({"path":path,"git_blob_oid":oid,
        "sha256":format!("{:x}",Sha256::digest(&bytes)),"bytes":bytes.len()})
    });
    Ok(bytes)
}
fn evaluate(
    id: &str,
    payload: Value,
    files: Vec<String>,
    metrics: Value,
    started: Instant,
) -> Result<CheckResult> {
    crate::json_canon::encoded_size(&payload, 32 * 1024 * 1024)?;
    let native =
        conductor_native::candidate_checks::evaluate(id, &payload).map_err(anyhow::Error::msg)?;
    let mut findings: Vec<Finding> = serde_json::from_value(native["findings"].clone())?;
    ensure!(
        findings.len() <= 4096,
        "review evidence exceeds 4096 findings"
    );
    for finding in &mut findings {
        ensure!(
            finding.check_id == id,
            "review evidence returned another check ID"
        );
        candidate_assessment::finalize_finding(finding);
    }
    Ok(CheckResult {
        check_id: id.into(),
        status: if findings.is_empty() {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        },
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        findings,
        files,
        command: Vec::new(),
        tool_version: Some("forge-review-evidence/1".into()),
        cache_key: None,
        cache_hit: false,
        exit_code: None,
        skipped_reason: None,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        metrics: metrics
            .as_object()
            .context("review evidence metrics must be object")?
            .clone(),
    })
}
