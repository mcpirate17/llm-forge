//! Fresh coordinator-owned results. No caller-supplied rows enter this path.
use crate::candidate_assessment::{self, CheckResult, CheckStatus, Finding, Severity};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(super) fn result(id: &str, status: CheckStatus) -> CheckResult {
    CheckResult {
        check_id: id.into(),
        status,
        duration_ms: 0,
        findings: Vec::new(),
        files: Vec::new(),
        command: Vec::new(),
        tool_version: None,
        cache_key: None,
        cache_hit: false,
        exit_code: None,
        skipped_reason: None,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        metrics: Default::default(),
    }
}
pub(super) fn skipped(id: &str, why: &str) -> CheckResult {
    let mut value = result(id, CheckStatus::Skipped);
    value.skipped_reason = Some(why.into());
    value
}
pub(super) fn error(id: &str, error: impl std::fmt::Display) -> CheckResult {
    let text = error.to_string();
    let message = if text.chars().count() > 4000 {
        format!(
            "{} [truncated]",
            text.chars().take(4000).collect::<String>()
        )
    } else {
        text
    };
    let mut row = result(id, CheckStatus::Error);
    let mut finding = Finding {
        check_id: id.into(),
        rule_id: "native-review-incomplete".into(),
        severity: Severity::Critical,
        message,
        path: None,
        line: None,
        column: None,
        help: None,
        evidence: Default::default(),
        fingerprint: String::new(),
        exception_id: None,
        inherited: false,
    };
    candidate_assessment::finalize_finding(&mut finding);
    row.findings.push(finding);
    row
}

#[derive(Default)]
pub(super) struct Results {
    rows: BTreeMap<String, CheckResult>,
    pub incomplete: Vec<Value>,
}
impl Results {
    pub fn insert(&mut self, value: CheckResult, complete: bool) -> Result<()> {
        ensure!(
            !self.rows.contains_key(&value.check_id),
            "duplicate review result {}",
            value.check_id
        );
        ensure!(
            !value.cache_hit && value.status != CheckStatus::Cached,
            "cached review result cannot establish fresh execution"
        );
        if !complete || value.status == CheckStatus::Error {
            self.incomplete.push(json!({"check_id":value.check_id,"reason":"required observation or execution is incomplete"}));
        }
        self.rows.insert(value.check_id.clone(), value);
        Ok(())
    }
    pub fn failed(&mut self, id: &str, why: impl std::fmt::Display) -> Result<()> {
        self.insert(error(id, why), false)
    }
    pub fn obligation(&mut self, id: &str, why: impl std::fmt::Display) {
        self.incomplete
            .push(json!({"obligation":id,"reason":why.to_string()}));
    }
    pub fn regular(&mut self, id: &str, row: &Value, command: bool) -> Result<()> {
        ensure!(
            row["check_id"] == id,
            "regular result identity differs from plan"
        );
        if let Some(message) = row["error"].as_str() {
            return self.failed(id, message);
        }
        if row["skipped"] == true {
            return self.insert(skipped(id, "no matching candidate files"), true);
        }
        let value: CheckResult = serde_json::from_value(row["result"].clone())
            .context("invalid fresh regular result")?;
        ensure!(
            value.check_id == id && !value.cache_hit && value.status != CheckStatus::Cached,
            "regular result is not a fresh observation for this check"
        );
        let stages = row["stages"].as_array().context("regular stages missing")?;
        let complete = value.status != CheckStatus::Error
            && (!command
                || (stages.len() == 2
                    && stages[0]["report"]["exit_code"] == 0
                    && stages.iter().all(|stage| {
                        stage["report"]["cleanup_complete"] == true
                            && stage["report"]["outcome"] == "exited"
                            && stage["report"]["resources"]["verified"] == true
                            && stage["report"]["errors"]
                                .as_array()
                                .is_some_and(Vec::is_empty)
                            && stage["report"]["signal"].is_null()
                            && stage["report"]["exit_code"].as_i64().is_some()
                    })));
        self.insert(value, complete)
    }
    pub fn ordered(mut self, ids: &[String]) -> Result<(Vec<CheckResult>, Vec<Value>)> {
        let mut output = Vec::with_capacity(ids.len());
        for id in ids {
            if !self.rows.contains_key(id) {
                self.failed(id, "required check produced no result")?;
            }
            output.push(self.rows.remove(id).expect("required result inserted"));
        }
        ensure!(
            self.rows.is_empty(),
            "review contains unrequested result identities"
        );
        if let Some(engine) = output
            .iter_mut()
            .find(|row| row.check_id == "engine-integrity")
        {
            // Surface coordinator failures to existing human/SARIF consumers,
            // which read findings rather than the extended incomplete ledger.
            for obligation in &self.incomplete {
                if let Some(name) = obligation["obligation"].as_str() {
                    let mut failure = error(
                        "engine-integrity",
                        format!(
                            "{name}: {}",
                            obligation["reason"]
                                .as_str()
                                .unwrap_or("required observation incomplete")
                        ),
                    );
                    engine.findings.append(&mut failure.findings);
                    engine.status = CheckStatus::Error;
                }
            }
        }
        Ok((output, self.incomplete))
    }
}
