use crate::candidate_assessment::{CheckResult, Finding, Severity};
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use serde_json::Value;

pub(super) const WAIVED: &str = "new-test-value-waived";
pub(super) struct Report<'a> {
    pub payload: &'a Value,
    pub id: &'a str,
    pub decision: &'a str,
    pub surface: &'a str,
    pub profile: &'a str,
    pub tree: &'a str,
    pub duration: u64,
    pub hits: u64,
    pub threshold: Severity,
    pub findings: Vec<Finding>,
    pub checks: Vec<CheckResult>,
}
impl<'a> Report<'a> {
    pub fn parse(receipt: &'a crate::candidate_receipt::VerifiedReceipt) -> Result<Self> {
        let p = receipt.payload();
        super::validate_structure(p, 0, &mut 1_000_000)?;
        ensure!(
            p["schema_version"].as_u64() == Some(1),
            "unsupported receipt schema"
        );
        let decision = text(p, "decision")?;
        ensure!(
            matches!(decision, "pass" | "fail"),
            "invalid recorded decision"
        );
        let id = text(p, "receipt_id")?;
        ensure!(
            id == receipt.receipt_id()
                && decision == receipt.decision()
                && text(p, "receipt_digest")? == receipt.digest(),
            "inconsistent verified receipt fields"
        );
        let policy = p["policy"]
            .as_object()
            .context("receipt policy must be an object")?;
        let threshold = if policy.is_empty() {
            ensure!(
                decision == "fail",
                "empty policy only permitted for failure receipts"
            );
            Severity::High
        } else {
            Severity::deserialize(policy.get("block_at").context("policy.block_at missing")?)?
        };
        let (surface, profile, tree) = labels(p, decision)?;
        let raw_findings = array(p, "findings", 100_000)?;
        let raw_checks = array(p, "checks", 20_000)?;
        let mut total = raw_findings.len();
        for check in raw_checks {
            if let Some(findings) = check.get("findings") {
                total = total
                    .checked_add(
                        findings
                            .as_array()
                            .context("check findings must be array")?
                            .len(),
                    )
                    .context("finding count overflow")?;
                ensure!(total <= 100_000, "receipt render finding count limit");
            }
        }
        let findings = Vec::<Finding>::deserialize(required(p, "findings")?)?;
        let checks = Vec::<CheckResult>::deserialize(required(p, "checks")?)?;
        for finding in findings
            .iter()
            .chain(checks.iter().flat_map(|c| &c.findings))
        {
            ensure!(
                finding.line.is_none_or(|n| n > 0),
                "finding line must be positive"
            );
            ensure!(
                finding.column.is_none_or(|n| (0..i64::MAX).contains(&n)),
                "finding column invalid"
            );
            ensure!(
                !finding.check_id.is_empty() && !finding.rule_id.is_empty(),
                "finding identifier empty"
            );
        }
        Ok(Self {
            payload: p,
            id,
            decision,
            surface,
            profile,
            tree,
            duration: integer(required(p, "timings")?, "duration_ms")?,
            hits: integer(required(p, "cache")?, "hits")?,
            threshold,
            findings,
            checks,
        })
    }
    pub fn suppressed(&self, f: &Finding) -> bool {
        f.rule_id == WAIVED
            || f.exception_id.as_ref().is_some_and(|id| !id.is_empty())
            || f.inherited
    }
    pub fn blocking(&self, f: &Finding) -> bool {
        !self.suppressed(f) && f.severity >= self.threshold
    }
}
fn labels<'a>(p: &'a Value, decision: &str) -> Result<(&'a str, &'a str, &'a str)> {
    let surface = text(p, "surface")?;
    ensure!(
        matches!(surface, "manual" | "pre-commit" | "post-commit" | "ci"),
        "invalid receipt surface"
    );
    let profile = text(p, "profile")?;
    ensure!(
        matches!(profile, "fast" | "full" | "nightly"),
        "invalid receipt profile"
    );
    let tree = text(required(p, "candidate")?, "tree_oid")?;
    ensure!(
        (matches!(tree.len(), 40 | 64)
            && tree
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
            || (decision == "fail" && tree == "unresolved"),
        "invalid receipt tree identity"
    );
    Ok((surface, profile, tree))
}
fn array<'a>(p: &'a Value, key: &str, max: usize) -> Result<&'a Vec<Value>> {
    let values = required(p, key)?
        .as_array()
        .context("receipt model field must be array")?;
    ensure!(values.len() <= max, "receipt render count limit");
    Ok(values)
}
fn required<'a>(v: &'a Value, key: &str) -> Result<&'a Value> {
    v.get(key)
        .with_context(|| format!("receipt field {key} missing"))
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    required(v, key)?
        .as_str()
        .with_context(|| format!("receipt field {key} must be text"))
}
fn integer(v: &Value, key: &str) -> Result<u64> {
    required(v, key)?
        .as_u64()
        .with_context(|| format!("receipt field {key} must be unsigned integer"))
}
