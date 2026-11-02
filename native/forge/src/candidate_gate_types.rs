use serde::Serialize;
use serde_json::Value;
#[derive(Clone, Serialize)]
pub(super) struct Phase {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    pub evidence: Value,
}
impl Phase {
    pub fn new(name: &str, ok: bool, detail: impl Into<String>, evidence: Value) -> Self {
        Self {
            name: name.into(),
            ok,
            detail: detail.into(),
            evidence,
        }
    }
}
#[derive(Clone, Serialize)]
pub(super) struct ToolStatus {
    pub tool_id: String,
    pub executable: String,
    pub found: bool,
    pub resolved_path: Option<String>,
    pub version: Option<String>,
    pub expected_version: String,
    pub matches_expected: bool,
    pub provided_by: String,
}
