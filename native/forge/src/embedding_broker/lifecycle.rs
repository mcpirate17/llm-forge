//! Serialized native startup. Never replace or terminate an existing listener.
use super::{config::Env, transport, Settings};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    fs,
    net::{SocketAddr, TcpStream},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
// Device selection may spend up to two seconds in a bounded nvidia-smi probe.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
const OLLAMA_HEALTH: &str = "http://127.0.0.1:11434/api/tags";

#[derive(Serialize)]
pub struct EnsureReport {
    pub ok: bool,
    pub started: bool,
    pub health: String,
    pub pid: Option<u32>,
}

fn runtime_root(selected: Option<&Path>) -> Result<PathBuf> {
    let path = selected.map(Path::to_owned).unwrap_or_else(|| {
        std::env::temp_dir().join(format!("forge-embedding-broker-{}", unsafe {
            libc::geteuid()
        }))
    });
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)?;
    let meta = fs::symlink_metadata(&path)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "broker runtime directory must be an owned private directory"
    );
    Ok(path)
}

fn startup_lock(path: &Path, deadline: Instant) -> Result<crate::file_lock::Lease> {
    loop {
        if let Some(lease) = crate::file_lock::try_owned(path, true)? {
            return Ok(lease);
        }
        let left = transport::remaining(deadline)?;
        std::thread::sleep(left.min(Duration::from_millis(25)));
    }
}

fn listening(address: SocketAddr) -> Result<bool> {
    match TcpStream::connect_timeout(&address, Duration::from_millis(150)) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => Ok(false),
        Err(_) => anyhow::bail!("cannot establish whether the loopback endpoint is occupied"),
    }
}

fn health_at(url: &str, deadline: Instant) -> Result<Value> {
    let value = transport::get_json(url, deadline.min(Instant::now() + HEALTH_TIMEOUT), 65536)?;
    ensure!(value.is_object(), "health response must be a JSON object");
    Ok(value)
}

pub fn status(settings: &Settings) -> Result<Value> {
    settings.validate()?;
    health_at(&settings.url("/health"), Instant::now() + HEALTH_TIMEOUT)
}

fn existing(settings: &Settings, expected: &str, deadline: Instant) -> Result<bool> {
    match health_at(&settings.url("/health"), deadline) {
        Ok(value) => {
            ensure!(value["ok"] == true && value["route"]["fingerprint"].as_str() == Some(expected),
                "running embedding broker route does not match this process; explicit restart required");
            Ok(true)
        }
        Err(_) if !listening(settings.bind)? => Ok(false),
        Err(_) => anyhow::bail!(
            "loopback endpoint is occupied by an unhealthy or unexpected service; refusing startup"
        ),
    }
}

fn spawn(mut command: Command) -> Result<Child> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // setsid is async-signal-safe and touches no parent state after fork.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().context("starting native embedding service")
}

fn detach_reaper(mut child: Child) {
    std::thread::spawn(move || {
        if let Err(error) = child.wait() {
            eprintln!("forge: embedding service wait failed: {error}");
        }
    });
}

fn wait_for_child(
    child: &mut Child,
    url: &str,
    expected: Option<&str>,
    deadline: Instant,
) -> Result<()> {
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("embedding service exited during startup: {status}");
        }
        if let Ok(value) = health_at(url, deadline) {
            let valid = if let Some(expected) = expected {
                value["ok"] == true
                    && value["route"]["fingerprint"].as_str() == Some(expected)
                    && value["broker_pid"].as_u64() == Some(u64::from(child.id()))
            } else {
                value["models"].is_array()
            };
            if valid {
                let address = url
                    .strip_prefix("http://")
                    .and_then(|s| s.split('/').next())
                    .context("startup health endpoint lacks literal address")?
                    .parse()?;
                crate::process_listener::owned(address, child.id())?;
                return Ok(());
            }
        }
        let left = transport::remaining(deadline)?;
        std::thread::sleep(left.min(Duration::from_millis(25)));
    }
}

fn finish_start(
    mut child: Child,
    url: &str,
    expected: Option<&str>,
    deadline: Instant,
) -> Result<u32> {
    if let Err(error) = wait_for_child(&mut child, url, expected, deadline) {
        if child.try_wait()?.is_none() {
            child.kill().context("stopping failed owned startup")?;
            child.wait()?;
        }
        return Err(error);
    }
    let pid = child.id();
    detach_reaper(child);
    Ok(pid)
}

