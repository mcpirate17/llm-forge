//! Opt-in deterministic read advice over caller-supplied facts only.
//! No file lookup, diagnostic prose interpretation or execution authority.
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::{path::Path, sync::OnceLock};

pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_SYMBOL_BYTES: usize = 256;
pub const MAX_ERROR_BYTES: usize = 4096;
pub const MAX_RESULTS: usize = 100;
pub const MAX_INSPECTION_BYTES: usize = 8192;

pub(crate) fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}
fn results() -> usize {
    20
}
fn inspection_bytes() -> usize {
    2048
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    InspectFailure,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Rustc,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecentError {
    pub kind: ErrorKind,
    pub exit_code: u16,
    #[serde(default, deserialize_with = "present")]
    pub code: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub message: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub path: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub line: Option<u64>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnownInspection {
    pub path: String,
    pub source_sha256: String,
    pub complete: bool,
    #[serde(default, deserialize_with = "present")]
    pub symbol: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub intent: Intent,
    /// Caller filter only; true never authorizes execution or a filesystem path.
    pub allow_read_advice: bool,
    #[serde(default, deserialize_with = "present")]
    pub path: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub symbol: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub source_sha256: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub recent_error: Option<RecentError>,
    #[serde(default, deserialize_with = "present")]
    pub known_inspection: Option<KnownInspection>,
    #[serde(default = "results")]
    pub max_results: usize,
    #[serde(default = "inspection_bytes")]
    pub max_bytes: usize,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    InspectSource,
    ReadAdviceDisabled,
    LocationRequired,
    LocationConflict,
    KnownInspectionReported,
}
#[derive(Debug, Serialize)]
pub struct Arguments {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub max_results: usize,
    pub max_bytes: usize,
}
#[derive(Debug, Serialize)]
pub struct Call {
    pub name: &'static str,
    pub arguments: Arguments,
}
#[derive(Debug, Serialize)]
pub struct Advice {
    pub decision: Decision,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<Call>,
    pub freshness: &'static str,
    pub purpose: &'static str,
    pub caveat: &'static str,
}
impl Request {
    /// Counts every supplied string value, including fixed enum spellings;
    /// JSON syntax is independently covered by raw request byte limits.
    pub(crate) fn text_bytes(&self) -> Result<usize, String> {
        let error = self.recent_error.as_ref();
        let known = self.known_inspection.as_ref();
        let fields = [
            self.path.as_deref(),
            self.symbol.as_deref(),
            self.source_sha256.as_deref(),
            error.and_then(|e| e.code.as_deref()),
            error.and_then(|e| e.message.as_deref()),
            error.and_then(|e| e.path.as_deref()),
            known.map(|k| k.path.as_str()),
            known.map(|k| k.source_sha256.as_str()),
            known.and_then(|k| k.symbol.as_deref()),
        ];
        let base = "inspect_failure".len() + if error.is_some() { "rustc".len() } else { 0 };
        fields.into_iter().flatten().try_fold(base, |total, text| {
            total
                .checked_add(text.len())
                .ok_or_else(|| "next-action text size overflow".into())
        })
    }
    fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_RESULTS).contains(&self.max_results)
            || !(512..=MAX_INSPECTION_BYTES).contains(&self.max_bytes)
        {
            return Err("next-action read bounds: max_results 1..100, max_bytes 512..8192".into());
        }
        for path in self
            .path
            .iter()
            .chain(self.recent_error.iter().filter_map(|e| e.path.as_ref()))
            .chain(self.known_inspection.iter().map(|k| &k.path))
        {
            path_value(path)?;
        }
        for symbol in self.symbol.iter().chain(
            self.known_inspection
                .iter()
                .filter_map(|k| k.symbol.as_ref()),
        ) {
            short(symbol, MAX_SYMBOL_BYTES)?;
        }
        for hash in self
            .source_sha256
            .iter()
            .chain(self.known_inspection.iter().map(|k| &k.source_sha256))
        {
            sha256(hash)?;
        }
        if let Some(error) = &self.recent_error {
            if !(1..=255).contains(&error.exit_code)
                || error
                    .line
                    .is_some_and(|line| !(1..=10_000_000).contains(&line))
            {
                return Err("recent-error exit code or source line exceeds bounds".into());
            }
            if let Some(code) = &error.code {
                short(code, 64)?;
            }
            if error
                .message
                .as_ref()
                .is_some_and(|text| text.len() > MAX_ERROR_BYTES)
            {
                return Err("recent-error message exceeds 4096 bytes".into());
            }
        }
        Ok(())
    }
}
fn short(text: &str, max: usize) -> Result<(), String> {
    if text.trim().is_empty() || text.len() > max || text.chars().any(char::is_control) {
        return Err(
            "next-action path, selector or code must be bounded nonblank text without controls"
                .into(),
        );
    }
    Ok(())
}
fn path_value(path: &str) -> Result<(), String> {
    short(path, MAX_PATH_BYTES)?;
    crate::source_file::validate_relative(Path::new(path))
        .map_err(|e| format!("next-action path: {e}"))
}
fn sha256(value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("next-action fingerprint must be lowercase SHA-256".into());
    }
    Ok(())
}
fn response(decision: Decision, call: Option<Call>) -> Advice {
    Advice {
        decision,
        call,
        freshness: "not_independently_verified",
        purpose: "Read source signatures and locations.",
        caveat: "Advisory only; grants no authority. Caller facts are unverified. Results may truncate and may omit diagnostic-line context.",
    }
}
pub(crate) fn analyze(request: &Request) -> Result<Advice, String> {
    request.validate()?;
    let error_path = request
        .recent_error
        .as_ref()
        .and_then(|error| error.path.as_deref());
    if matches!((request.path.as_deref(), error_path), (Some(a), Some(b)) if a != b) {
        return Ok(response(Decision::LocationConflict, None));
    }
    if !request.allow_read_advice {
        return Ok(response(Decision::ReadAdviceDisabled, None));
    }
    let Some(path) = request.path.as_deref().or(error_path) else {
        return Ok(response(Decision::LocationRequired, None));
    };
    if request.known_inspection.as_ref().is_some_and(|known| {
        known.complete
            && known.path == path
            && known.symbol == request.symbol
            && request.source_sha256.as_deref() == Some(known.source_sha256.as_str())
    }) {
        return Ok(response(Decision::KnownInspectionReported, None));
    }
    let arguments = Arguments {
        path: path.into(),
        symbol: request.symbol.clone(),
        max_results: request.max_results,
        max_bytes: request.max_bytes,
    };
    validate_arguments(&arguments)?;
    Ok(response(
        Decision::InspectSource,
        Some(Call {
            name: "code_inspect",
            arguments,
        }),
    ))
}
fn validate_arguments(arguments: &Arguments) -> Result<(), String> {
    static SCHEMA: OnceLock<Result<crate::tool_schema::CompiledSchema, String>> = OnceLock::new();
    let schema = SCHEMA
        .get_or_init(|| {
            crate::tool_schema::CompiledSchema::compile(
                &crate::code_tools::inspection_definition()["inputSchema"],
            )
            .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)?;
    let input = serde_json::to_string(arguments).map_err(|e| e.to_string())?;
    schema
        .validate(&input)
        .map_err(|e| format!("read advice does not match code_inspect schema: {e}"))
}
fn string(min: usize, max: usize) -> Value {
    json!({"type":"string","minLength":min,"maxLength":max})
}
fn integer(min: usize, max: usize) -> Value {
    json!({"type":"integer","minimum":min,"maximum":max})
}
pub(crate) fn request_schema(text_cap: usize) -> Value {
    let path = string(1, text_cap.min(MAX_PATH_BYTES));
    let symbol = string(1, text_cap.min(MAX_SYMBOL_BYTES));
    let known = json!({"type":"object","additionalProperties":false,"required":["path","source_sha256","complete"],
        "properties":{"path":path.clone(),"source_sha256":string(64,64),"symbol":symbol.clone(),"complete":{"type":"boolean"}}});
    let error = json!({"type":"object","additionalProperties":false,"required":["kind","exit_code"],
        "properties":{"kind":{"type":"string","enum":["rustc"]},"exit_code":integer(1,255),
            "code":string(1,text_cap.min(64)),"message":string(0,text_cap.min(MAX_ERROR_BYTES)),
            "path":path.clone(),"line":integer(1,10_000_000)}});
    json!({"type":"object","additionalProperties":false,"required":["intent","allow_read_advice"],"properties":{
        "intent":{"type":"string","enum":["inspect_failure"]},"allow_read_advice":{"type":"boolean"},
        "path":path,"symbol":symbol,"source_sha256":string(64,64),"known_inspection":known,"recent_error":error,
        "max_results":integer(1,MAX_RESULTS),"max_bytes":integer(512,MAX_INSPECTION_BYTES)}})
}

#[cfg(test)]
#[path = "prompt_next_action_tests.rs"]
mod tests;
