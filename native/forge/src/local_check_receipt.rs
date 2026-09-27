//! Source-bound, inspectable receipts for `forge check` in the Git common dir.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Passed,
    Failed,
    TimedOut,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepRecord {
    pub stage: String,
    pub name: String,
    pub command: String,
    pub timeout_s: u64,
    pub blocking: bool,
    pub verdict: Verdict,
    pub log_file: Option<String>,
    pub log_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub schema: u32,
    pub head: String,
    pub tree: String,
    pub base_ref: String,
    pub base_sha: String,
    pub merge_base: String,
    pub policy_path: String,
    pub policy_source: String,
    pub policy_sha256: String,
    pub changed_paths: Vec<String>,
    pub changed_file_sha256: String,
    pub changed_python_sha256: String,
    pub all: bool,
    pub passed: bool,
    pub steps: Vec<StepRecord>,
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes).with_context(|| format!("writing {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("renaming {}", path.display()))
}

pub fn save(path: &Path, receipt: &Receipt) -> Result<String> {
    let mut bytes = serde_json::to_vec_pretty(receipt)?;
    bytes.push(b'\n');
    let digest = sha256(&bytes);
    write_atomic(path, &bytes)?;
    Ok(digest)
}

pub fn load(path: &Path) -> Result<(Receipt, String)> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let receipt = serde_json::from_slice(&bytes).context("parsing local check receipt")?;
    Ok((receipt, sha256(&bytes)))
}

pub fn validate_logs(path: &Path, receipt: &Receipt) -> Result<()> {
    let parent = path.parent().context("receipt has no parent")?;
    for step in &receipt.steps {
        match (&step.log_file, &step.log_sha256, &step.verdict) {
            (None, None, Verdict::Skipped) => continue,
            (Some(file), Some(expected), verdict) if *verdict != Verdict::Skipped => {
                if file.is_empty() || file.contains('/') || file.contains('\\') || file == ".." {
                    bail!("unsafe log filename for step {}", step.name);
                }
                let actual = sha256(&fs::read(parent.join(file))?);
                if &actual != expected {
                    bail!("log hash changed for step {}", step.name);
                }
            }
            _ => bail!("inconsistent log fields for step {}", step.name),
        }
    }
    Ok(())
}

pub fn receipt_dir(common_dir: &Path) -> PathBuf {
    common_dir.join("forge-checks")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_log_invalidates_receipt() {
        let dir = std::env::temp_dir().join(format!("forge-receipt-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("run.log"), b"original").unwrap();
        let receipt = Receipt {
            schema: SCHEMA,
            head: "a".into(),
            tree: "b".into(),
            base_ref: "main".into(),
            base_sha: "c".into(),
            merge_base: "c".into(),
            policy_path: ".forge/local-check.toml".into(),
            policy_source: "HEAD".into(),
            policy_sha256: "d".into(),
            changed_paths: vec![],
            changed_file_sha256: "e".into(),
            changed_python_sha256: "f".into(),
            all: true,
            passed: true,
            steps: vec![StepRecord {
                stage: "check".into(),
                name: "one".into(),
                command: "true".into(),
                timeout_s: 1,
                blocking: true,
                verdict: Verdict::Passed,
                log_file: Some("run.log".into()),
                log_sha256: Some(sha256(b"original")),
            }],
        };
        let path = dir.join("receipt.json");
        save(&path, &receipt).unwrap();
        validate_logs(&path, &receipt).unwrap();
        fs::write(dir.join("run.log"), b"changed").unwrap();
        assert!(validate_logs(&path, &receipt).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