pub fn ensure_ollama(timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout.min(STARTUP_TIMEOUT);
    let address: SocketAddr = "127.0.0.1:11434".parse()?;
    if health_at(OLLAMA_HEALTH, deadline).is_ok_and(|v| v["models"].is_array()) {
        return Ok(false);
    }
    ensure!(
        !listening(address)?,
        "default Ollama endpoint is occupied but unhealthy"
    );
    let root = runtime_root(None)?;
    let _lock = startup_lock(&root.join("ollama.lock"), deadline)?;
    if health_at(OLLAMA_HEALTH, deadline).is_ok_and(|v| v["models"].is_array()) {
        return Ok(false);
    }
    ensure!(
        !listening(address)?,
        "default Ollama endpoint became occupied but unhealthy"
    );
    let mut command = Command::new("ollama");
    command.arg("serve").env("OLLAMA_HOST", "127.0.0.1:11434");
    let child = spawn(command)?;
    finish_start(child, OLLAMA_HEALTH, None, deadline)?;
    Ok(true)
}

fn command(settings: &Settings) -> Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command.args([
        "embedding-broker",
        "serve",
        "--host",
        &settings.bind.ip().to_string(),
        "--port",
        &settings.bind.port().to_string(),
        "--timeout-seconds",
        &settings.timeout.as_secs().to_string(),
    ]);
    if let Some(lifetime) = settings.lifetime {
        command
            .arg("--lifetime-seconds")
            .arg(lifetime.as_secs().to_string());
    }
    if let Some(path) = &settings.routes {
        command.arg("--routes").arg(path);
    }
    if let Some(path) = &settings.quality {
        command.arg("--quality").arg(path);
    }
    if !settings.allow_ollama_start {
        command.arg("--no-ollama-start");
    }
    Ok(command)
}

pub fn ensure(settings: &Settings, env: &Env, state: Option<&Path>) -> Result<EnsureReport> {
    settings.validate()?;
    let (attempts, _) = settings.attempts(env)?;
    let route = &attempts
        .first()
        .context("no approved embedding route")?
        .route;
    let expected = route.fingerprint();
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let report = |started, pid| EnsureReport {
        ok: true,
        started,
        health: settings.url("/health"),
        pid,
    };
    if existing(settings, &expected, deadline)? {
        return Ok(report(false, None));
    }
    let root = runtime_root(state)?;
    let _lock = startup_lock(
        &root.join(format!("broker-{}.lock", settings.bind.port())),
        deadline,
    )?;
    if existing(settings, &expected, deadline)? {
        return Ok(report(false, None));
    }
    if route.protocol == "ollama-embed"
        && route.endpoint == "http://127.0.0.1:11434/api/embed"
        && settings.allow_ollama_start
    {
        ensure_ollama(transport::remaining(deadline)?)?;
    }
    let pid = finish_start(
        spawn(command(settings)?)?,
        &settings.url("/health"),
        Some(&expected),
        deadline,
    )?;
    Ok(report(true, Some(pid)))
}

pub fn warm(settings: &Settings, env: &Env, state: Option<&Path>) -> Result<Value> {
    ensure(settings, env, state)?;
    let started = Instant::now();
    let value = transport::post_json(
        &settings.url("/v1/embeddings"),
        &json!({"input":["warm"],"workspace_purpose":"document"}),
        None,
        started + settings.timeout,
    )?;
    let rows = value["data"]
        .as_array()
        .context("warm-up response lacks vectors")?;
    ensure!(rows.len() == 1, "warm-up response must contain one vector");
    let vector = rows[0]["embedding"]
        .as_array()
        .context("warm-up response lacks vector")?;
    ensure!(
        !vector.is_empty()
            && vector.len() <= 8192
            && vector
                .iter()
                .all(|n| n.as_f64().is_some_and(f64::is_finite)),
        "invalid warm-up vector"
    );
    Ok(
        json!({"ok":true,"seconds":started.elapsed().as_secs_f64(),"dimension":vector.len(),
        "model":value["model"],"estimated_cost_usd":value["workspace_embedding"]["estimated_cost_usd"]}),
    )
}
