use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq, Ord, PartialOrd)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Passed,
    Failed,
    Error,
    Skipped,
    Cached,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub check_id: String,
    pub rule_id: String,
    pub severity: Severity,
    pub message: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub line: Option<i64>,
    #[serde(default)]
    pub column: Option<i64>,
    #[serde(default)]
    pub help: Option<String>,
    #[serde(default)]
    pub evidence: Map<String, Value>,
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default)]
    pub exception_id: Option<String>,
    #[serde(default)]
    pub inherited: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckResult {
    pub check_id: String,
    pub status: CheckStatus,
    pub duration_ms: u64,
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub tool_version: Option<String>,
    #[serde(default)]
    pub cache_key: Option<String>,
    #[serde(default)]
    pub cache_hit: bool,
    #[serde(default)]
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub skipped_reason: Option<String>,
    #[serde(default)]
    pub stdout_tail: String,
    #[serde(default)]
    pub stderr_tail: String,
    #[serde(default)]
    pub metrics: Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StaleException {
    pub id: String,
    pub check: String,
    pub rule: String,
    pub path: String,
    pub owner: String,
    pub expires: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Assessment {
    pub decision: &'static str,
    pub findings: Vec<Finding>,
    pub stale_exceptions: Vec<StaleException>,
    pub scope: &'static str,
    pub review_complete: bool,
    pub execution_verified: bool,
}
