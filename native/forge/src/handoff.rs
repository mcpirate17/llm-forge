//! Native compatibility for durable, verified handoff envelopes. A projected
//! handoff never grants authority, releases claims or modifies a conversation.
#[path = "handoff_cli.rs"]
mod cli;
#[path = "handoff_store.rs"]
mod store;
use crate::{
    active_state::{self, ActiveState},
    instant, json_canon,
};
use anyhow::{ensure, Context, Result};
pub use cli::{run, HandoffArgs};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

const MAX_CONTEXT_CHARS: usize = 3500;
const MAX_TASK_CHARS: usize = 500;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    schema_version: u32,
    handoff_id: String,
    parent_handoff_id: Option<String>,
    created_at: String,
    activated_at: Option<String>,
    status: Status,
    task: String,
    paths: Vec<String>,
    runtime: BTreeMap<String, Value>,
    active_state: Value,
    active_state_digest: String,
    context: String,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Prepared,
    Active,
}

fn digest(value: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(json_canon::canonical_json(value).as_bytes())
    )
}
fn state_fingerprint(state: &Value) -> Result<String> {
    let mut state = state
        .as_object()
        .context("handoff state must be an object")?
        .clone();
    state.remove("last_updated");
    Ok(digest(&Value::Object(state)))
}
fn validate_state(state: &Value, now: f64) -> Result<()> {
    let parsed: ActiveState = serde_json::from_value(state.clone())?;
    active_state::validate_active_state(&parsed, now).map_err(anyhow::Error::msg)
}
fn fit_text(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.into();
    }
    let prefix: String = text.chars().take(max - 1).collect();
    format!("{}…", prefix.trim_end())
}
fn redact(text: &str) -> String {
    static PATTERNS: OnceLock<[Regex; 3]> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            Regex::new(r"\bsk-[A-Za-z0-9_-]{12,}\b").expect("constant regex"),
            Regex::new(r"(?i)(authorization\s*:\s*bearer\s+)[^\s]+").expect("constant regex"),
            Regex::new(r"(?i)(api[_-]?key\s*[=:]\s*)[^\s,;]+").expect("constant regex"),
        ]
    });
    let mut text = patterns[0].replace_all(text, "[REDACTED]").into_owned();
    for pattern in &patterns[1..] {
        text = pattern.replace_all(&text, "${1}[REDACTED]").into_owned();
    }
    text
}

