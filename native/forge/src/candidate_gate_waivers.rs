//! Report base matches while requiring the review's explicit waiver authority.
use super::types::Phase;
use crate::candidate_execution_plan::Selection;
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
fn assess(
    rows: &[Value],
    values: &[Value],
    policy_sha: &str,
    grant: Option<&str>,
    base: &str,
) -> Result<Phase> {
    if !rows.is_empty() || !values.is_empty() {
        ensure!(
            grant == Some(policy_sha),
            "candidate waiver policy requires an explicit matching operator grant"
        );
    }
    let active = rows
        .iter()
        .filter(|row| row["integration_base"].as_str() == Some(base))
        .count();
    let total = rows.len();
    Ok(Phase::new("waiver-activation",true,format!("{active}/{total} mutation waiver(s) match base {}; full activation requires review hash and date checks",&base[..12.min(base.len())]),
        json!({"total":total,"active":active,"inert":total-active,"base_commit":base,"scope":"base match only"})))
}
pub(super) fn run(selection: &Selection) -> Result<Phase> {
    let rows = selection.policy.document["mutation_waivers"]
        .as_array()
        .context("mutation waivers missing")?;
    let values = selection.policy.document["value_waivers"]
        .as_array()
        .context("value waivers missing")?;
    let base = selection
        .candidate
        .base_commit_oid
        .as_deref()
        .context("range merge base is missing")?;
    assess(
        rows,
        values,
        &selection.policy.digest,
        selection.grants.mutation_waivers_policy_sha256.as_deref(),
        base,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn waiver_base_matches_never_substitute_for_operator_authority() {
        let rows = vec![
            json!({"integration_base":"base"}),
            json!({"integration_base":"else"}),
        ];
        assert!(assess(&rows, &[], "policy", None, "base").is_err());
        assert!(assess(&rows, &[], "policy", Some("other"), "base").is_err());
        let phase = assess(&rows, &[], "policy", Some("policy"), "base").unwrap();
        assert!(phase.ok);
        assert_eq!(phase.evidence["active"], 1);
        assert_eq!(phase.evidence["inert"], 1);
        assert!(assess(&[], &[json!({})], "policy", None, "base").is_err());
        assert!(assess(&[], &[], "policy", None, "base").unwrap().ok);
    }
}
