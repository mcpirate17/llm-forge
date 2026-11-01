//! Bounded native client for an explicitly configured local Ollama embedding
//! service. It never pulls models, follows redirects, or uses environment proxies.
use anyhow::{ensure, Context, Result};
use conductor_native::{
    sha256_hex,
    vector_index::{normalize, Space, MAX_DIMENSION},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub endpoint: String,
    pub model: String,
    #[serde(default)]
    pub runner: String,
    #[serde(with = "conductor_native::sha256_hex")]
    pub artifact_sha256: [u8; 32],
    pub dimension: usize,
    pub context_tokens: usize,
    pub threads: usize,
    #[serde(default)]
    pub gpu_layers: usize,
    pub timeout_seconds: u64,
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        validate_endpoint(&self.endpoint)?;
        ensure!(
            !self.model.is_empty()
                && self.model.len() <= 256
                && self.model.contains(':')
                && self
                    .model
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"/_.:-".contains(&c)),
            "embedding model must be an explicit ASCII model:tag"
        );
        ensure!(
            self.runner.len() <= 128
                && self
                    .runner
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)),
            "invalid embedding runner"
        );
        ensure!(
            self.artifact_sha256 != [0; 32]
                && (1..=MAX_DIMENSION).contains(&self.dimension)
                && (128..=32768).contains(&self.context_tokens)
                && (1..=256).contains(&self.threads)
                && self.gpu_layers <= 256
                && (1..=60).contains(&self.timeout_seconds),
            "invalid embedding identity, dimensions, resources, or deadline"
        );
        Ok(())
    }
    /// The embedding-space identity binds the artifact AND numerical/runtime
    /// recipe. Endpoint and timeout do not change the vector space.
    pub fn space(&self) -> Result<Space> {
        self.validate()?;
        let recipe = json!({"protocol":"ollama-embed-v1","normalization":"l2-f32-v1","truncate":false,
            "artifact":self.artifact_sha256,"model":self.model,"runner":self.runner,
            "dimension":self.dimension,"context_tokens":self.context_tokens,"threads":self.threads,"gpu_layers":self.gpu_layers});
        Ok(Space {
            dimension: self.dimension,
            model_sha256: Sha256::digest(serde_json::to_vec(&recipe)?).into(),
        })
    }
}

