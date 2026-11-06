use super::{policy::Policy, state, transport, Harness};
use anyhow::{ensure, Context, Result};
use clap::{Args, Subcommand};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Args)]
pub struct HarnessArgs {
    #[command(subcommand)]
    action: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Internal fixed read-only analysis worker; only safe under caller confinement.
    #[command(hide = true)]
    TestReportWorker(crate::test_report_worker::WorkerArgs),
    /// Native MCP stdio adapter for an already authorized persistent service.
    Stdio {
        #[arg(long)]
        socket: PathBuf,
        /// Require this canonical source root before exposing the tool catalog.
        #[arg(long)]
        source_root: Option<PathBuf>,
    },
    /// Start an owned, bounded native service using explicit operator policy.
    Start {
        #[arg(long)]
        policy: PathBuf,
        #[arg(long)]
        state_parent: Option<PathBuf>,
    },
    /// Read-only policy/schema/kernel/service-manager admission checks.
    Preflight {
        #[arg(long)]
        policy: PathBuf,
        #[arg(long)]
        state_parent: PathBuf,
    },
    /// Send one protocol JSON request from a file (no implicit shell expansion).
    Call {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        request: PathBuf,
    },
    /// Discover available tool schemas.
    Catalog {
        #[arg(long)]
        socket: PathBuf,
    },
    /// Fetch a GBNF grammar for the exact operator-enabled catalog.
    Grammar {
        #[arg(long)]
        socket: PathBuf,
    },
    /// Gracefully stop the owned service and all terminal descendants.
    Stop {
        #[arg(long)]
        socket: PathBuf,
    },
    #[command(hide = true)]
    Serve {
        #[arg(long)]
        state: PathBuf,
    },
}

