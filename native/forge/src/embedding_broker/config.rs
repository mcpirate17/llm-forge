//! Bounded routing configuration and legacy-compatible public vector identities.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub type Env = BTreeMap<String, String>;
const MAX_BYTES: usize = 1024 * 1024;
const DEFAULT: &str = include_str!("../../../../src/conductor/embedding_routes.toml");

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RouteSpec {
    pub route_id: String,
    pub backend_kind: String,
    pub protocol: String,
    pub priority: i64,
    pub normalization: String,
    pub tokenizer_policy: String,
    pub truncation_policy: String,
    pub max_input_chars: usize,
    pub device_policy: String,
    pub paid: bool,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub endpoint_env: String,
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default)]
    pub enabled_env: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub model_env: String,
    #[serde(default)]
    pub model_revision: String,
    #[serde(default)]
    pub model_revision_env: String,
    #[serde(default)]
    pub dimension: usize,
    #[serde(default)]
    pub dimension_env: String,
    #[serde(default)]
    pub cost_per_million_tokens_env: String,
    #[serde(default)]
    pub max_request_usd: f64,
}

#[derive(Clone, Debug)]
pub struct ResolvedRoute {
    pub route_id: String,
    pub backend_kind: String,
    pub protocol: String,
    pub priority: i64,
    pub normalization: String,
    pub tokenizer_policy: String,
    pub truncation_policy: String,
    pub max_input_chars: usize,
    pub device_policy: String,
    pub paid: bool,
    pub endpoint: String,
    pub api_key_env: String,
    pub model: String,
    pub model_revision: String,
    pub dimension: usize,
    pub cost_per_million_tokens: f64,
    pub max_request_usd: f64,
}

#[derive(Clone, Debug)]
pub struct RoutingConfig {
    pub primary_route: String,
    pub fallback_routes: Vec<String>,
    pub routes: Vec<RouteSpec>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    primary_route: String,
    fallback_routes: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u32,
    policy: Policy,
    route: Vec<RouteSpec>,
}

pub(super) fn id(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid embedding route ID"
    );
    Ok(())
}
fn text(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.chars().count() <= maximum && !value.chars().any(char::is_control)
}
fn env_name(value: &str) -> bool {
    value.is_empty()
        || (value.len() <= 128
            && value
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_'))
}
fn configured(env: &Env, name: &str, value: &str) -> String {
    if name.is_empty() {
        value.trim()
    } else {
        env.get(name).map(String::as_str).unwrap_or("").trim()
    }
    .to_owned()
}
fn endpoint(value: &str, paid: bool) -> Result<()> {
    ensure!(
        value.len() <= 4096 && !value.contains(['?', '#', '@']),
        "invalid embedding endpoint"
    );
    let uri: ureq::http::Uri = value
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid embedding endpoint"))?;
    let authority = uri
        .authority()
        .context("embedding endpoint requires an authority")?;
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    let scheme = uri
        .scheme_str()
        .context("embedding endpoint requires a scheme")?;
    ensure!(
        (loopback && scheme == "http") || (paid && !loopback && scheme == "https"),
        "local embeddings require loopback HTTP; remote paid embeddings require HTTPS"
    );
    let suffix = &authority.as_str()[authority.host().len()..];
    ensure!(
        suffix.is_empty()
            || suffix
                .strip_prefix(':')
                .and_then(|port| port.parse::<u16>().ok())
                .is_some_and(|port| port > 0),
        "invalid embedding endpoint port"
    );
    Ok(())
}

impl RoutingConfig {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        match path {
            None => Self::parse(DEFAULT),
            Some(path) => {
                let bytes = crate::bounded_file::read(path, MAX_BYTES)
                    .context("read embedding routing config")?;
                Self::parse(std::str::from_utf8(&bytes).context("embedding config must be UTF-8")?)
            }
        }
    }
    pub fn parse(raw: &str) -> Result<Self> {
        ensure!(raw.len() <= MAX_BYTES, "embedding config exceeds 1 MiB");
        let document: Document =
            toml::from_str(raw).map_err(|_| anyhow::anyhow!("invalid embedding routing TOML"))?;
        ensure!(
            document.schema_version == 1,
            "unsupported embedding routing schema"
        );
        ensure!(
            !document.route.is_empty() && document.route.len() <= 256,
            "embedding config requires 1..256 routes"
        );
        let mut ids = BTreeSet::new();
        for route in &document.route {
            route.validate()?;
            ensure!(
                ids.insert(route.route_id.clone()),
                "embedding route IDs must be unique"
            );
        }
        ensure!(
            document.policy.fallback_routes.len() <= 255,
            "too many embedding fallbacks"
        );
        let mut policy_ids = BTreeSet::from([document.policy.primary_route.clone()]);
        ensure!(
            ids.contains(&document.policy.primary_route),
            "embedding primary route is missing"
        );
        for fallback in &document.policy.fallback_routes {
            ensure!(
                ids.contains(fallback) && policy_ids.insert(fallback.clone()),
                "embedding fallback is missing or ambiguous"
            );
        }
        Ok(Self {
            primary_route: document.policy.primary_route,
            fallback_routes: document.policy.fallback_routes,
            routes: document.route,
        })
    }
}

