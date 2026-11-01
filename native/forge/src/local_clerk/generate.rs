//! Bounded, explicitly requested clerical generation. No environment reads.
use super::{
    exact_duplicate_groups, normalize_model_draft, read_sources, Settings, Source, APPLICATION,
    MAX_SOURCE_COUNT, MAX_TEXT_CHARS, MODEL,
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const MAX_RESPONSE_BYTES: usize = 64_000;
const MAX_REQUEST_BYTES: usize = 64_000;
const MAX_PROMPT_CHARS: usize = 4_000;
const INSTRUCTION: &str = concat!(
    "Task class: compaction. Produce a very compact clerical draft from the supplied ",
    "workspace material. Return JSON with exactly these keys: summary ",
    "(string <= 400 chars), source_decisions, open_tasks, todo_items, and ",
    "duplicate_candidates (each list has at most 3 strings <= 160 chars). Preserve ",
    "uncertainty, identify conflicts instead of resolving them, and never ",
    "invent missing facts. This is a draft for human review; do not state ",
    "that anything is approved, authorized, deleted, launched, or complete. ",
    "The following source envelope is valid JSON.\n\n"
);

#[derive(Clone, Debug)]
pub struct GenerateOptions {
    pub model: String,
    pub task_class: String,
    /// Plain loopback origin. `localhost` is pinned to 127.0.0.1 without DNS.
    pub endpoint: String,
    /// HTTP exchange only; admitted source reads, prompt construction and final
    /// provenance rehash run outside this deadline under their finite byte bounds.
    pub timeout: Duration,
}
impl GenerateOptions {
    fn admitted_endpoint(&self) -> Result<String> {
        ensure!(
            self.model == MODEL,
            "local clerk model must be exactly {MODEL}"
        );
        ensure!(
            !self.timeout.is_zero() && self.timeout <= Duration::from_secs(120),
            "clerk timeout must be greater than zero and at most 120 seconds"
        );
        ensure!(
            self.task_class.len() <= 128,
            "clerk task class exceeds byte limit"
        );
        crate::local_ai_policy::require_clerical_task(
            &self.task_class,
            "make a bounded summary of source text",
        )
        .map_err(|_| anyhow::anyhow!("local inference requires an explicit clerical task class"))?;
        endpoint(&self.endpoint)
    }
}

/// Only exact legacy loopback hostnames are admitted; authority is never echoed.
fn endpoint(raw: &str) -> Result<String> {
    let raw = raw.trim();
    ensure!(
        raw.len() <= 4096 && !raw.contains(['@', '?', '#', '%', '\\']),
        "clerk endpoint must be a plain loopback HTTP origin"
    );
    let uri: ureq::http::Uri = raw
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid clerk loopback origin"))?;
    ensure!(
        uri.scheme_str() == Some("http") && matches!(uri.path(), "" | "/"),
        "clerk endpoint must be a plain loopback HTTP origin"
    );
    let authority = uri
        .authority()
        .context("clerk endpoint requires a loopback authority")?;
    let hostname = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let ip = match hostname {
        host if host.eq_ignore_ascii_case("localhost") => IpAddr::V4(Ipv4Addr::LOCALHOST),
        "127.0.0.1" => IpAddr::V4(Ipv4Addr::LOCALHOST),
        "::1" => IpAddr::V6(Ipv6Addr::LOCALHOST),
        _ => anyhow::bail!("clerk endpoint must use an explicit loopback host"),
    };
    let suffix = &authority.as_str()[authority.host().len()..];
    let port = if suffix.is_empty() {
        80
    } else {
        suffix
            .strip_prefix(':')
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|p| *p > 0)
            .context("invalid clerk endpoint port")?
    };
    Ok(format!("http://{}/api/generate", SocketAddr::new(ip, port)))
}

fn excerpt(text: &str, limit: usize) -> String {
    let length = text.chars().count();
    if length <= limit {
        return text.to_owned();
    }
    if limit == 0 {
        return String::new();
    }
    let prefix: String = text.chars().take(limit - 1).collect();
    format!("{}…", prefix.trim_end())
}
fn render(sources: &[Source], limit: usize) -> Result<(String, Vec<usize>)> {
    let mut counts = Vec::with_capacity(sources.len());
    let mut material = Vec::with_capacity(sources.len());
    for source in sources {
        let text = excerpt(&source.text, limit);
        let count = text.chars().count();
        counts.push(count);
        let mut metadata = serde_json::to_value(source.metadata(Some(count)))?;
        metadata
            .as_object_mut()
            .context("invalid clerk source metadata")?
            .insert("text".into(), json!(text));
        material.push(metadata);
    }
    let envelope = serde_json::to_string(&json!({"schema_version":1,"sources":material}))?;
    Ok((format!("{INSTRUCTION}{envelope}"), counts))
}
fn prompt(sources: &[Source]) -> Result<(String, Vec<usize>)> {
    ensure!(
        !sources.is_empty() && sources.len() <= MAX_SOURCE_COUNT,
        "invalid clerk source count"
    );
    let (mut low, mut high) = (0, MAX_TEXT_CHARS);
    let mut best = None;
    while low <= high {
        let midpoint = (low + high) / 2;
        let candidate = render(sources, midpoint)?;
        if candidate.0.chars().count() <= MAX_PROMPT_CHARS {
            best = Some(candidate);
            low = midpoint + 1;
        } else if midpoint == 0 {
            break;
        } else {
            high = midpoint - 1;
        }
    }
    let best = best.context("source metadata leaves too little prompt room")?;
    ensure!(
        sources
            .iter()
            .zip(&best.1)
            .all(|(source, chars)| *chars >= 128.min(source.text.chars().count())),
        "source metadata leaves too little prompt room; use fewer or shorter paths"
    );
    Ok(best)
}

