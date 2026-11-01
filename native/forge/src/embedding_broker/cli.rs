//! Operator commands; no interpreter, provider download or implicit warm-up.
use super::{environment, lifecycle, service, Settings};
use anyhow::Result;
use clap::{Args, Subcommand};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    time::Duration,
};

#[derive(Args)]
pub struct BrokerArgs {
    #[command(subcommand)]
    action: Action,
    #[arg(long, global=true, default_value_t=IpAddr::V4(Ipv4Addr::LOCALHOST))]
    host: IpAddr,
    #[arg(long, global = true, default_value_t = 7317)]
    port: u16,
    #[arg(long, global = true)]
    routes: Option<PathBuf>,
    #[arg(long, global = true)]
    quality: Option<PathBuf>,
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[arg(long, global=true, default_value_t=300, value_parser=clap::value_parser!(u64).range(1..=3600))]
    timeout_seconds: u64,
    /// Optional finite lifetime; otherwise serve until explicitly stopped.
    #[arg(long, global=true, value_parser=clap::value_parser!(u64).range(1..=86400))]
    lifetime_seconds: Option<u64>,
    /// Refuse backend startup; use only an already running configured service.
    #[arg(long, global = true)]
    no_ollama_start: bool,
}

#[derive(Subcommand)]
enum Action {
    /// Serve the loopback embedding protocol, with an optional finite lifetime.
    Serve,
    /// Start this native executable only when no listener exists.
    Ensure,
    /// Read health without creating state or starting any process.
    Status,
    /// Explicitly ensure the service and execute one warm-up embedding.
    Warm,
}

pub fn run(args: BrokerArgs) -> Result<u8> {
    let settings = Settings {
        routes: args.routes,
        quality: args.quality,
        bind: SocketAddr::new(args.host, args.port),
        timeout: Duration::from_secs(args.timeout_seconds),
        lifetime: args.lifetime_seconds.map(Duration::from_secs),
        allow_ollama_start: !args.no_ollama_start,
    };
    settings.validate()?;
    let env = environment();
    let value = match args.action {
        Action::Serve => {
            service::serve(settings, env)?;
            return Ok(0);
        }
        Action::Ensure => serde_json::to_value(lifecycle::ensure(
            &settings,
            &env,
            args.state_dir.as_deref(),
        )?)?,
        Action::Warm => lifecycle::warm(&settings, &env, args.state_dir.as_deref())?,
        Action::Status => match lifecycle::status(&settings) {
            Ok(value) => value,
            Err(error) => {
                println!(
                    "{}",
                    serde_json::json!({"ok":false,"error":error.to_string()})
                );
                return Ok(2);
            }
        },
    };
    println!("{}", serde_json::to_string(&value)?);
    Ok(0)
}