impl RouteSpec {
    fn validate(&self) -> Result<()> {
        id(&self.route_id)?;
        ensure!(
            self.priority.unsigned_abs() <= 1_000_000
                && (1..=65536).contains(&self.max_input_chars)
                && self.dimension <= 8192,
            "invalid embedding route resource bounds"
        );
        ensure!(
            self.protocol != "ollama-embed" || self.max_input_chars <= 2000,
            "ollama-embed max_input_chars must be 1..2000 to match backend truncation"
        );
        ensure!(
            [
                &self.backend_kind,
                &self.protocol,
                &self.normalization,
                &self.tokenizer_policy,
                &self.truncation_policy,
                &self.device_policy
            ]
            .iter()
            .all(|value| text(value, 128)),
            "invalid embedding route policy fields"
        );
        ensure!(
            self.endpoint.len() <= 4096
                && self.model.len() <= 4096
                && self.model_revision.len() <= 4096,
            "embedding route fields exceed byte limits"
        );
        ensure!(
            [
                &self.endpoint_env,
                &self.api_key_env,
                &self.enabled_env,
                &self.model_env,
                &self.model_revision_env,
                &self.dimension_env,
                &self.cost_per_million_tokens_env
            ]
            .iter()
            .all(|name| env_name(name)),
            "invalid embedding environment variable name"
        );
        ensure!(
            self.max_request_usd.is_finite() && (0.0..=1e12).contains(&self.max_request_usd),
            "invalid embedding request cost limit"
        );
        Ok(())
    }
    fn resolve_cost(&self, env: &Env) -> Result<Option<f64>> {
        let mut cost = 0.0;
        if !self.cost_per_million_tokens_env.is_empty() {
            let value = env
                .get(&self.cost_per_million_tokens_env)
                .map(|s| s.trim())
                .unwrap_or("");
            if self.paid && value.is_empty() {
                return Ok(None);
            }
            cost = if value.is_empty() {
                0.0
            } else {
                value
                    .parse::<f64>()
                    .map_err(|_| anyhow::anyhow!("embedding cost must be numeric"))?
            };
            ensure!(
                cost.is_finite() && (0.0..=1e12).contains(&cost),
                "embedding cost must be finite and nonnegative"
            );
        }
        Ok(Some(cost))
    }