pub struct Client {
    config: Config,
    http: ureq::Agent,
    space: Space,
}
#[derive(Debug, Serialize)]
pub struct Embedding {
    pub space: Space,
    pub vector: Vec<f32>,
    pub input_tokens: u64,
}
#[derive(Debug, Serialize)]
pub struct Batch {
    pub space: Space,
    pub vectors: Vec<Vec<f32>>,
    pub input_tokens_total: u64,
}
#[derive(Deserialize)]
struct EmbedResponse {
    model: String,
    embeddings: Vec<Vec<f32>>,
    prompt_eval_count: u64,
}
impl Client {
    pub fn new(config: Config) -> Result<Self> {
        let space = config.space()?;
        let http = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .max_response_header_size(16384)
            .max_idle_connections(2)
            .http_status_as_error(false)
            .build()
            .new_agent();
        Ok(Self {
            config,
            http,
            space,
        })
    }
    pub fn space(&self) -> &Space {
        &self.space
    }
    /// All identity checks and inference share one deadline. A disconnected
    /// request does not prove the independently owned model server stopped work.
    pub fn embed(&self, text: &str) -> Result<Embedding> {
        let mut batch = self.embed_batch(
            &[text],
            Instant::now() + Duration::from_secs(self.config.timeout_seconds),
        )?;
        Ok(Embedding {
            space: batch.space,
            vector: batch.vectors.pop().context("empty validated batch")?,
            input_tokens: batch.input_tokens_total,
        })
    }
    /// Bounded batched input with an optional stricter caller deadline. Input and
    /// output row order are preserved. Token usage belongs to the whole batch.
    pub fn embed_batch(&self, texts: &[&str], caller_deadline: Instant) -> Result<Batch> {
        ensure!(
            (1..=32).contains(&texts.len()) && texts.iter().all(|text| !text.is_empty() && text.len() <= 16384 && !text.contains('\0'))
                && texts.iter().map(|text| text.len()).sum::<usize>() <= 65536,
            "embedding batch requires 1..32 nonempty texts, <=16 KiB each and <=64 KiB total, without NUL"
        );
        let deadline =
            caller_deadline.min(Instant::now() + Duration::from_secs(self.config.timeout_seconds));
        self.verify_model(deadline)?;
        let body = serde_json::to_vec(
            &json!({"model":self.config.model,"runner":self.config.runner,"input":texts,
            "truncate":false,"dimensions":self.config.dimension,"keep_alive":"60s",
            "options":{"num_ctx":self.config.context_tokens,"num_thread":self.config.threads,"num_gpu":self.config.gpu_layers}}),
        )?;
        let response = self
            .http
            .post(format!("{}/api/embed", self.config.endpoint))
            .header("Content-Type", "application/json")
            .config()
            .timeout_global(Some(remaining(deadline)?))
            .build()
            .send(&body)?;
        let limit = 4096 + texts.len() * self.space.dimension * 32;
        let mut data: EmbedResponse =
            serde_json::from_slice(&response_bytes(response, limit as u64)?)?;
        ensure!(
            data.model == self.config.model,
            "embedding response model mismatch"
        );
        ensure!(
            data.embeddings.len() == texts.len(),
            "embedding server returned wrong batch size"
        );
        for vector in &mut data.embeddings {
            ensure!(
                vector.len() == self.space.dimension,
                "embedding server returned wrong dimension"
            );
            normalize(vector)?;
        }
        ensure!(
            data.prompt_eval_count > 0
                && data.prompt_eval_count <= (self.config.context_tokens * texts.len()) as u64,
            "embedding token usage exceeds the pinned context"
        );
        self.verify_model(deadline)?;
        Ok(Batch {
            space: self.space.clone(),
            vectors: data.embeddings,
            input_tokens_total: data.prompt_eval_count,
        })
    }
    fn verify_model(&self, deadline: Instant) -> Result<()> {
        let response = self
            .http
            .get(format!("{}/api/tags", self.config.endpoint))
            .config()
            .timeout_global(Some(remaining(deadline)?))
            .build()
            .call()?;
        let data = response_json(response)?;
        let models = data
            .get("models")
            .and_then(Value::as_array)
            .context("model inventory lacks models")?;
        ensure!(models.len() <= 4096, "model inventory exceeds limit");
        let mut matches = models.iter().filter(|model| {
            model.get("name").and_then(Value::as_str) == Some(&self.config.model)
                && model
                    .pointer("/details/runner")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    == self.config.runner
        });
        let model = matches
            .next()
            .context("pinned local model/runner is not installed")?;
        ensure!(matches.next().is_none(), "ambiguous local model inventory");
        for key in ["remote_host", "remote_model"] {
            ensure!(
                model.get(key).is_none_or(|v| v.as_str() == Some("")),
                "remote-backed embedding model is not a local capability"
            );
        }
        ensure!(
            model
                .get("size")
                .and_then(Value::as_u64)
                .is_some_and(|size| size > 0),
            "local model inventory lacks artifact size"
        );
        let digest = model
            .get("digest")
            .and_then(Value::as_str)
            .context("model inventory lacks digest")?;
        ensure!(
            sha256_hex::parse(digest.strip_prefix("sha256:").unwrap_or(digest))
                .map_err(anyhow::Error::msg)?
                == self.config.artifact_sha256,
            "embedding model artifact changed"
        );
        Ok(())
    }
}
fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .context("embedding deadline expired")
}
fn response_json(response: ureq::http::Response<ureq::Body>) -> Result<Value> {
    Ok(serde_json::from_slice(&response_bytes(
        response,
        1024 * 1024,
    )?)?)
}
fn response_bytes(mut response: ureq::http::Response<ureq::Body>, limit: u64) -> Result<Vec<u8>> {
    let status = response.status();
    ensure!(
        status.as_u16() == 200,
        "embedding service returned HTTP {status}"
    );
    Ok(response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()?)
}
fn validate_endpoint(endpoint: &str) -> Result<()> {
    let uri: ureq::http::Uri = endpoint.parse()?;
    ensure!(uri.scheme_str() == Some("http") && uri.port_u16().is_some_and(|p| p > 0)
        && uri.path_and_query().is_none_or(|p| p.as_str() == "/") && !endpoint.ends_with('/') && !endpoint.contains('@'),
        "embedding endpoint must be http://IP:port without credentials, path, query, or trailing slash");
    let host = uri
        .host()
        .context("embedding endpoint lacks host")?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let address: IpAddr = host
        .parse()
        .context("embedding endpoint requires a literal local/private IP")?;
    let local = match address {
        IpAddr::V4(v) => v.is_loopback() || v.is_private(),
        IpAddr::V6(v) => v.is_loopback() || v.is_unique_local(),
    };
    ensure!(
        local,
        "embedding endpoint must be a loopback or private address"
    );
    Ok(())
}