impl Envelope {
    fn validate(&self) -> Result<()> {
        ensure!(self.schema_version == 1, "unsupported handoff schema");
        ensure!(
            !self.handoff_id.is_empty()
                && self.handoff_id.len() <= 128
                && !self.task.trim().is_empty()
                && self.task.chars().count() <= MAX_TASK_CHARS,
            "invalid handoff ID/task bounds"
        );
        ensure!(
            self.parent_handoff_id
                .as_ref()
                .is_none_or(|id| !id.is_empty() && id.len() <= 128 && *id != self.handoff_id),
            "invalid parent handoff ID"
        );
        ensure!(
            self.paths.len() <= 128 && self.runtime.len() <= 64,
            "handoff metadata count limit"
        );
        for path in &self.paths {
            ensure!(path.len() <= 4096, "handoff path too long");
            forge::source_file::validate_relative(Path::new(path))?;
        }
        for (key, value) in &self.runtime {
            ensure!(
                !key.is_empty()
                    && key.len() <= 128
                    && (value.is_null()
                        || value.is_boolean()
                        || value.as_str().is_some_and(|v| v.len() <= 4096)
                        || value.as_i64().is_some()
                        || value.as_u64().is_some()),
                "runtime metadata must contain bounded scalar strings, integers, booleans or null"
            );
        }
        ensure!(
            self.context.chars().count() <= MAX_CONTEXT_CHARS,
            "handoff context exceeds character cap"
        );
        ensure!(
            self.active_state_digest == state_fingerprint(&self.active_state)?,
            "handoff active-state digest mismatch"
        );
        let created = instant::parse(&self.created_at)
            .context("handoff created_at requires a valid timezone")?;
        validate_state(&self.active_state, created)?;
        match self.status {
            Status::Prepared => ensure!(
                self.activated_at.is_none(),
                "prepared handoff has activation time"
            ),
            Status::Active => {
                let activated = self
                    .activated_at
                    .as_deref()
                    .and_then(instant::parse)
                    .context("active handoff needs valid activation time")?;
                ensure!(activated >= created, "handoff activation predates creation");
            }
        }
        Ok(())
    }
    fn payload(&self) -> Result<Value> {
        self.validate()?;
        let mut value = serde_json::to_value(self)?;
        value["integrity_sha256"] = Value::String(digest(&value));
        Ok(value)
    }
    fn from_value(mut value: Value) -> Result<Self> {
        let integrity = value
            .as_object_mut()
            .context("handoff must be an object")?
            .remove("integrity_sha256")
            .context("handoff integrity missing")?;
        ensure!(
            integrity.as_str() == Some(digest(&value).as_str()),
            "handoff envelope integrity check failed"
        );
        let envelope: Self = serde_json::from_value(value)?;
        envelope.validate()?;
        Ok(envelope)
    }
    fn live_context(&self) -> String {
        let runtime = self
            .runtime
            .iter()
            .map(|(k, v)| format!("{k}={}", scalar(v)))
            .collect::<Vec<_>>()
            .join(", ");
        fit_text(&format!("HANDOFF {}: {}\nGOVERNANCE: revalidate live claims and approvals before mutation.\nRUNTIME: {}\nWORKING CONTEXT (non-authoritative; verify against durable sources):\n{}",
            self.handoff_id,self.task,if runtime.is_empty(){"none"}else{&runtime},self.context),MAX_CONTEXT_CHARS)
    }
}
fn scalar(value: &Value) -> String {
    match value {
        Value::String(v) => v.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        v => v.to_string(),
    }
}
struct Preparation<'a> {
    task: &'a str,
    paths: Vec<String>,
    runtime: BTreeMap<String, Value>,
    context: &'a str,
    parent: Option<String>,
}
fn prepare(request: Preparation<'_>, state: Value, now: f64) -> Result<Envelope> {
    validate_state(&state, now)?;
    ensure!(
        request.context.len() <= 1024 * 1024,
        "handoff context input exceeds 1 MiB"
    );
    let envelope = Envelope {
        schema_version: 1,
        handoff_id: forge::resource_id("handoff")?,
        parent_handoff_id: request.parent,
        created_at: instant::isoformat_utc(now),
        activated_at: None,
        status: Status::Prepared,
        task: request.task.trim().into(),
        paths: request.paths,
        runtime: request.runtime,
        active_state_digest: state_fingerprint(&state)?,
        active_state: state,
        context: fit_text(&redact(request.context), MAX_CONTEXT_CHARS),
    };
    envelope.validate()?;
    Ok(envelope)
}

fn activate(path: &Path, state: &Value, now: f64) -> Result<(Envelope, bool)> {
    let _lock = store::lock(path)?;
    let mut envelope = store::load(path)?;
    validate_state(state, now)?;
    ensure!(
        state_fingerprint(state)? == envelope.active_state_digest,
        "handoff is stale: live mandates, headings or claims changed"
    );
    let already = envelope.status == Status::Active;
    if !already {
        envelope.status = Status::Active;
        envelope.activated_at = Some(instant::isoformat_utc(now));
        store::save(path, &envelope)?;
    }
    Ok((envelope, already))
}

pub fn hook_context(root: &Path, identity: &str) -> Result<String> {
    validate_identity(identity)?;
    let path = staged_path(root, identity);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error.into()),
    }
    let state = serde_json::to_value(active_state::generate_active_state(root)?)?;
    match activate(&path,&state,instant::now()) {
        Ok((envelope,false))=>Ok(envelope.live_context()), Ok((_,true))=>Ok(String::new()),
        Err(error)=>Ok(format!("HANDOFF NOT ACTIVATED for {identity}: {error}. Staged envelope {} was left untouched; re-stage with `forge handoff stage --task ...`.",path.display())),
    }
}
fn validate_identity(identity: &str) -> Result<()> {
    ensure!(
        !identity.is_empty()
            && identity.len() <= 64
            && identity.as_bytes()[0].is_ascii_alphanumeric()
            && identity
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid handoff identity"
    );
    Ok(())
}
fn staged_path(root: &Path, identity: &str) -> std::path::PathBuf {
    root.join(".agents/handoff")
        .join(identity)
        .join("pending.json")
}
pub fn hook_output(context: &str) -> Value {
    let mut value = json!({"hookSpecificOutput":{"hookEventName":"SessionStart"}});
    if !context.is_empty() {
        value["hookSpecificOutput"]["additionalContext"] = Value::String(context.into());
    }
    value
}

#[cfg(test)]
#[path = "handoff_tests.rs"]
mod tests;
