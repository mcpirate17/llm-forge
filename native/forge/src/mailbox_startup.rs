//! Native receive bootstrap and bounded sender-scoped retry/preview.
use super::{registry, store::Store, view};
use anyhow::{ensure, Context, Result};
use clap::Args;
use forge::process_group::ProcessGroup;
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Args)]
pub struct StartupArgs {
    #[arg(long)]
    pub(super) identity: String,
    /// Maximum time waiting for the native receiver's readiness record.
    #[arg(long, default_value_t = 3, value_parser=clap::value_parser!(u64).range(1..=10))]
    pub(super) serve_timeout_seconds: u64,
    /// Automatically started receivers expire; the next startup can renew them.
    #[arg(long, default_value_t = 86400, value_parser=clap::value_parser!(u64).range(1..=86400))]
    pub(super) lifetime_seconds: u64,
}

pub(super) fn owned_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.nlink() == 1
            && meta.mode() & 0o077 == 0,
        "mailbox startup file must be private, owned and singly linked"
    );
    Ok(file)
}

pub(super) fn startup_lock(root: &Path, identity: &str) -> Result<forge::file_lock::Exclusive> {
    let dir = root.join(identity);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)?;
    let meta = fs::symlink_metadata(&dir)?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "mailbox identity directory must be private and owned"
    );
    let file = owned_file(&dir.join(".startup.lock"))?;
    forge::file_lock::Exclusive::try_new(file, "mailbox startup")
}

pub(super) fn ready_record(
    group: &mut ProcessGroup,
    timeout: Duration,
    interrupts: Option<&forge::process_signals::Interrupts>,
) -> Result<Value> {
    let fd: OwnedFd = group
        .take_stdout()
        .context("receiver readiness pipe")?
        .into();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let pipe = forge::process_io::Descriptor::new(fd)?;
        let read = async {
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let count = pipe.read(&mut buffer).await?;
                ensure!(
                    count > 0,
                    "receiver exited before readiness; inspect serve.log"
                );
                bytes.extend_from_slice(&buffer[..count]);
                ensure!(
                    bytes.len() <= 4096,
                    "receiver readiness record exceeds 4096 bytes"
                );
                if bytes.contains(&b'\n') {
                    return forge::tool_schema::parse_document(std::str::from_utf8(&bytes)?);
                }
            }
        };
        let signal = interrupts.map(|i| i.register_reactor()).transpose()?;
        let cancelled = async {
            if let Some(signal) = signal {
                let mut bytes = [0; 128];
                signal.read(&mut bytes).await?;
            } else {
                std::future::pending::<()>().await;
            }
            Err::<Value, anyhow::Error>(forge::process_probe::Interrupted.into())
        };
        tokio::select! {
            result = tokio::time::timeout(timeout, read) => result.context("receiver readiness timed out")?,
            result = cancelled => result,
        }
    })
}

fn ensure_receiver(args: &StartupArgs, root: &Path) -> Result<Value> {
    let records = registry::load(root)?;
    let record = records
        .get(&args.identity)
        .context("mailbox identity is not registered")?;
    let _lock = startup_lock(root, &args.identity)?;
    if registry::probe(record)["status"] == "up" {
        return Ok(json!({"status":"already-running","port":record.port}));
    }
    // bind() in the child is the final ownership check. Never terminate or
    // replace an existing listener, including one with an invalid Agent Card.
    let log = owned_file(&root.join(&args.identity).join("serve.log"))?;
    ensure!(
        log.metadata()?.len() <= 1024 * 1024,
        "serve.log exceeds 1 MiB; rotate it before startup"
    );
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["mailbox", "--state-dir"])
        .arg(root)
        .args(["serve", "--name", &args.identity, "--lifetime-seconds"])
        .arg(args.lifetime_seconds.to_string())
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(log);
    let mut child = ProcessGroup::spawn(&mut command)?;
    let ready = ready_record(
        &mut child,
        Duration::from_secs(args.serve_timeout_seconds),
        None,
    )?;
    ensure!(
        ready["pid"].as_u64() == Some(u64::from(child.id()))
            && ready["name"].as_str() == Some(&args.identity)
            && ready["port"].as_u64() == Some(u64::from(record.port)),
        "receiver readiness identity mismatch"
    );
    ensure!(
        registry::probe(record)["status"] == "up",
        "receiver card probe failed after readiness"
    );
    // Intentional long-lived receiver. This short CLI exits immediately after
    // bounded flush/preview; init then reaps the orphan when its lifetime ends.
    let pid = child.into_child().id();
    Ok(json!({"status":"started","port":record.port,"pid":pid,
        "lifetime_seconds":args.lifetime_seconds}))
}

fn flush(root: &Path, identity: &str) -> Result<u8> {
    let mut command = Command::new(std::env::current_exe()?);
    command.args(["mailbox", "--state-dir"]).arg(root).args([
        "flush",
        "--as-name",
        identity,
        "--max-messages",
        "8",
    ]);
    let out = forge::process_probe::run(&mut command, Duration::from_secs(2), 64 * 1024)?;
    match out.status.code() {
        Some(code @ (0 | 3)) => Ok(code as u8),
        _ => anyhow::bail!(
            "sender flush exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

pub fn run(args: StartupArgs, root: &Path) -> Result<Value> {
    super::store::validate_identity(&args.identity)?;
    let root = root.canonicalize()?;
    let receiver = ensure_receiver(&args, &root)?;
    let (flush_status, flush_error) = match flush(&root, &args.identity) {
        Ok(code) => (Some(code), None),
        Err(error) => (
            None,
            Some(format!("{error:#}").chars().take(500).collect::<String>()),
        ),
    };
    let store = Store::required(&root, &args.identity, false)?;
    let preview = view::inbox(&store, &args.identity, true, 8, 140, 1200)?;
    Ok(json!({"identity":args.identity,"receiver":receiver,
        "flush_status":flush_status,"flush_error":flush_error,"preview":preview}))
}