    pub fn resolve(&self, env: &Env) -> Result<Option<ResolvedRoute>> {
        self.validate()?;
        if !self.enabled_env.is_empty() && env.get(&self.enabled_env).map(|s| s.trim()) != Some("1")
        {
            return Ok(None);
        }
        if self.paid
            && !self.api_key_env.is_empty()
            && env
                .get(&self.api_key_env)
                .is_none_or(|s| s.trim().is_empty())
        {
            return Ok(None);
        }
        ensure!(
            [
                &self.endpoint_env,
                &self.model_env,
                &self.model_revision_env,
                &self.dimension_env,
                &self.cost_per_million_tokens_env
            ]
            .iter()
            .all(|name| env.get(*name).is_none_or(|value| value.len() <= 4096)),
            "embedding environment value exceeds byte limit"
        );
        let endpoint_value = configured(env, &self.endpoint_env, &self.endpoint);
        let model = configured(env, &self.model_env, &self.model);
        let model_revision = configured(env, &self.model_revision_env, &self.model_revision);
        let dimension = configured(
            env,
            &self.dimension_env,
            if self.dimension == 0 {
                String::new()
            } else {
                self.dimension.to_string()
            }
            .as_str(),
        );
        if [&endpoint_value, &model, &model_revision, &dimension]
            .iter()
            .any(|s| s.is_empty())
        {
            ensure!(
                self.paid,
                "local embedding route is missing required fields"
            );
            return Ok(None);
        }
        ensure!(
            text(&model, 1024) && text(&model_revision, 1024),
            "invalid embedding model identity"
        );
        let dimension: usize = dimension
            .parse()
            .map_err(|_| anyhow::anyhow!("embedding dimension must be an integer"))?;
        ensure!(
            (1..=8192).contains(&dimension),
            "embedding dimension must be 1..8192"
        );
        endpoint(&endpoint_value, self.paid)?;
        let Some(cost) = self.resolve_cost(env)? else {
            return Ok(None);
        };
        Ok(Some(ResolvedRoute {
            route_id: self.route_id.clone(),
            backend_kind: self.backend_kind.clone(),
            protocol: self.protocol.clone(),
            priority: self.priority,
            normalization: self.normalization.clone(),
            tokenizer_policy: self.tokenizer_policy.clone(),
            truncation_policy: self.truncation_policy.clone(),
            max_input_chars: self.max_input_chars,
            device_policy: self.device_policy.clone(),
            paid: self.paid,
            endpoint: endpoint_value,
            api_key_env: self.api_key_env.clone(),
            model,
            model_revision,
            dimension,
            cost_per_million_tokens: cost,
            max_request_usd: self.max_request_usd,
        }))
    }
}

