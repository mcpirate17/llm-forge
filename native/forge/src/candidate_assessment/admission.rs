//! Validate the supplied scope and resource bounds before cloning or evaluating.
use super::{CheckResult, CheckStatus, Severity};
use crate::{candidate_policy::Policy, candidate_source::Candidate};
use anyhow::{ensure, Context, Result};
use conductor_native::candidate_policy::PreparedGlob;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const MAX_RESULTS: usize = 20_000;
const MAX_FINDINGS: usize = 100_000;
const MAX_FILES: usize = 100_000;
const MAX_ARGS: usize = 100_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_FIELD: usize = 1024 * 1024;
const MAX_NODES: usize = 1_000_000;
const MAX_PAIRS: u64 = 2_000_000;

type ExceptionGroups<'a> = BTreeMap<&'a str, Vec<usize>>;
type ExaminedPaths<'a> = BTreeMap<&'a str, BTreeSet<&'a str>>;
type FindingCounts<'a> = BTreeMap<&'a str, usize>;

pub(super) struct Exception<'a> {
    pub id: &'a str,
    pub check: &'a str,
    pub rule: Option<&'a str>,
    pub path: &'a str,
    pub fingerprint: Option<&'a str>,
    pub owner: &'a str,
    pub expires: &'a str,
    pub glob: PreparedGlob,
    pub pattern_chars: usize,
}
pub(super) struct ContextData<'a> {
    pub threshold: Severity,
    pub checks: BTreeMap<&'a str, bool>,
    pub exceptions: Vec<Exception<'a>>,
    pub groups: ExceptionGroups<'a>,
    pub changed: BTreeSet<&'a str>,
    pub examined: ExaminedPaths<'a>,
}
#[derive(Default)]
struct Budget {
    bytes: usize,
    nodes: usize,
}
impl Budget {
    fn node(&mut self) -> Result<()> {
        self.nodes = self
            .nodes
            .checked_add(1)
            .context("assessment node count overflow")?;
        self.bytes = self
            .bytes
            .checked_add(16)
            .context("assessment byte count overflow")?;
        ensure!(
            self.nodes <= MAX_NODES && self.bytes <= MAX_BYTES,
            "assessment input exceeds structural limit"
        );
        Ok(())
    }
    fn text(&mut self, value: &str) -> Result<()> {
        self.node()?;
        ensure!(value.len() <= MAX_FIELD, "assessment string exceeds 1 MiB");
        self.bytes = self
            .bytes
            .checked_add(value.len())
            .context("assessment byte count overflow")?;
        ensure!(self.bytes <= MAX_BYTES, "assessment input exceeds 64 MiB");
        Ok(())
    }
    fn value(&mut self, value: &Value, depth: usize) -> Result<()> {
        self.node()?;
        ensure!(depth <= 32, "assessment value exceeds depth limit");
        match value {
            Value::String(text) => self.text(text)?,
            Value::Array(items) => {
                for item in items {
                    self.value(item, depth + 1)?;
                }
            }
            Value::Object(map) => {
                for (key, item) in map {
                    self.text(key)?;
                    self.value(item, depth + 1)?;
                }
            }
            Value::Number(number) => self.text(&number.to_string())?,
            _ => (),
        }
        Ok(())
    }
    fn object(&mut self, map: &serde_json::Map<String, Value>) -> Result<()> {
        self.node()?;
        for (key, item) in map {
            self.text(key)?;
            self.value(item, 1)?;
        }
        Ok(())
    }
    fn optional(&mut self, value: &Option<String>) -> Result<()> {
        if let Some(value) = value {
            self.text(value)?;
        }
        Ok(())
    }
}
fn severity(value: &str) -> Result<Severity> {
    match value {
        "info" => Ok(Severity::Info),
        "low" => Ok(Severity::Low),
        "medium" => Ok(Severity::Medium),
        "high" => Ok(Severity::High),
        "critical" => Ok(Severity::Critical),
        _ => anyhow::bail!("invalid assessment block threshold"),
    }
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("assessment policy missing {key}"))
}
fn optional<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match value.get(key) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        _ => anyhow::bail!("invalid assessment exception {key}"),
    }
}
fn bounded_id(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control),
        "invalid assessment ID"
    );
    Ok(())
}
fn reserved(id: &str) -> bool {
    matches!(id, "engine-integrity" | "attestation")
}
fn preflight_counts(results: &[CheckResult]) -> Result<()> {
    ensure!(
        results.len() <= MAX_RESULTS,
        "assessment result count exceeds limit"
    );
    let (mut files, mut args, mut findings) = (0usize, 0usize, 0usize);
    for result in results {
        files = files
            .checked_add(result.files.len())
            .context("assessment file count overflow")?;
        args = args
            .checked_add(result.command.len())
            .context("assessment argument count overflow")?;
        findings = findings
            .checked_add(result.findings.len())
            .context("assessment finding count overflow")?;
        ensure!(
            files <= MAX_FILES && args <= MAX_ARGS && findings <= MAX_FINDINGS,
            "assessment files, arguments, or findings exceed count limits"
        );
    }
    Ok(())
}

