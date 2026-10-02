//! Shared numerical benchmark evidence. No filename or prose can prove a pass.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const SCHEMA: &str = "forge.performance.v1";
pub const EVIDENCE_SCHEMA: &str = "forge.performance-evidence.v1";
pub const MAX_GATE_REGRESSION_PERCENT: f64 = 10.0;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub host: String,
    pub head: String,
    pub source_files: BTreeMap<String, String>,
    pub command: String,
    pub workload_sha256: String,
    pub environment_sha256: String,
    pub platform: String,
    pub toolchain: String,
    pub warmups: usize,
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    pub wall_ms: f64,
    pub cpu_ms: f64,
    pub max_rss_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub cpu_p50_ms: f64,
    pub max_rss_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: String,
    pub created_unix: u64,
    pub identity: Identity,
    pub samples: Vec<Sample>,
    pub summary: Summary,
    pub receipt_sha256: String,
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn summarize(samples: &[Sample]) -> Result<Summary, String> {
    if !(5..=1000).contains(&samples.len()) {
        return Err("performance evidence needs 5..1000 successful samples".into());
    }
    if samples.iter().any(|sample| {
        !sample.wall_ms.is_finite()
            || sample.wall_ms <= 0.0
            || !sample.cpu_ms.is_finite()
            || sample.cpu_ms < 0.0
            || sample.max_rss_bytes == 0
    }) {
        return Err("invalid wall/CPU/RSS sample".into());
    }
    let percentile = |mut values: Vec<f64>, p: f64| {
        values.sort_by(f64::total_cmp);
        values[((values.len() as f64 * p).ceil() as usize).saturating_sub(1)]
    };
    Ok(Summary {
        p50_ms: percentile(samples.iter().map(|s| s.wall_ms).collect(), 0.5),
        p95_ms: percentile(samples.iter().map(|s| s.wall_ms).collect(), 0.95),
        cpu_p50_ms: percentile(samples.iter().map(|s| s.cpu_ms).collect(), 0.5),
        max_rss_bytes: samples.iter().map(|s| s.max_rss_bytes).max().unwrap(),
    })
}

fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Receipt {
    pub fn seal(&mut self) -> Result<(), String> {
        self.receipt_sha256.clear();
        self.receipt_sha256 = digest(&serde_json::to_vec(self).map_err(|e| e.to_string())?);
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA
            || self.created_unix == 0
            || self.identity.host.is_empty()
            || self.identity.head.is_empty()
            || self.identity.platform.is_empty()
            || self.identity.toolchain.is_empty()
            || self.identity.command.trim().is_empty()
            || self.identity.source_files.is_empty()
            || !hash(&self.identity.workload_sha256)
            || !hash(&self.identity.environment_sha256)
            || !(1..=86400).contains(&self.identity.timeout_seconds)
            || self.identity.warmups > 1000
        {
            return Err("incomplete benchmark identity".into());
        }
        for (path, value) in &self.identity.source_files {
            if path.is_empty()
                || path.starts_with('/')
                || path.contains('\\')
                || path
                    .split('/')
                    .any(|piece| piece == ".." || piece.is_empty())
                || !hash(value)
            {
                return Err(format!("invalid benchmark source identity: {path}"));
            }
        }
        if summarize(&self.samples)? != self.summary {
            return Err("benchmark summary differs from measured samples".into());
        }
        let mut copy = self.clone();
        copy.seal()?;
        if !hash(&self.receipt_sha256) || copy.receipt_sha256 != self.receipt_sha256 {
            return Err("benchmark receipt hash differs".into());
        }
        Ok(())
    }

    pub fn matches_sources(&self, sources: &BTreeMap<String, String>) -> Result<(), String> {
        self.validate()?;
        for (path, value) in sources {
            if self.identity.source_files.get(path) != Some(value) {
                return Err(format!(
                    "benchmark does not cover current source bytes: {path}"
                ));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Comparison {
    pub schema: String,
    pub current_sha256: String,
    pub baseline_sha256: String,
    pub max_regression_percent: f64,
    pub wall_p50_percent: f64,
    pub wall_p95_percent: f64,
    pub cpu_p50_percent: f64,
    pub rss_percent: f64,
    pub passed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub schema: String,
    pub current: Receipt,
    pub baseline: Receipt,
    pub max_regression_percent: f64,
}

impl Evidence {
    pub fn validate(
        &self,
        sources: &BTreeMap<String, String>,
        host: Option<&str>,
        policy_budget: f64,
    ) -> Result<(), String> {
        if self.schema != EVIDENCE_SCHEMA
            || !policy_budget.is_finite()
            || !(0.0..=MAX_GATE_REGRESSION_PERCENT).contains(&policy_budget)
            || !self.max_regression_percent.is_finite()
            || !(0.0..=policy_budget).contains(&self.max_regression_percent)
        {
            return Err("performance evidence budget exceeds the policy ceiling".into());
        }
        self.current.matches_sources(sources)?;
        if host.is_some_and(|host| host != self.current.identity.host) {
            return Err("performance evidence belongs to a different host checkout".into());
        }
        if !compare(&self.current, &self.baseline, self.max_regression_percent)?.passed {
            return Err("paired performance regression exceeds the declared budget".into());
        }
        Ok(())
    }
}

pub fn compare(current: &Receipt, baseline: &Receipt, budget: f64) -> Result<Comparison, String> {
    current.validate()?;
    baseline.validate()?;
    if !budget.is_finite() || !(0.0..=1000.0).contains(&budget) {
        return Err("regression budget must be finite and 0..1000 percent".into());
    }
    let a = &current.identity;
    let b = &baseline.identity;
    if a.host != b.host
        || a.command != b.command
        || a.workload_sha256 != b.workload_sha256
        || a.environment_sha256 != b.environment_sha256
        || a.platform != b.platform
        || a.toolchain != b.toolchain
        || a.warmups != b.warmups
        || a.timeout_seconds != b.timeout_seconds
        || a.source_files.keys().ne(b.source_files.keys())
        || current.samples.len() != baseline.samples.len()
    {
        return Err(
            "baseline workload, environment, toolchain or source scope is unmatched".into(),
        );
    }
    let delta = |now: f64, old: f64| {
        if old == 0.0 {
            if now == 0.0 {
                0.0
            } else {
                1001.0
            }
        } else {
            (now / old - 1.0) * 100.0
        }
    };
    let wall_p50_percent = delta(current.summary.p50_ms, baseline.summary.p50_ms);
    let wall_p95_percent = delta(current.summary.p95_ms, baseline.summary.p95_ms);
    let cpu_p50_percent = delta(current.summary.cpu_p50_ms, baseline.summary.cpu_p50_ms);
    let rss_percent = delta(
        current.summary.max_rss_bytes as f64,
        baseline.summary.max_rss_bytes as f64,
    );
    Ok(Comparison {
        schema: "forge.performance-comparison.v1".into(),
        current_sha256: current.receipt_sha256.clone(),
        baseline_sha256: baseline.receipt_sha256.clone(),
        max_regression_percent: budget,
        wall_p50_percent,
        wall_p95_percent,
        cpu_p50_percent,
        rss_percent,
        passed: [
            wall_p50_percent,
            wall_p95_percent,
            cpu_p50_percent,
            rss_percent,
        ]
        .iter()
        .all(|v| *v <= budget),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    pub fn fixture() -> Receipt {
        let samples = vec![
            Sample {
                wall_ms: 2.0,
                cpu_ms: 1.0,
                max_rss_bytes: 1024
            };
            5
        ];
        let mut receipt = Receipt {
            schema: SCHEMA.into(),
            created_unix: 1,
            identity: Identity {
                host: "/fixture".into(),
                head: "head".into(),
                source_files: BTreeMap::from([("src/a.rs".into(), digest(b"a"))]),
                command: "true".into(),
                workload_sha256: digest(b"true"),
                environment_sha256: digest(b"env"),
                platform: "cpu".into(),
                toolchain: "rustc".into(),
                warmups: 1,
                timeout_seconds: 2,
            },
            summary: summarize(&samples).unwrap(),
            samples,
            receipt_sha256: String::new(),
        };
        receipt.seal().unwrap();
        receipt
    }
    #[test]
    fn rejects_fabricated_summary_hash_drift_and_unmatched_workload() {
        let baseline = fixture();
        baseline.validate().unwrap();
        let mut receipt = baseline.clone();
        receipt.summary.p95_ms = 0.5;
        assert!(receipt.validate().is_err());
        receipt = baseline.clone();
        receipt
            .identity
            .source_files
            .insert("src/a.rs".into(), digest(b"changed"));
        assert!(receipt.validate().is_err());
        receipt.seal().unwrap();
        assert!(compare(&receipt, &baseline, 5.0).unwrap().passed);
        receipt.identity.environment_sha256 = digest(b"different");
        receipt.seal().unwrap();
        assert!(compare(&receipt, &baseline, 5.0).is_err());
    }
    #[test]
    fn source_coverage_and_numerical_regression_fail_closed() {
        let baseline = fixture();
        let mut current = baseline.clone();
        assert!(current
            .matches_sources(&BTreeMap::from([("src/a.rs".into(), digest(b"different"))]))
            .is_err());
        current.samples[0].wall_ms = 20.0;
        current.summary = summarize(&current.samples).unwrap();
        current.seal().unwrap();
        assert!(!compare(&current, &baseline, 10.0).unwrap().passed);
        current.samples.truncate(4);
        assert!(current.validate().is_err());
    }

    #[test]
    fn gate_requires_a_matching_passing_pair_with_a_policy_bounded_budget() {
        let baseline = fixture();
        let mut evidence = Evidence {
            schema: EVIDENCE_SCHEMA.into(),
            current: baseline.clone(),
            baseline,
            max_regression_percent: 10.0,
        };
        let sources = evidence.current.identity.source_files.clone();
        evidence.validate(&sources, Some("/fixture"), 10.0).unwrap();
        evidence.max_regression_percent = 11.0;
        assert!(evidence.validate(&sources, Some("/fixture"), 10.0).is_err());
        evidence.max_regression_percent = 10.0;
        evidence.current.samples[0].wall_ms = 20.0;
        evidence.current.summary = summarize(&evidence.current.samples).unwrap();
        evidence.current.seal().unwrap();
        assert!(evidence.validate(&sources, Some("/fixture"), 10.0).is_err());
        evidence.current = evidence.baseline.clone();
        evidence.baseline.identity.environment_sha256 = digest(b"unmatched");
        evidence.baseline.seal().unwrap();
        assert!(evidence.validate(&sources, Some("/fixture"), 10.0).is_err());
    }
}
