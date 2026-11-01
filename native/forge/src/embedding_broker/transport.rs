//! Bounded backend transport. Never include URLs, credentials, or server bodies in errors.
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::io::Read;
use std::time::{Duration, Instant};

const MAX_RESPONSE: usize = 32 * 1024 * 1024;

pub(super) fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .context("embedding request deadline expired")
}
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .max_response_header_size(16384)
        .http_status_as_error(false)
        .build()
        .new_agent()
}
fn read_json(
    mut response: ureq::http::Response<ureq::Body>,
    deadline: Instant,
    limit: usize,
) -> Result<Value> {
    ensure!(
        (1..=MAX_RESPONSE).contains(&limit),
        "invalid embedding response limit"
    );
    ensure!(
        response.status().is_success(),
        "embedding backend returned HTTP {}",
        response.status().as_u16()
    );
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("embedding backend response read failed"))?;
    ensure!(
        bytes.len() <= limit,
        "embedding backend response exceeds configured limit"
    );
    remaining(deadline)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| anyhow::anyhow!("embedding backend returned invalid JSON"))?;
    crate::tool_schema::parse_document_with_limit(text, limit)
        .map_err(|_| anyhow::anyhow!("embedding backend returned invalid JSON"))
}

pub(super) fn get_json(endpoint: &str, deadline: Instant, max_bytes: usize) -> Result<Value> {
    ensure!(
        (1..=MAX_RESPONSE).contains(&max_bytes),
        "invalid embedding response limit"
    );
    let response = agent()
        .get(endpoint)
        .config()
        .timeout_global(Some(remaining(deadline)?))
        .build()
        .call()
        .map_err(|_| anyhow::anyhow!("embedding backend request failed"))?;
    read_json(response, deadline, max_bytes)
}

/// `authorization` is a bearer token; it is never included in errors.
pub(super) fn post_json(
    endpoint: &str,
    body: &Value,
    authorization: Option<&str>,
    deadline: Instant,
) -> Result<Value> {
    post_with_agent(&agent(), endpoint, body, authorization, deadline)
}

/// Shared bounded JSON transport. Caller admits endpoint and bounds the request.
pub(crate) fn post_json_limited(
    endpoint: &str,
    body: &Value,
    authorization: Option<&str>,
    deadline: Instant,
    max_response: usize,
) -> Result<Value> {
    post_with_agent_limit(
        &agent(),
        endpoint,
        body,
        authorization,
        deadline,
        max_response,
    )
}

fn post_with_agent(
    agent: &ureq::Agent,
    endpoint: &str,
    body: &Value,
    authorization: Option<&str>,
    deadline: Instant,
) -> Result<Value> {
    post_with_agent_limit(agent, endpoint, body, authorization, deadline, MAX_RESPONSE)
}

fn post_with_agent_limit(
    agent: &ureq::Agent,
    endpoint: &str,
    body: &Value,
    authorization: Option<&str>,
    deadline: Instant,
    max_response: usize,
) -> Result<Value> {
    ensure!(
        (1..=MAX_RESPONSE).contains(&max_response),
        "invalid backend response limit"
    );
    let encoded = serde_json::to_vec(body).context("encoding backend request")?;
    let mut request = agent
        .post(endpoint)
        .header("Content-Type", "application/json");
    if let Some(secret) = authorization {
        request = request.header("Authorization", &format!("Bearer {secret}"));
    }
    let response = request
        .config()
        .timeout_global(Some(remaining(deadline)?))
        .build()
        .send(encoded.as_slice())
        .map_err(|_| anyhow::anyhow!("embedding backend request failed"))?;
    read_json(response, deadline, max_response)
}

#[cfg(test)]
#[path = "transport_tls_tests.rs"]
mod tls_tests;