impl ResolvedRoute {
    fn payload(&self) -> Value {
        json!({"contract_version":1,"route_id":self.route_id,"backend_kind":self.backend_kind,"protocol":self.protocol,
            "endpoint":self.endpoint,"model":self.model,"model_revision":self.model_revision,"dimension":self.dimension,
            "normalization":self.normalization,"tokenizer_policy":self.tokenizer_policy,"truncation_policy":self.truncation_policy,
            "max_input_chars":self.max_input_chars})
    }
    pub fn fingerprint(&self) -> String {
        let mut payload = self.payload();
        payload.sort_all_objects();
        let utf8 = serde_json::to_string(&payload).expect("fixed vector-space payload");
        let mut ascii = String::with_capacity(utf8.len());
        for ch in utf8.chars() {
            if ch.is_ascii() && ch != '\u{7f}' {
                ascii.push(ch);
            } else {
                for unit in ch.encode_utf16(&mut [0; 2]) {
                    use std::fmt::Write;
                    write!(ascii, "\\u{unit:04x}").expect("write string");
                }
            }
        }
        format!("sha256:{:x}", Sha256::digest(ascii.as_bytes()))
    }
    pub fn public_metadata(&self) -> Value {
        let mut metadata = self.payload();
        for (name, value) in [
            ("fingerprint", json!(self.fingerprint())),
            ("device_policy", json!(self.device_policy)),
            ("paid", json!(self.paid)),
            (
                "cost_per_million_tokens",
                json!(self.cost_per_million_tokens),
            ),
            ("max_request_usd", json!(self.max_request_usd)),
        ] {
            metadata[name] = value;
        }
        metadata
    }
    pub fn estimate_request_cost(&self, texts: &[String]) -> f64 {
        let characters = texts
            .iter()
            .fold(0usize, |sum, text| sum.saturating_add(text.chars().count()));
        (characters / 4).max(1) as f64 * self.cost_per_million_tokens / 1_000_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_python_fingerprints_include_utf16_surrogates() {
        let config = RoutingConfig::load(None).unwrap();
        let mut route = config.routes[0].resolve(&Env::new()).unwrap().unwrap();
        assert_eq!(
            route.fingerprint(),
            "sha256:11452959ec832f34c3258249cabf73c114282b9f6ad27ecf94b438598893537c"
        );
        route.model = "雪🦀/e\u{301}".into();
        route.model_revision = "版本é".into();
        assert_eq!(
            route.fingerprint(),
            "sha256:60d660a58aa360f78166ba8ed96036b52f71c55a848c87021b8ff5eb70c8710c"
        );
        let fingerprint = route.fingerprint();
        route.device_policy = "cpu".into();
        route.cost_per_million_tokens = 123.0;
        assert_eq!(route.fingerprint(), fingerprint);
    }

    #[test]
    fn ollama_input_limit_matches_wire_truncation_and_paid_limit_stays_larger() {
        let mut spec = RoutingConfig::load(None).unwrap().routes.remove(0);
        spec.max_input_chars = 2000;
        assert_eq!(
            spec.resolve(&Env::new()).unwrap().unwrap().max_input_chars,
            2000
        );
        spec.max_input_chars = 2001;
        assert_eq!(
            spec.resolve(&Env::new()).unwrap_err().to_string(),
            "ollama-embed max_input_chars must be 1..2000 to match backend truncation"
        );
        spec.protocol = "openai-embeddings".into();
        spec.paid = true;
        spec.max_input_chars = 65536;
        assert_eq!(
            spec.resolve(&Env::new()).unwrap().unwrap().max_input_chars,
            65536
        );
        spec.max_input_chars = 65537;
        assert!(spec.resolve(&Env::new()).is_err());
    }

    #[test]
    fn paid_environment_enablement_cost_and_public_metadata() {
        let config = RoutingConfig::load(None).unwrap();
        let spec = &config.routes[1];
        assert!(spec.resolve(&Env::new()).unwrap().is_none());
        let mut env: Env = [
            ("WORKSPACE_EMBED_PAID_ENABLED", "1"),
            (
                "WORKSPACE_EMBED_PAID_API_KEY",
                "private-key-never-published",
            ),
            (
                "WORKSPACE_EMBED_PAID_BASE_URL",
                "https://example.org/v1/embeddings",
            ),
            ("WORKSPACE_EMBED_PAID_MODEL", "model"),
            ("WORKSPACE_EMBED_PAID_MODEL_REVISION", "revision"),
            ("WORKSPACE_EMBED_PAID_DIMENSION", "1024"),
            ("WORKSPACE_EMBED_PAID_COST_PER_MILLION_TOKENS", "2"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let route = spec.resolve(&env).unwrap().unwrap();
        assert_eq!(
            route.estimate_request_cost(&["雪🦀abcde日".into()]),
            0.000004
        );
        assert_eq!(route.estimate_request_cost(&[]), 0.000002);
        assert!(!route.public_metadata().to_string().contains("private-key"));
        assert_eq!(route.api_key_env, "WORKSPACE_EMBED_PAID_API_KEY");
        for value in ["NaN", "inf", "-1", "secret-bad-cost"] {
            env.insert(
                "WORKSPACE_EMBED_PAID_COST_PER_MILLION_TOKENS".into(),
                value.into(),
            );
            let error = spec.resolve(&env).unwrap_err().to_string();
            assert!(!error.contains(value));
        }
        env.remove("WORKSPACE_EMBED_PAID_COST_PER_MILLION_TOKENS");
        assert!(spec.resolve(&env).unwrap().is_none());
    }

    #[test]
    fn malformed_or_ambiguous_configs_and_unsafe_endpoints_fail() {
        for raw in [
            DEFAULT.replace("schema_version = 1", "schema_version = 2"),
            DEFAULT.replace(
                "fallback_routes = [\"paid-openai-compatible\"]",
                "fallback_routes = [\"local-qwen3\"]",
            ),
            format!("{DEFAULT}\nunknown = 3"),
            "x".repeat(MAX_BYTES + 1),
        ] {
            assert!(RoutingConfig::parse(&raw).is_err());
        }
        for (url, paid) in [
            ("http://example.org", false),
            ("https://localhost", false),
            ("http://example.org", true),
            ("https://user:password@example.org", true),
            ("http://localhost/#fragment", false),
            ("http://localhost:0", false),
            ("http://localhost:bad", false),
        ] {
            assert!(endpoint(url, paid).is_err(), "{url}");
        }
        for (url, paid) in [
            ("http://[::1]:1234/api/embed", false),
            ("http://127.0.0.2:1234", false),
            ("https://example.org", true),
        ] {
            endpoint(url, paid).unwrap();
        }
    }

    #[test]
    fn endpoint_queries_never_publish_secrets_or_echo_them_in_errors() {
        let config = RoutingConfig::load(None).unwrap();
        for (url, paid) in [
            (
                "http://127.0.0.1:11434/api/embed?token=private-query-secret",
                false,
            ),
            (
                "https://example.org/v1/embeddings?api_key=private-query-secret",
                true,
            ),
        ] {
            let mut spec = config.routes[0].clone();
            spec.endpoint = url.into();
            spec.paid = paid;
            let error = spec.resolve(&Env::new()).unwrap_err().to_string();
            assert_eq!(error, "invalid embedding endpoint");
            assert!(!error.contains("private-query-secret"));
            assert!(!error.contains(url));
        }
    }
}
