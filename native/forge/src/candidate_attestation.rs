//! Source-bound post-commit observation of an existing pre-commit receipt.
//! A matching recorded pass is a checksum/binding observation, not authenticated
//! execution or approval. Future review orchestration must collect its own checks.
use crate::{
    candidate_policy::Policy,
    candidate_receipt,
    candidate_source::{Candidate, Kind, Source},
};
use anyhow::{ensure, Context, Result};
use cap_std::fs::Dir;
use conductor_native::{candidate_cargo_lint::read_optional, candidate_checks::Finding};
use serde_json::{json, Value};

#[path = "candidate_attestation_ci.rs"]
mod ci;
pub(crate) use ci::collect as collect_ci;

const RECOVERY: &str="no matching recorded pre-commit pass was observed for this commit tree; post-commit recovery evidence is required";

fn recorded_pass(
    common: &Dir,
    relative: &str,
    candidate: &Candidate,
    policy: &Policy,
) -> Result<String> {
    let bytes = read_optional(common, relative, candidate_receipt::MAX_BYTES)
        .map_err(anyhow::Error::msg)?
        .context("missing pre-commit receipt")?;
    let receipt = candidate_receipt::parse_and_verify(&bytes)?;
    let payload = receipt.payload();
    ensure!(
        payload["schema_version"] == 1
            && payload["surface"] == "pre-commit"
            && payload["profile"] == "fast",
        "pre-commit receipt schema, surface or profile mismatch"
    );
    ensure!(
        payload["candidate"]["tree_oid"] == candidate.tree_oid,
        "pre-commit receipt is bound to a different tree"
    );
    ensure!(
        payload["policy"]["sha256"] == policy.digest,
        "pre-commit receipt is bound to a different policy"
    );
    ensure!(
        receipt.decision() == "pass",
        "pre-commit receipt did not record pass"
    );
    Ok(receipt.digest().to_owned())
}

/// Collect one post-commit observation from the resolved source's common Git
/// directory. No caller receipt JSON, alternate receipt path, subprocess or write
/// is accepted. Return the standard native `{findings,metrics}` payload so the
/// existing builtin result mapper supplies status, timing and fresh fingerprints.
pub fn collect_post_commit(
    source: &Source,
    candidate: &Candidate,
    policy: &Policy,
) -> Result<Value> {
    ensure!(
        matches!(candidate.kind, Kind::Commit | Kind::Range),
        "post-commit attestation requires a committed candidate"
    );
    let commit = candidate
        .commit_oid
        .as_deref()
        .context("post-commit attestation commit missing")?;
    ensure!(
        source.commit_tree(commit)? == candidate.tree_oid,
        "post-commit candidate differs from source commit tree"
    );
    conductor_native::sha256_hex::parse(&policy.digest).map_err(anyhow::Error::msg)?;
    let relative = format!(
        "governance/receipts/pre-commit/{}-fast.json",
        candidate.tree_oid
    );
    let common = Dir::open_ambient_dir(source.common_dir(), cap_std::ambient_authority())?;
    let observation = recorded_pass(&common, &relative, candidate, policy);
    let (findings, state, detail, digest) = match observation {
        Ok(digest) => (
            Vec::new(),
            "matching-recorded-pass",
            "ok".to_owned(),
            Some(digest),
        ),
        Err(error) => {
            let detail = format!("{error:#}").chars().take(1000).collect::<String>();
            let findings = vec![Finding::new(
                "attestation",
                "precommit-bypass-recovered",
                "medium",
                RECOVERY,
            )
            .evidence(json!({"detail":detail}))];
            (findings, "missing-or-invalid", detail, None)
        }
    };
    Ok(
        json!({"findings":findings,"metrics":{"surface":"post-commit","precommit_receipt":state,
        "detail":detail,"receipt_path":relative,"receipt_digest":digest,"candidate_tree_oid":candidate.tree_oid,
        "policy_sha256":policy.digest,"evidence_scope":"recorded-precommit-receipt-checksum-and-binding",
        "execution_verified":false,"review_complete":false,"approval":false}}),
    )
}

#[cfg(test)]
#[path = "candidate_attestation_tests.rs"]
mod tests;
