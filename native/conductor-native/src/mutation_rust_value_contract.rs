//! Typed admission of an explicit Rust test-value declaration.
//!
//! This parser reads no ambient state and grants no execution authority. The
//! coordinator must independently bind files and hashes, check the Cargo target
//! and harness, and discover the actual tests before using the declaration.
use serde::Deserialize;
use std::collections::BTreeSet;

const MAX_DOCUMENT: usize = 64 * 1024;
const MAX_ROWS: usize = 4096;
const MAX_STRING: usize = 4096;

/// Validated declaration. It cannot be deserialized or modified by callers.
#[derive(Debug)]
pub struct Declaration {
    raw: Raw,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    schema_version: u32,
    target: Target,
    baseline_repetitions: u8,
    required_contracts: Vec<Contract>,
    ranked_tests: Vec<RankedTest>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    manifest_path: String,
    package: String,
    test: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    id: String,
    criticality: String,
    active_paths: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RankedTest {
    rank: usize,
    nodeid: String,
    contract: String,
    rationale: String,
    #[serde(default)]
    intentional_redundancy: bool,
}
impl Declaration {
    pub fn target(&self) -> &Target {
        &self.raw.target
    }
    pub fn baseline_repetitions(&self) -> u8 {
        self.raw.baseline_repetitions
    }
    pub fn required_contracts(&self) -> &[Contract] {
        &self.raw.required_contracts
    }
    pub fn ranked_tests(&self) -> &[RankedTest] {
        &self.raw.ranked_tests
    }
}
impl Target {
    pub fn manifest_path(&self) -> &str {
        &self.manifest_path
    }
    pub fn package(&self) -> &str {
        &self.package
    }
    pub fn test(&self) -> &str {
        &self.test
    }
}
impl Contract {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn criticality(&self) -> &str {
        &self.criticality
    }
    pub fn active_paths(&self) -> &[String] {
        &self.active_paths
    }
}
impl RankedTest {
    pub fn rank(&self) -> usize {
        self.rank
    }
    pub fn nodeid(&self) -> &str {
        &self.nodeid
    }
    pub fn contract(&self) -> &str {
        &self.contract
    }
    pub fn rationale(&self) -> &str {
        &self.rationale
    }
    pub fn intentional_redundancy(&self) -> bool {
        self.intentional_redundancy
    }
}
fn text(value: &str, label: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > MAX_STRING || value.chars().any(char::is_control) {
        return Err(format!(
            "{label} must be bounded, nonempty text without control characters"
        ));
    }
    Ok(())
}
fn path(value: &str) -> Result<(), String> {
    text(value, "path")?;
    let safe = crate::mutation_manifest::safe_relative(value, "path")?;
    if safe != value || value.contains('\\') {
        return Err("path must be canonical and relative".into());
    }
    Ok(())
}
fn cargo_name(value: &str) -> Result<(), String> {
    text(value, "Cargo target")?;
    if !value
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err("unsupported Cargo package or test target name".into());
    }
    Ok(())
}
fn sources(paths: &[String]) -> Result<BTreeSet<&str>, String> {
    if paths.is_empty() || paths.len() > MAX_ROWS {
        return Err("source path inventory must contain 1..4096 entries".into());
    }
    let mut out = BTreeSet::new();
    for value in paths {
        path(value)?;
        if !value.ends_with(".rs") || !out.insert(value.as_str()) {
            return Err("source paths must be unique Rust paths".into());
        }
    }
    Ok(out)
}
fn validate(raw: &Raw, selected: &[String]) -> Result<(), String> {
    if raw.schema_version != 1 || !(2..=5).contains(&raw.baseline_repetitions) {
        return Err("unsupported schema or baseline repetitions".into());
    }
    path(&raw.target.manifest_path)?;
    if raw.target.manifest_path.rsplit('/').next() != Some("Cargo.toml") {
        return Err("target manifest must name Cargo.toml".into());
    }
    cargo_name(&raw.target.package)?;
    cargo_name(&raw.target.test)?;
    if raw.required_contracts.len() != 1 {
        return Err("exactly one explicit contract is required".into());
    }
    let contract = &raw.required_contracts[0];
    text(&contract.id, "contract id")?;
    if !matches!(contract.criticality.as_str(), "critical" | "high") {
        return Err("contract criticality must be critical or high".into());
    }
    if sources(&contract.active_paths)? != sources(selected)? {
        return Err("selected sources differ from declared active paths".into());
    }
    if raw.ranked_tests.is_empty() || raw.ranked_tests.len() > MAX_ROWS {
        return Err("ranked tests must contain 1..4096 entries".into());
    }
    let mut nodes = BTreeSet::new();
    let mut identities = BTreeSet::new();
    for (index, test) in raw.ranked_tests.iter().enumerate() {
        if test.rank != index + 1 {
            return Err("ranked tests require contiguous ordered ranks".into());
        }
        text(&test.nodeid, "test nodeid")?;
        text(&test.rationale, "test rationale")?;
        if test.intentional_redundancy {
            return Err(
                "intentional redundancy is not admitted for this Rust declaration slice".into(),
            );
        }
        if test.contract != contract.id {
            return Err("ranked test must map to the explicit contract".into());
        }
        let (file, _) = test
            .nodeid
            .split_once("::")
            .ok_or("Rust nodeid missing function")?;
        path(file)?;
        let identity = crate::mutation_value_inputs::cargo_identity(&test.nodeid)?;
        if !nodes.insert(&test.nodeid) || !identities.insert(identity) {
            return Err("duplicate or ambiguous Rust test identity".into());
        }
    }
    Ok(())
}
/// Decode an untrusted author/operator declaration against the independently
/// selected source paths. No file existence, target, harness or test discovery
/// is asserted by successful parsing.
pub fn parse(bytes: &[u8], selected_sources: &[String]) -> Result<Declaration, String> {
    if bytes.len() > MAX_DOCUMENT {
        return Err("declaration exceeds 64 KiB".into());
    }
    let raw: Raw =
        serde_json::from_slice(bytes).map_err(|e| format!("invalid declaration JSON: {e}"))?;
    validate(&raw, selected_sources)?;
    Ok(Declaration { raw })
}
#[cfg(test)]
#[path = "mutation_rust_value_contract_tests.rs"]
mod tests;
