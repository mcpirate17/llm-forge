//! Native loopback embedding broker with explicit route and vector-space policy.
pub mod backend;
pub mod cli;
pub mod config;
pub mod device;
pub mod lifecycle;
pub mod quality;
pub mod service;
mod transport;
pub(crate) use transport::post_json_limited;

use anyhow::{ensure, Result};
use config::{Env, RoutingConfig};
use quality::{route_attempts, QualityReceipt, RouteAttempt};
use std::{net::SocketAddr, path::PathBuf, time::Duration};

#[derive(Clone)]
pub struct Settings {
    pub routes: Option<PathBuf>,
    pub quality: Option<PathBuf>,
    pub bind: SocketAddr,
    pub timeout: Duration,
    pub lifetime: Option<Duration>,
    pub allow_ollama_start: bool,
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.bind.ip().is_loopback() && self.bind.port() != 0,
            "embedding broker requires a nonzero loopback address"
        );
        ensure!(
            (1..=3600).contains(&self.timeout.as_secs()),
            "request timeout must be 1..3600 seconds"
        );
        if let Some(lifetime) = self.lifetime {
            ensure!(
                (1..=86400).contains(&lifetime.as_secs()),
                "broker lifetime must be 1..86400 seconds"
            );
        }
        Ok(())
    }

    pub fn policy(&self, env: &Env) -> Result<(RoutingConfig, QualityReceipt)> {
        Ok((
            RoutingConfig::load(self.routes.as_deref())?,
            QualityReceipt::load(self.quality.as_deref(), env)?,
        ))
    }

    pub fn attempts(&self, env: &Env) -> Result<(Vec<RouteAttempt>, QualityReceipt)> {
        let (config, quality) = self.policy(env)?;
        Ok((route_attempts(&config, &quality, "", env)?, quality))
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.bind)
    }
}

pub fn environment() -> Env {
    std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect()
}
