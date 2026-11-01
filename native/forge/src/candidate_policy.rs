//! Bounded candidate-only policy loading and native change classification.
mod load;
mod normalize;
#[cfg(test)]
mod tests;

use crate::candidate_source::Candidate;
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};

const MAX_BYTES: usize = 1024 * 1024;
const MAX_STRING: usize = 4096;
const MAX_NODES: usize = 20_000;
const MAX_DEPTH: usize = 32;
const MAX_CLASSIFICATION_WORK: u64 = 100_000_000;

pub struct Policy {
    pub path: String,
    pub digest: String,
    pub document: Value,
}

pub fn load(snapshot: &std::path::Path, explicit: Option<&str>, today: &str) -> Result<Policy> {
    let requested = if explicit.is_some_and(|s| !s.is_empty()) {
        None
    } else {
        std::env::var_os("CONDUCTOR_POLICY")
            .map(|s| {
                s.into_string()
                    .map_err(|_| anyhow::anyhow!("CONDUCTOR_POLICY must be UTF-8"))
            })
            .transpose()?
    };
    load::load(snapshot, explicit, requested.as_deref(), today)
}

pub(crate) fn validate_today(today: &str) -> Result<()> {
    let bytes = today.as_bytes();
    ensure!(
        bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit()),
        "policy today must be a valid YYYY-MM-DD date"
    );
    let digits = |bytes: &[u8]| {
        bytes
            .iter()
            .fold(0u32, |value, byte| value * 10 + u32::from(byte - b'0'))
    };
    let year = i64::from(digits(&bytes[..4]));
    let month = digits(&bytes[5..7]);
    let day = digits(&bytes[8..]);
    ensure!(
        (1..=9999).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day),
        "policy today must be a valid YYYY-MM-DD date"
    );
    ensure!(
        crate::civil::civil_from_days(crate::civil::days_from_civil(year, month, day))
            == (year, month, day),
        "policy today must be a valid YYYY-MM-DD date"
    );
    Ok(())
}

impl Policy {
    /// Apply a single prepared native classifier, leaving the candidate unchanged
    /// if admission or classification fails anywhere in the batch.
    pub fn classify_candidate(&self, candidate: &mut Candidate) -> Result<()> {
        normalize::validate_json(&self.document)?;
        ensure!(
            candidate.changes.len() <= 100_000,
            "candidate classification exceeds 100000 changes"
        );
        let patterns = pattern_work(&self.document)?;
        let mut work = 0u64;
        let mut path_bytes = 0usize;
        for change in &candidate.changes {
            ensure!(
                [&change.old_mode, &change.new_mode]
                    .iter()
                    .all(|mode| mode.len() == 6
                        && mode.bytes().all(|byte| (b'0'..=b'7').contains(&byte))),
                "candidate modes must be six octal digits"
            );
            for path in std::iter::once(&change.path).chain(change.old_path.as_ref()) {
                crate::candidate_source::safe_path(path.as_bytes())?;
                path_bytes = path_bytes
                    .checked_add(path.len())
                    .context("classification path size overflow")?;
                ensure!(
                    path_bytes <= 64 * 1024 * 1024,
                    "classification paths exceed 64 MiB"
                );
                work = work
                    .checked_add(
                        patterns
                            .checked_mul(path.chars().count() as u64 + 1)
                            .context("classification work overflow")?,
                    )
                    .context("classification work overflow")?;
                ensure!(
                    work <= MAX_CLASSIFICATION_WORK,
                    "candidate classification exceeds work limit"
                );
            }
        }
        let mut classifier =
            conductor_native::candidate_policy::PreparedClassifier::new(&self.document)
                .map_err(anyhow::Error::msg)?;
        let results = candidate
            .changes
            .iter()
            .map(|change| {
                let result = classifier
                    .classify(&json!({"path":change.path,"old_path":change.old_path,
                "new_mode":change.new_mode,"old_mode":change.old_mode}))
                    .map_err(anyhow::Error::msg)?;
                let classes = result["classes"]
                    .as_array()
                    .context("classifier classes missing")?
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .context("classifier class must be a string")
                    })
                    .collect::<Result<Vec<_>>>()?;
                let risk = result["risk"]
                    .as_str()
                    .context("classifier risk missing")?
                    .to_owned();
                Ok((classes, risk))
            })
            .collect::<Result<Vec<_>>>()?;
        for (change, (classes, risk)) in candidate.changes.iter_mut().zip(results) {
            change.classes = classes;
            change.risk = risk;
        }
        Ok(())
    }

    pub fn active_checks(&self, profile: &str) -> Result<Vec<Value>> {
        ensure!(
            ["fast", "full", "nightly"].contains(&profile),
            "unknown candidate policy profile"
        );
        normalize::validate_json(&self.document)?;
        let checks = self.document["checks"]
            .as_array()
            .context("normalized policy checks missing")?;
        let mut active = Vec::new();
        for check in checks {
            let profiles = check["profiles"]
                .as_array()
                .context("normalized check profiles missing")?;
            if profiles.iter().any(|p| p.as_str() == Some(profile)) {
                active.push(check.clone());
            }
        }
        Ok(active)
    }
}

fn pattern_work(document: &Value) -> Result<u64> {
    let classes = document["class_globs"]
        .as_object()
        .context("policy class globs missing")?;
    let mut work = 0u64;
    for patterns in classes
        .values()
        .chain([&document["generated_globs"], &document["high_risk_globs"]])
    {
        for pattern in patterns
            .as_array()
            .context("policy patterns must be arrays")?
        {
            let text = pattern
                .as_str()
                .context("policy patterns must be strings")?;
            work = work
                .checked_add(text.chars().count() as u64 + 1)
                .context("classification pattern size overflow")?;
        }
    }
    Ok(work)
}