pub(super) fn prepare<'a>(
    policy: &'a Policy,
    candidate: &'a Candidate,
    results: &'a [CheckResult],
) -> Result<ContextData<'a>> {
    preflight_counts(results)?;
    ensure!(
        candidate.changes.len() <= 100_000,
        "assessment candidate count exceeds limit"
    );
    let mut budget = Budget::default();
    budget.value(&policy.document, 0)?;
    ensure!(
        budget.bytes <= 4 * 1024 * 1024,
        "assessment normalized policy exceeds 4 MiB"
    );
    let threshold = severity(required(&policy.document, "block_at")?)?;
    let checks = checks(&policy.document)?;
    let (exceptions, groups) = exceptions(&policy.document, &checks)?;
    let mut changed = BTreeSet::new();
    for change in &candidate.changes {
        budget.node()?;
        for path in std::iter::once(&change.path).chain(change.old_path.as_ref()) {
            crate::candidate_source::safe_path(path.as_bytes())?;
            budget.text(path)?;
            changed.insert(path.as_str());
        }
    }
    let (examined, finding_counts) = inspect_results(results, &checks, &mut budget)?;
    let mut pairs = 0u64;
    for (check, indexes) in &groups {
        let findings = finding_counts.get(check).copied().unwrap_or(0);
        let files = examined.get(check).map_or(0, BTreeSet::len);
        let count = findings
            .checked_add(files)
            .context("assessment item count overflow")?;
        let group = (indexes.len() as u64)
            .checked_mul(count as u64)
            .context("assessment pair count overflow")?;
        pairs = pairs
            .checked_add(group)
            .context("assessment pair count overflow")?;
        ensure!(
            pairs <= MAX_PAIRS,
            "assessment grouped exception matching exceeds pair limit"
        );
    }
    Ok(ContextData {
        threshold,
        checks,
        exceptions,
        groups,
        changed,
        examined,
    })
}
fn checks(document: &Value) -> Result<BTreeMap<&str, bool>> {
    let mut checks = BTreeMap::new();
    for check in document["checks"]
        .as_array()
        .context("assessment checks missing")?
    {
        let id = required(check, "check_id")?;
        bounded_id(id)?;
        ensure!(!reserved(id), "assessment policy uses reserved check ID");
        let diff = match required(check, "attribution")? {
            "diff" => true,
            "candidate" => false,
            _ => anyhow::bail!("invalid assessment attribution"),
        };
        ensure!(
            checks.insert(id, diff).is_none(),
            "duplicate assessment policy check ID"
        );
    }
    Ok(checks)
}
fn exceptions<'a>(
    document: &'a Value,
    checks: &BTreeMap<&str, bool>,
) -> Result<(Vec<Exception<'a>>, ExceptionGroups<'a>)> {
    let mut ids = BTreeSet::new();
    let mut result = Vec::new();
    let mut groups = ExceptionGroups::new();
    for value in document["exceptions"]
        .as_array()
        .context("assessment exceptions missing")?
    {
        let id = required(value, "exception_id")?;
        let check = required(value, "check_id")?;
        bounded_id(id)?;
        bounded_id(check)?;
        ensure!(
            ids.insert(id) && checks.contains_key(check),
            "invalid assessment exception identity"
        );
        let path = required(value, "path")?;
        ensure!(
            !path.is_empty() && path.len() <= 4096 && !path.contains('\0'),
            "invalid exception path pattern"
        );
        let rule = optional(value, "rule_id")?;
        let fingerprint = optional(value, "fingerprint")?;
        if let Some(rule) = rule {
            bounded_id(rule)?;
        }
        if let Some(fingerprint) = fingerprint {
            bounded_id(fingerprint)?;
        }
        groups.entry(check).or_default().push(result.len());
        result.push(Exception {
            id,
            check,
            rule,
            path,
            fingerprint,
            owner: required(value, "owner")?,
            expires: required(value, "expires")?,
            glob: PreparedGlob::new(path),
            pattern_chars: path.chars().count(),
        });
    }
    Ok((result, groups))
}
fn inspect_results<'a>(
    results: &'a [CheckResult],
    checks: &BTreeMap<&str, bool>,
    budget: &mut Budget,
) -> Result<(ExaminedPaths<'a>, FindingCounts<'a>)> {
    let mut examined = ExaminedPaths::new();
    let mut counts = FindingCounts::new();
    for result in results {
        budget.node()?;
        budget.text(&result.check_id)?;
        bounded_id(&result.check_id)?;
        ensure!(
            checks.contains_key(result.check_id.as_str()) || reserved(&result.check_id),
            "assessment result names unknown check"
        );
        ensure!(
            result.status != CheckStatus::Failed || !result.findings.is_empty(),
            "failed check has no findings"
        );
        ensure!(
            result.status != CheckStatus::Skipped || result.findings.is_empty(),
            "skipped check has findings"
        );
        ensure!(
            result.status != CheckStatus::Passed
                || result.findings.iter().all(|f| f.severity < Severity::High),
            "passed check has high or critical finding"
        );
        inspect_result(result, budget)?;
        let count = counts.entry(&result.check_id).or_default();
        *count = count
            .checked_add(result.findings.len())
            .context("finding count overflow")?;
        examined
            .entry(&result.check_id)
            .or_default()
            .extend(result.files.iter().map(String::as_str));
    }
    Ok((examined, counts))
}
fn inspect_result(result: &CheckResult, budget: &mut Budget) -> Result<()> {
    for path in &result.files {
        crate::candidate_source::safe_path(path.as_bytes())?;
        budget.text(path)?;
    }
    for text in &result.command {
        budget.text(text)?;
    }
    for value in [
        &result.tool_version,
        &result.cache_key,
        &result.skipped_reason,
    ] {
        budget.optional(value)?;
    }
    budget.text(&result.stdout_tail)?;
    budget.text(&result.stderr_tail)?;
    budget.object(&result.metrics)?;
    for finding in &result.findings {
        budget.node()?;
        ensure!(
            finding.check_id == result.check_id,
            "finding check ID mismatch"
        );
        bounded_id(&finding.check_id)?;
        bounded_id(&finding.rule_id)?;
        if !finding.fingerprint.is_empty() {
            bounded_id(&finding.fingerprint)?;
        }
        if let Some(path) = &finding.path {
            crate::candidate_source::safe_path(path.as_bytes())?;
        }
        if let Some(id) = &finding.exception_id {
            if !id.is_empty() {
                bounded_id(id)?;
            }
        }
        for value in [
            &finding.check_id,
            &finding.rule_id,
            &finding.message,
            &finding.fingerprint,
        ] {
            budget.text(value)?;
        }
        for value in [&finding.path, &finding.help, &finding.exception_id] {
            budget.optional(value)?;
        }
        budget.object(&finding.evidence)?;
    }
    Ok(())
}