pub fn run(args: HarnessArgs) -> Result<u8> {
    let action = match args.action {
        Action::TestReportWorker(worker) => return crate::test_report_worker::run(worker),
        action => action,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(8)
        .build()?;
    let result = match action {
        Action::TestReportWorker(_) => unreachable!("worker dispatched before runtime"),
        Action::Stdio {
            socket,
            source_root,
        } => runtime
            .block_on(super::mcp::stdio(socket, source_root))
            .map(|_| 0),
        Action::Start {
            policy,
            state_parent,
        } => runtime.block_on(start(&policy, state_parent)),
        Action::Serve { state } => {
            let result = bootstrap(&state).and_then(|(harness, unit)| {
                runtime.block_on(transport::serve(harness, &state, &unit))
            });
            if let Err(error) = &result {
                if let Err(write) = state::write_new(
                    &state.join("failure.json"),
                    &serde_json::to_vec(&json!({"error":format!("{error:#}")}))?,
                ) {
                    eprintln!("forge: could not persist service failure: {write:#}");
                }
            }
            result.map(|_| 0)
        }
        Action::Preflight {
            policy,
            state_parent,
        } => preflight(&policy, &state_parent),
        Action::Call { socket, request } => {
            let bytes = state::read(&request, transport::MAX_FRAME)?;
            // Preserve duplicate keys in call arguments. Trim only the outer line.
            let input = std::str::from_utf8(&bytes)?.trim();
            runtime.block_on(client(&socket, input.as_bytes()))
        }
        Action::Catalog { socket } => runtime.block_on(client(&socket, br#"{"method":"catalog"}"#)),
        Action::Grammar { socket } => runtime.block_on(client(&socket, br#"{"method":"grammar"}"#)),
        Action::Stop { socket } => runtime.block_on(client(&socket, br#"{"method":"shutdown"}"#)),
    };
    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}

fn load(path: &Path, state: &Path) -> Result<Policy> {
    let policy: Policy = serde_json::from_slice(&state::read(path, 1024 * 1024)?)?;
    policy.validate(state)
}

fn bootstrap(root: &Path) -> Result<(Harness, String)> {
    let state = state::State::open(root)?;
    let policy = load(&root.join("policy.json"), root)?;
    let unit = std::env::var("FORGE_SERVICE_UNIT")
        .context("serve requires an owned service; use harness start")?;
    let parent = crate::managed_service::prepare_children(&unit)?;
    crate::managed_service::verify_limits(&parent, policy.aggregate)?;
    Ok((Harness::new(policy, state, parent, &unit)?, unit))
}

async fn start(path: &Path, parent: Option<PathBuf>) -> Result<u8> {
    let parent = parent.map_or_else(
        || Ok::<_, anyhow::Error>(conductor_native::state_paths::state_root()?.join("h")),
        Ok,
    )?;
    let root = state::State::create(&parent)?;
    let policy = load(path, &root)?;
    state::write_new(
        &root.join("policy.json"),
        &serde_json::to_vec_pretty(&policy)?,
    )?;
    let events = directory_events(&root)?;
    let service = crate::managed_service::Service::launch(
        &std::env::current_exe()?,
        &[
            "harness".into(),
            "serve".into(),
            "--state".into(),
            root.as_os_str().into(),
        ],
        policy.aggregate,
        policy.lifetime_seconds,
        &BTreeMap::new(),
    )?;
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut buffer = [0u8; 4096];
        loop {
            if root.join("failure.json").exists() {
                anyhow::bail!(
                    "service failed: {}",
                    String::from_utf8_lossy(&state::read(&root.join("failure.json"), 65536)?)
                );
            }
            if root.join("ready.json").exists() {
                let response =
                    transport::request(&root.join("socket"), br#"{"method":"status"}"#).await?;
                ensure!(response["ok"] == true, "service readiness rejected");
                break;
            }
            ensure!(
                events.read(&mut buffer).await? > 0,
                "readiness event stream closed"
            );
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("service readiness deadline exceeded")??;
    let record = service.detach();
    println!(
        "{}",
        serde_json::to_string(
            &json!({"state":root,"socket":root.join("socket"),"service":record})
        )?
    );
    Ok(0)
}

fn preflight(path: &Path, parent: &Path) -> Result<u8> {
    let policy = load(path, parent)?;
    let registry = crate::tool_schema::ToolRegistry::from_json(&serde_json::to_string(
        &super::catalog::catalog(&policy),
    )?)?;
    crate::systemd::Manager::connect(254)?;
    let abi = crate::sandbox::kernel_abi()?;
    ensure!(abi >= 6, "Landlock ABI 6 required");
    crate::task_resources::ResourceRequest {
        cpus: Some(policy.aggregate.cpu_millis as f64 / 1000.0),
        memory_mib: Some(policy.aggregate.memory_mib),
        ..Default::default()
    }
    .admit()?;
    println!(
        "{}",
        json!({"policy_valid":true,"landlock_abi":abi,"grammar_bytes":registry.grammar().len(),
        "execution_probe":false,"note":"launch-time cgroup, seccomp and descriptor verification remain mandatory"})
    );
    Ok(0)
}

async fn client(socket: &Path, input: &[u8]) -> Result<u8> {
    let value=tokio::time::timeout(Duration::from_secs(40),transport::request(socket,input)).await.context("client deadline exceeded; execution may have started; reuse the same session id/sequence")??;
    println!("{}", serde_json::to_string(&value)?);
    Ok(if value["ok"] == true { 0 } else { 2 })
}

fn directory_events(path: &Path) -> Result<crate::process_io::Descriptor> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let raw = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
    ensure!(raw >= 0, "inotify: {}", std::io::Error::last_os_error());
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
    ensure!(
        unsafe {
            libc::inotify_add_watch(
                fd.as_raw_fd(),
                path.as_ptr(),
                libc::IN_CLOSE_WRITE | libc::IN_MOVED_TO | libc::IN_CREATE,
            )
        } > 0,
        "watch service readiness: {}",
        std::io::Error::last_os_error()
    );
    Ok(crate::process_io::Descriptor::new(fd)?)
}
