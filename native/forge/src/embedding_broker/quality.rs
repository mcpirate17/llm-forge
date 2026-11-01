//! Measured quality policy and vector-space-pinned route selection.
use super::config::{self, Env, ResolvedRoute, RoutingConfig};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum QualityStatus {
    Pass,
    Fail,
    Unknown,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityResult {
    pub status: QualityStatus,
    pub route_fingerprint: String,
    pub score: Option<f64>,
    pub threshold: Option<f64>,
    #[serde(default)]
    pub fixture_sha256: String,
}

#[derive(Clone, Debug, Default)]
pub struct QualityReceipt {
    pub sha256: String,
    pub routes: BTreeMap<String, QualityResult>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u32,
    routes: BTreeMap<String, QualityResult>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionReason {
    Primary,
    QualityFallback,
    AvailabilityFallback,
    PinnedIndex,
}
impl SelectionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::QualityFallback => "quality-fallback",
            Self::AvailabilityFallback => "availability-fallback",
            Self::PinnedIndex => "pinned-index",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RouteAttempt {
    pub route: ResolvedRoute,
    pub reason: SelectionReason,
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

impl QualityReceipt {
    pub fn load(path: Option<&Path>, env: &Env) -> Result<Self> {
        let configured = env
            .get("WORKSPACE_EMBED_QUALITY_RECEIPT")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        let Some(path) = path.or_else(|| configured.map(Path::new)) else {
            return Ok(Self::default());
        };
        Self::parse(
            &crate::bounded_file::read(path, MAX_BYTES)
                .context("read embedding quality receipt")?,
        )
    }

    pub fn parse(raw: &[u8]) -> Result<Self> {
        ensure!(
            raw.len() <= MAX_BYTES,
            "embedding quality receipt exceeds 1 MiB"
        );
        let text = std::str::from_utf8(raw).context("embedding quality receipt must be UTF-8")?;
        let value = crate::tool_schema::parse_document(text)
            .map_err(|_| anyhow::anyhow!("invalid embedding quality JSON"))?;
        let document: Document = serde_json::from_value(value)
            .map_err(|_| anyhow::anyhow!("invalid embedding quality schema"))?;
        ensure!(
            document.schema_version == 1 && document.routes.len() <= 256,
            "unsupported embedding quality schema or route count"
        );
        for (route_id, result) in &document.routes {
            config::id(route_id)?;
            ensure!(
                result
                    .route_fingerprint
                    .strip_prefix("sha256:")
                    .is_some_and(digest),
                "invalid quality route fingerprint"
            );
            ensure!(
                result.fixture_sha256.is_empty() || digest(&result.fixture_sha256),
                "invalid quality fixture digest"
            );
            ensure!(
                [result.score, result.threshold]
                    .into_iter()
                    .flatten()
                    .all(f64::is_finite),
                "quality scores must be finite"
            );
            if let (Some(score), Some(threshold)) = (result.score, result.threshold) {
                ensure!(
                    match result.status {
                        QualityStatus::Pass => score >= threshold,
                        QualityStatus::Fail => score < threshold,
                        QualityStatus::Unknown => true,
                    },
                    "quality status contradicts threshold"
                );
            }
        }
        Ok(Self {
            sha256: format!("{:x}", Sha256::digest(raw)),
            routes: document.routes,
        })
    }

    fn status(&self, route: &ResolvedRoute) -> Result<QualityStatus> {
        let Some(result) = self.routes.get(&route.route_id) else {
            return Ok(QualityStatus::Unknown);
        };
        ensure!(
            result.route_fingerprint == route.fingerprint(),
            "quality receipt is bound to another vector space"
        );
        Ok(result.status)
    }
}

pub fn route_attempts(
    config: &RoutingConfig,
    quality: &QualityReceipt,
    required_fingerprint: &str,
    env: &Env,
) -> Result<Vec<RouteAttempt>> {
    let mut resolved = BTreeMap::new();
    for spec in &config.routes {
        if let Some(route) = spec.resolve(env)? {
            ensure!(
                resolved.insert(route.route_id.clone(), route).is_none(),
                "ambiguous embedding route IDs"
            );
        }
    }
    if !required_fingerprint.is_empty() {
        let mut matches = resolved
            .into_values()
            .filter(|route| route.fingerprint() == required_fingerprint);
        let route = matches
            .next()
            .context("no configured route matches required vector space")?;
        ensure!(
            matches.next().is_none(),
            "ambiguous pinned embedding vector space"
        );
        return Ok(vec![RouteAttempt {
            route,
            reason: SelectionReason::PinnedIndex,
        }]);
    }
    let primary = resolved.remove(&config.primary_route);
    let status = primary
        .as_ref()
        .map(|route| quality.status(route))
        .transpose()?
        .unwrap_or(QualityStatus::Unknown);
    let fallback_reason = if status == QualityStatus::Fail {
        SelectionReason::QualityFallback
    } else {
        SelectionReason::AvailabilityFallback
    };
    let mut attempts = Vec::new();
    if let Some(route) = primary.filter(|_| status != QualityStatus::Fail) {
        attempts.push(RouteAttempt {
            route,
            reason: SelectionReason::Primary,
        });
    }
    for id in &config.fallback_routes {
        if let Some(route) = resolved.remove(id) {
            if quality.status(&route)? != QualityStatus::Fail {
                attempts.push(RouteAttempt {
                    route,
                    reason: fallback_reason,
                });
            }
        }
    }
    ensure!(
        !attempts.is_empty(),
        "no approved embedding route is available for current quality policy"
    );
    Ok(attempts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn receipt(fingerprint: &str, status: &str, score: f64) -> QualityReceipt {
        QualityReceipt::parse(
            json!({"schema_version":1,"routes":{"local-qwen3":{
                "status":status,"route_fingerprint":fingerprint,"score":score,"threshold":0.5
            }}})
            .to_string()
            .as_bytes(),
        )
        .unwrap()
    }

    fn two_local_routes() -> RoutingConfig {
        let mut config = RoutingConfig::load(None).unwrap();
        let mut fallback = config.routes[0].clone();
        fallback.route_id = "fallback".into();
        config.fallback_routes = vec![fallback.route_id.clone()];
        config.routes = vec![config.routes[0].clone(), fallback];
        config
    }

    #[test]
    fn receipt_hashes_original_bytes_and_rejects_malformed_evidence() {
        let raw = br#"{"schema_version":1,"routes":{}}"#;
        let parsed = QualityReceipt::parse(raw).unwrap();
        assert_eq!(
            parsed.sha256,
            "6ae2e7f26873be311ea841e52e904a19562465c1772f988edf72e25dfa8cb21d"
        );
        assert_ne!(
            parsed.sha256,
            QualityReceipt::parse(b"{\"schema_version\":1, \"routes\":{}}")
                .unwrap()
                .sha256
        );
        for raw in [
            r#"{"schema_version":2,"routes":{}}"#,
            r#"{"schema_version":1,"schema_version":1,"routes":{}}"#,
            r#"{"schema_version":1,"routes":{},"secret":"never-display"}"#,
        ] {
            assert!(QualityReceipt::parse(raw.as_bytes()).is_err());
        }
        let fingerprint = format!("sha256:{}", "a".repeat(64));
        for (status, score, threshold) in [
            ("PASS", json!(0.1), json!(0.5)),
            ("FAIL", json!(0.5), json!(0.5)),
            ("PASS", json!(true), json!(0.5)),
            ("PASS", json!("NaN"), json!(0.5)),
            ("INVALID", json!(1), json!(0)),
        ] {
            let raw = json!({"schema_version":1,"routes":{"local":{"status":status,
                "route_fingerprint":fingerprint,"score":score,"threshold":threshold}}});
            assert!(QualityReceipt::parse(raw.to_string().as_bytes()).is_err());
        }
        let unknown = receipt(&fingerprint, "UNKNOWN", 0.1);
        assert_eq!(unknown.routes["local-qwen3"].status, QualityStatus::Unknown);
        assert!(QualityReceipt::parse(&vec![b' '; MAX_BYTES + 1]).is_err());
    }

    #[test]
    fn quality_fallback_and_availability_order_are_distinct() {
        let mut config = two_local_routes();
        let env = Env::new();
        let fingerprint = config.routes[0]
            .resolve(&env)
            .unwrap()
            .unwrap()
            .fingerprint();
        let normal = route_attempts(&config, &QualityReceipt::default(), "", &env).unwrap();
        assert_eq!(
            normal.iter().map(|a| a.reason.as_str()).collect::<Vec<_>>(),
            vec!["primary", "availability-fallback"]
        );
        let failed = receipt(&fingerprint, "FAIL", 0.1);
        let fallback = route_attempts(&config, &failed, "", &env).unwrap();
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].route.route_id, "fallback");
        assert_eq!(fallback[0].reason.as_str(), "quality-fallback");
        config.routes[0].enabled_env = "ENABLE_LOCAL".into();
        let unavailable = route_attempts(&config, &failed, "", &env).unwrap();
        assert_eq!(unavailable[0].reason.as_str(), "availability-fallback");
        config.routes[1].enabled_env = "ENABLE_FALLBACK".into();
        assert!(route_attempts(&config, &failed, "", &env).is_err());
    }

    #[test]
    fn pin_is_exact_and_never_quality_or_availability_fallback() {
        let config = two_local_routes();
        let env = Env::new();
        let fingerprint = config.routes[0]
            .resolve(&env)
            .unwrap()
            .unwrap()
            .fingerprint();
        let failed = receipt(&fingerprint, "FAIL", 0.1);
        let pinned = route_attempts(&config, &failed, &fingerprint, &env).unwrap();
        assert_eq!(pinned.len(), 1);
        assert_eq!(pinned[0].reason.as_str(), "pinned-index");
        let other = format!("sha256:{}", "b".repeat(64));
        let stale = receipt(&other, "PASS", 1.0);
        assert!(route_attempts(&config, &stale, "", &env).is_err());
        assert!(route_attempts(&config, &stale, &other, &env).is_err());
        assert_eq!(
            route_attempts(&config, &stale, &fingerprint, &env)
                .unwrap()
                .len(),
            1
        );
    }
}