/// Extract a complete object while tracking JSON strings; strict parser rejects
/// duplicate keys, excessive nesting and malformed escapes in that object.
fn model_json(raw: &str) -> Result<Value> {
    ensure!(
        raw.len() <= MAX_RESPONSE_BYTES,
        "clerk model response exceeds byte limit"
    );
    let mut candidate = raw.trim();
    if candidate.starts_with("```") {
        candidate = candidate
            .split_once('\n')
            .map(|(_, rest)| rest)
            .context("clerk model response has an invalid code fence")?;
        candidate = candidate.strip_suffix("```").unwrap_or(candidate).trim();
    }
    if let Ok(value) = crate::tool_schema::parse_document_with_limit(candidate, MAX_RESPONSE_BYTES)
    {
        return normalize_model_draft(&value);
    }
    let start = candidate
        .find('{')
        .context("clerk model response lacks a JSON object")?;
    let mut depth = 0_usize;
    let (mut quoted, mut escaped) = (false, false);
    for (offset, byte) in candidate.as_bytes()[start..].iter().copied().enumerate() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    ensure!(depth <= 32, "clerk model JSON depth exceeded");
                }
                b'}' | b']' => {
                    depth = depth.checked_sub(1).context("invalid clerk model JSON")?;
                    if depth == 0 {
                        let text = &candidate[start..=start + offset];
                        let value =
                            crate::tool_schema::parse_document_with_limit(text, MAX_RESPONSE_BYTES)
                                .context("invalid clerk model JSON")?;
                        return normalize_model_draft(&value);
                    }
                }
                _ => {}
            }
        }
    }
    anyhow::bail!("clerk model JSON object is incomplete")
}

pub fn draft(settings: &Settings, paths: &[PathBuf], options: &GenerateOptions) -> Result<Value> {
    let endpoint = options.admitted_endpoint()?;
    let sources = read_sources(settings, paths)?;
    let (prompt, counts) = prompt(&sources)?;
    let request = json!({"model":MODEL,"system":crate::local_ai_policy::CLERK_SYSTEM_PROMPT,
        "prompt":prompt,"stream":false,"format":"json","think":false,"keep_alive":-1,
        "options":{"num_ctx":2048,"num_predict":384,"temperature":0.1}});
    ensure!(
        serde_json::to_vec(&request)?.len() <= MAX_REQUEST_BYTES,
        "clerk request exceeds byte limit"
    );
    let deadline = Instant::now()
        .checked_add(options.timeout)
        .context("clerk deadline overflow")?;
    let payload = crate::embedding_broker::post_json_limited(
        &endpoint,
        &request,
        None,
        deadline,
        MAX_RESPONSE_BYTES,
    )?;
    // This checks the server's reported label, not model-artifact attestation.
    ensure!(
        payload.get("model").and_then(Value::as_str) == Some(MODEL),
        "local clerk returned a missing or mismatched model label"
    );
    let raw = payload
        .get("response")
        .and_then(Value::as_str)
        .context("local clerk returned no response text")?;
    let model_draft = model_json(raw)?;
    let eval_count = match payload.get("eval_count") {
        None | Some(Value::Null) => 0,
        Some(value) => value
            .as_u64()
            .context("local clerk returned an invalid eval_count")?,
    };
    let document = json!({"schema_version":1,"authority":"local-clerk-only","task_class":"compaction",
        "created_at":crate::instant::isoformat_utc(crate::instant::now()),"model":MODEL,
        "eval_count":eval_count,"sources":sources.iter().zip(counts).map(|(source,count)|
            source.metadata(Some(count))).collect::<Vec<_>>(),
        "exact_duplicate_groups":exact_duplicate_groups(&sources),"draft":model_draft,
        "application":APPLICATION});
    // Inference may outlast source stability. Rehash complete bounded sources
    // after the reply; the publisher must revalidate again before publication.
    super::validate_value(settings, &document)?;
    Ok(document)
}

#[cfg(test)]
#[path = "generate_tests.rs"]
mod tests;
