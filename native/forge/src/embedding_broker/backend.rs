//! Broker route execution and legacy OpenAI-compatible response encoding.
use super::{
    config::{Env, ResolvedRoute, RoutingConfig},
    device,
    quality::{self, QualityReceipt},
    transport,
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const DEFAULT_OLLAMA: &str = "http://127.0.0.1:11434/api/embed";
pub struct BackendOptions {
    pub timeout: Duration,
    pub allow_ollama_start: bool,
}

/// Estimated exposure is reserved before dispatch and remains reserved after failure.
struct Reservation {
    ceiling: f64,
    reserved: f64,
}
impl Reservation {
    fn new(attempts: &[quality::RouteAttempt]) -> Self {
        let ceiling = attempts
            .iter()
            .filter(|a| a.route.paid)
            .map(|a| a.route.max_request_usd)
            .fold(0.0_f64, f64::max);
        Self {
            ceiling,
            reserved: 0.0,
        }
    }
    fn admit(&mut self, paid: bool, cost: f64, route_cap: f64) -> bool {
        if !paid {
            return true;
        }
        let total = self.reserved + cost;
        if cost > route_cap || !total.is_finite() || total > self.ceiling {
            return false;
        }
        self.reserved = total;
        true
    }
}

/// Execute one approved route, retaining the historical route fingerprint and metadata.
pub fn embed(
    texts: &[String],
    pin: &str,
    config: &RoutingConfig,
    quality: &QualityReceipt,
    env: &Env,
    options: &BackendOptions,
) -> Result<Value> {
    ensure!(
        !options.timeout.is_zero(),
        "embedding deadline must be positive"
    );
    ensure!(
        (1..=128).contains(&texts.len())
            && texts.iter().all(|s| !s.is_empty())
            && texts
                .iter()
                .try_fold(0usize, |n, s| n.checked_add(s.len()))
                .is_some_and(|n| n <= 1024 * 1024),
        "embedding input requires 1..128 nonempty texts totaling at most 1 MiB"
    );
    let deadline = Instant::now()
        .checked_add(options.timeout)
        .context("embedding deadline too large")?;
    let attempts = quality::route_attempts(config, quality, pin, env)?;
    let mut failures = Vec::new();
    let mut budget = Reservation::new(&attempts);
    for attempt in attempts {
        transport::remaining(deadline)?;
        let route = &attempt.route;
        ensure!(
            (1..=8192).contains(&route.dimension),
            "embedding route dimension outside 1..8192"
        );
        let clipped: Vec<String> = texts
            .iter()
            .map(|text| text.chars().take(route.max_input_chars).collect())
            .collect();
        ensure!(
            clipped.iter().all(|s| !s.is_empty()),
            "embedding route produced empty input"
        );
        let cost = route.estimate_request_cost(&clipped);
        ensure!(
            cost.is_finite() && cost >= 0.0 && route.max_request_usd.is_finite(),
            "invalid embedding route cost"
        );
        if !budget.admit(route.paid, cost, route.max_request_usd) {
            failures.push("paid route exceeds its cost limit or request budget".to_owned());
            continue;
        }
        match execute(route, &clipped, env, options, deadline) {
            Ok((vectors, gpu)) => {
                return response(
                    route,
                    vectors,
                    gpu,
                    cost,
                    &quality.sha256,
                    attempt.reason.as_str(),
                    &budget,
                )
            }
            Err(error) => failures.push(error.to_string()),
        }
        if !pin.is_empty() {
            break;
        }
    }
    anyhow::bail!(
        "all approved embedding routes failed: {}",
        failures.join("; ")
    )
}

fn execute(
    route: &ResolvedRoute,
    texts: &[String],
    env: &Env,
    options: &BackendOptions,
    deadline: Instant,
) -> Result<(Vec<Vec<f64>>, Option<u32>)> {
    let (vectors, gpu) = match route.protocol.as_str() {
        "ollama-embed" => {
            let mut gpu = device::choose_with_timeout(env, transport::remaining(deadline)?)?;
            let first = ollama(route, texts, gpu, deadline);
            let vectors = match first {
                Ok(vectors) => vectors,
                Err(_) if options.allow_ollama_start && route.endpoint == DEFAULT_OLLAMA => {
                    super::lifecycle::ensure_ollama(transport::remaining(deadline)?)
                        .map_err(|_| anyhow::anyhow!("local embedding server startup failed"))?;
                    gpu = device::choose_with_timeout(env, transport::remaining(deadline)?)?;
                    ollama(route, texts, gpu, deadline)?
                }
                Err(error) => return Err(error),
            };
            (vectors, Some(gpu))
        }
        "openai-embeddings" => (paid(route, texts, env, deadline)?, None),
        _ => anyhow::bail!("unsupported embedding route protocol"),
    };
    Ok((validate_vectors(vectors, texts.len(), route)?, gpu))
}

fn ollama(
    route: &ResolvedRoute,
    texts: &[String],
    gpu: u32,
    deadline: Instant,
) -> Result<Vec<Vec<f64>>> {
    let mut output = Vec::with_capacity(texts.len());
    let count = texts.chunks(16).len();
    for (batch_number, batch) in texts.chunks(16).enumerate() {
        let clipped: Vec<String> = batch
            .iter()
            .map(|text| text.chars().take(2000).collect())
            .collect();
        let body = json!({"model":route.model,"input":clipped,"options":{"num_ctx":2048,"num_thread":8,"num_gpu":gpu},
            "keep_alive":if batch_number + 1 == count { json!(0) } else { json!("5s") }});
        let value = transport::post_json(&route.endpoint, &body, None, deadline)?;
        let rows = value
            .get("embeddings")
            .and_then(Value::as_array)
            .context("local embedding response lacks vectors")?;
        ensure!(
            rows.len() == batch.len(),
            "local embedding response count mismatch"
        );
        for row in rows {
            output.push(vector(row)?);
        }
    }
    Ok(output)
}

fn paid(
    route: &ResolvedRoute,
    texts: &[String],
    env: &Env,
    deadline: Instant,
) -> Result<Vec<Vec<f64>>> {
    let secret = env
        .get(&route.api_key_env)
        .filter(|v| !v.trim().is_empty())
        .context("paid embedding credential unavailable")?;
    let value = transport::post_json(
        &route.endpoint,
        &json!({"model":route.model,"input":texts}),
        Some(secret),
        deadline,
    )?;
    let rows = value
        .get("data")
        .and_then(Value::as_array)
        .context("paid embedding response lacks vectors")?;
    ensure!(
        rows.len() == texts.len(),
        "paid embedding response count mismatch"
    );
    let mut output: Vec<Option<Vec<f64>>> = vec![None; texts.len()];
    for (position, row) in rows.iter().enumerate() {
        let index = match row.get("index") {
            Some(v) => v
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .context("invalid paid embedding index")?,
            None => position,
        };
        ensure!(
            index < output.len() && output[index].is_none(),
            "invalid or duplicate paid embedding index"
        );
        output[index] = Some(vector(
            row.get("embedding")
                .context("paid embedding vector missing")?,
        )?);
    }
    output
        .into_iter()
        .map(|row| row.context("missing paid embedding index"))
        .collect()
}

fn vector(value: &Value) -> Result<Vec<f64>> {
    let values = value
        .as_array()
        .context("embedding vector must be an array")?;
    ensure!(
        !values.is_empty() && values.len() <= 8192,
        "invalid embedding vector length"
    );
    values
        .iter()
        .map(|v| {
            v.as_f64()
                .filter(|v| v.is_finite())
                .context("embedding vector contains invalid number")
        })
        .collect()
}

fn validate_vectors(
    vectors: Vec<Vec<f64>>,
    count: usize,
    route: &ResolvedRoute,
) -> Result<Vec<Vec<f64>>> {
    ensure!(vectors.len() == count, "embedding response count mismatch");
    vectors
        .into_iter()
        .map(|vector| {
            ensure!(
                vector.len() == route.dimension && vector.iter().all(|v| v.is_finite()),
                "embedding vector dimension or numeric values invalid"
            );
            if route.normalization == "l2-client" {
                let norm = vector.iter().map(|v| v * v).sum::<f64>().sqrt();
                ensure!(
                    norm.is_finite() && norm > 0.0,
                    "embedding vector must be finite and nonzero"
                );
                conductor_native::kb_retrieve::l2_normalize(&vector).map_err(anyhow::Error::msg)
            } else {
                Ok(vector)
            }
        })
        .collect()
}

fn response(
    route: &ResolvedRoute,
    vectors: Vec<Vec<f64>>,
    gpu: Option<u32>,
    cost: f64,
    receipt: &str,
    reason: &str,
    budget: &Reservation,
) -> Result<Value> {
    let mut metadata = route.public_metadata();
    let fields = metadata
        .as_object_mut()
        .context("invalid public route metadata")?;
    fields.insert("selection_reason".into(), json!(reason));
    fields.insert("quality_receipt_sha256".into(), json!(receipt));
    fields.insert("estimated_cost_usd".into(), json!(cost));
    fields.insert("reserved_cost_usd".into(), json!(budget.reserved));
    fields.insert("request_budget_usd".into(), json!(budget.ceiling));
    fields.insert("num_gpu".into(), json!(gpu));
    fields.insert(
        "num_ctx".into(),
        if gpu.is_some() {
            json!(2048)
        } else {
            Value::Null
        },
    );
    fields.insert(
        "keep_alive".into(),
        if gpu.is_some() { json!(0) } else { Value::Null },
    );
    let data: Vec<_> = vectors
        .into_iter()
        .enumerate()
        .map(|(index, embedding)| json!({"object":"embedding","index":index,"embedding":embedding}))
        .collect();
    Ok(
        json!({"object":"list","model":route.model,"data":data,"usage":{"prompt_tokens":0,"total_tokens":0},"workspace_embedding":metadata}),
    )
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
