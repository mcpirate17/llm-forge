//! One operator-pinned, confined outbound MCP endpoint. Remote output is untrusted.
use super::{
    policy::{OutboundCapability, Policy},
    state,
};
use crate::{
    cgroup::Cgroup,
    confined_execution::{
        self, Cancellation, Completion, Duplex, DuplexEvent, DuplexLimits, Resources,
    },
    outbound_mcp_protocol::{OutboundProtocol, Reply, MAX_FRAME},
    sandbox,
};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::{oneshot, watch, Mutex as AsyncMutex};

struct Status {
    state: &'static str,
    launches: u64,
    calls: u64,
    error: Option<String>,
    scratch: Option<PathBuf>,
    cgroup: Option<PathBuf>,
    owner: Option<(Arc<Cancellation>, Completion)>,
    setup_cleanup_error: Option<String>,
}
struct Session {
    io: Duplex,
    protocol: OutboundProtocol,
}
pub struct Outbound {
    capability: OutboundCapability,
    bindings: Arc<crate::mutation_execution_grants::OutboundBindings>,
    state_root: PathBuf,
    parent: PathBuf,
    stopping: Arc<AtomicBool>,
    status: Arc<Mutex<Status>>,
    session: AsyncMutex<Option<Session>>,
    setup: watch::Sender<bool>,
}
struct Flight {
    status: Arc<Mutex<Status>>,
    done: bool,
}
impl Drop for Flight {
    fn drop(&mut self) {
        if !self.done {
            fail(&self.status, "outbound call interrupted");
        }
    }
}
fn fail(status: &Mutex<Status>, message: &str) {
    let mut status = status.lock().expect("outbound status lock");
    if status.state != "stopped" {
        status.state = "failed";
        status.error = Some(message.chars().take(512).collect());
    }
    if let Some((cancel, _)) = &status.owner {
        let _ = cancel.cancel();
    }
}
struct Scratch {
    scratch: PathBuf,
    record: PathBuf,
    cgroup: PathBuf,
    status: Arc<Mutex<Status>>,
    armed: bool,
}
impl Resources for Scratch {
    fn arm(&mut self) {
        self.armed = true;
    }
    fn finish(self: Box<Self>, safe: bool) -> Result<()> {
        if safe {
            std::fs::remove_dir_all(&self.scratch)?;
        }
        state::write_new(
            &self.record,
            &serde_json::to_vec(
                &json!({"schema_version":1,"cgroup_path":self.cgroup,"scratch_path":self.scratch,"cleanup_safe":safe}),
            )?,
        )?;
        Ok(())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.armed {
            if let Err(error) = std::fs::remove_dir_all(&self.scratch) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    self.status
                        .lock()
                        .expect("outbound status lock")
                        .setup_cleanup_error = Some(format!(
                        "outbound scratch preparation cleanup failed: {error}"
                    ));
                }
            }
        }
    }
}
struct SetupGuard(watch::Sender<bool>);
impl Drop for SetupGuard {
    fn drop(&mut self) {
        self.0.send_replace(false);
    }
}
struct Prepared {
    plan: sandbox::CommandPlan,
    group: Cgroup,
    resource: Scratch,
    protocol: OutboundProtocol,
}
fn prepare(
    capability: &OutboundCapability,
    bindings: &crate::mutation_execution_grants::OutboundBindings,
    root: &Path,
    parent: &Path,
    status: &Arc<Mutex<Status>>,
) -> Result<Prepared> {
    bindings.verify()?;
    let protocol = OutboundProtocol::new(
        &capability.remote_name,
        &capability.input_schema_json,
        capability.output_schema_json.as_deref(),
    )?;
    let scratch = root.join("outbound-scratch");
    std::fs::DirBuilder::new().mode(0o700).create(&scratch)?;
    let mut resource = Scratch {
        scratch: scratch.clone(),
        record: root.join("outbound-disposition.json"),
        cgroup: PathBuf::new(),
        status: status.clone(),
        armed: false,
    };
    let sandbox = sandbox::Sandbox::prepare(sandbox::Policy {
        read_only: capability.read_only.clone(),
        scratch: scratch.clone(),
        working_directory: scratch,
        environment: BTreeMap::new(),
    })?;
    let mut plan = sandbox.command(&capability.executable.path)?;
    plan.command().args(&capability.fixed_arguments);
    bindings.verify()?;
    let group = match Cgroup::create(parent, capability.resources) {
        Ok(group) => group,
        Err(error) => {
            status
                .lock()
                .expect("outbound status lock")
                .setup_cleanup_error =
                Some("outbound cgroup preparation cleanup could not be verified".into());
            return Err(error);
        }
    };
    resource.cgroup = group.report().path.clone();
    Ok(Prepared {
        plan,
        group,
        resource,
        protocol,
    })
}
async fn start_prepared(
    prepared: Prepared,
    capability: &OutboundCapability,
    shared: &Mutex<Status>,
    stopping: &AtomicBool,
) -> Result<Session> {
    let Prepared {
        plan,
        mut group,
        resource,
        protocol,
    } = prepared;
    let unavailable = {
        let status = shared.lock().expect("outbound status lock");
        status.state != "not_started" || stopping.load(Ordering::Acquire)
    };
    if unavailable {
        if let Err(error) = group.finish(Duration::from_secs(3)).await {
            let message = format!("outbound preparation cleanup failed: {error:#}");
            shared
                .lock()
                .expect("outbound status lock")
                .setup_cleanup_error = Some(message.clone());
            bail!(message);
        }
        bail!("outbound endpoint unavailable");
    }
    let scratch = resource.scratch.clone();
    let cgroup = resource.cgroup.clone();
    let io = match confined_execution::start_duplex_with_resources(
        plan,
        group,
        confined_execution::Limits {
            timeout: Duration::from_secs(capability.max_wall_seconds),
            max_output_chars: 0,
            cpu_seconds: None,
            address_space_bytes: None,
        },
        Box::new(resource),
        DuplexLimits {
            message_bytes: MAX_FRAME,
            input_bytes: 16 * 1024 * 1024,
            stdout_bytes: 16 * 1024 * 1024,
            stderr_bytes: 1024 * 1024,
            output_chunks: 64,
        },
    ) {
        Ok(io) => io,
        Err(error) => {
            shared
                .lock()
                .expect("outbound status lock")
                .setup_cleanup_error = Some("outbound start cleanup could not be verified".into());
            return Err(error);
        }
    };
    let mut status = shared.lock().expect("outbound status lock");
    status.launches += 1;
    status.scratch = Some(scratch);
    status.cgroup = Some(cgroup);
    status.owner = Some((io.cancellation(), io.completion()));
    if status.state != "not_started" || stopping.load(Ordering::Acquire) {
        io.cancellation().cancel()?;
        bail!("outbound endpoint stopped during launch");
    }
    Ok(Session { io, protocol })
}
impl Outbound {
    pub fn new(
        policy: &Policy,
        root: &Path,
        parent: PathBuf,
        stopping: Arc<AtomicBool>,
    ) -> Result<Option<Self>> {
        let Some(capability) = policy.outbound_mcp.clone() else {
            return Ok(None);
        };
        let bindings = capability.bind(policy, root)?;
        Ok(Some(Self {
            capability,
            bindings: Arc::new(bindings),
            state_root: root.into(),
            parent,
            stopping,
            status: Arc::new(Mutex::new(Status {
                state: "not_started",
                launches: 0,
                calls: 0,
                error: None,
                scratch: None,
                cgroup: None,
                owner: None,
                setup_cleanup_error: None,
            })),
            session: AsyncMutex::new(None),
            setup: watch::channel(false).0,
        }))
    }
    pub fn name(&self) -> &str {
        &self.capability.public_name
    }
    pub fn status(&self) -> Value {
        let s = self.status.lock().expect("outbound status lock");
        json!({"state":s.state,"launches":s.launches,"calls":s.calls,"error":s.error,"scratch_path":s.scratch,"cgroup_path":s.cgroup})
    }
    async fn verify(&self) -> Result<()> {
        let bindings = self.bindings.clone();
        tokio::task::spawn_blocking(move || bindings.verify())
            .await
            .context("outbound binding worker failed")?
    }
    async fn launch(&self) -> Result<Session> {
        let (sender, receiver) = oneshot::channel();
        {
            let status = self.status.lock().expect("outbound status lock");
            ensure!(
                status.state == "not_started" && !self.stopping.load(Ordering::Acquire),
                "outbound endpoint unavailable"
            );
            self.setup.send_replace(true);
        }
        let capability = self.capability.clone();
        let bindings = self.bindings.clone();
        let root = self.state_root.clone();
        let parent = self.parent.clone();
        let status = self.status.clone();
        let stopping = self.stopping.clone();
        let setup = self.setup.clone();
        tokio::spawn(async move {
            let _guard = SetupGuard(setup);
            let config = capability.clone();
            let prepare_status = status.clone();
            let prepared = match tokio::task::spawn_blocking(move || {
                prepare(&config, &bindings, &root, &parent, &prepare_status)
            })
            .await
            {
                Ok(result) => result,
                Err(error) => {
                    status
                        .lock()
                        .expect("outbound status lock")
                        .setup_cleanup_error = Some(
                        "outbound preparation worker failed; cleanup could not be verified".into(),
                    );
                    Err(anyhow::anyhow!(error).context("outbound preparation worker failed"))
                }
            };
            let result = match prepared {
                Ok(prepared) => start_prepared(prepared, &capability, &status, &stopping).await,
                Err(error) => Err(error),
            };
            // A departed caller drops the returned duplex here, cancelling its owner.
            let _ = sender.send(result);
        });
        receiver.await.context("outbound setup task stopped")?
    }
    pub async fn call(&self, arguments: &str) -> Result<Value> {
        let mut slot = self
            .session
            .try_lock()
            .map_err(|_| anyhow::anyhow!("outbound endpoint already busy"))?;
        {
            let status = self.status.lock().expect("outbound status lock");
            ensure!(
                matches!(status.state, "not_started" | "ready")
                    && !self.stopping.load(Ordering::Acquire),
                "outbound endpoint unavailable"
            );
        }
        let mut flight = Flight {
            status: self.status.clone(),
            done: false,
        };
        let result = async {
            if slot.is_none() {
                let mut session = self.launch().await?;
                session.initialize().await?;
                *slot = Some(session);
                {
                    let mut status = self.status.lock().expect("outbound status lock");
                    ensure!(
                        !self.stopping.load(Ordering::Acquire),
                        "harness is stopping"
                    );
                    status.state = "ready";
                }
            }
            self.verify().await?;
            let session = slot.as_mut().context("outbound session unavailable")?;
            session.idle()?;
            let request = session.protocol.call_request(arguments)?;
            session.io.send(request).await?;
            let response = session.response().await?;
            match session.protocol.accept_call(&response)? {
                Reply::Result(result) => {
                    self.verify().await?;
                    session.idle()?;
                    ensure!(
                        session.io.completion().finished().is_none(),
                        "outbound owner stopped"
                    );
                    let status = self.status.lock().expect("outbound status lock");
                    ensure!(
                        status.state == "ready" && !self.stopping.load(Ordering::Acquire),
                        "outbound endpoint unavailable"
                    );
                    Ok(result)
                }
                Reply::RpcError { code, message, .. } => {
                    bail!("outbound RPC error {code}: {message}")
                }
            }
        }
        .await;
        match result {
            Ok(value) => {
                let mut status = self.status.lock().expect("outbound status lock");
                ensure!(
                    status.state == "ready" && !self.stopping.load(Ordering::Acquire),
                    "outbound endpoint unavailable"
                );
                status.calls += 1;
                flight.done = true;
                Ok(value)
            }
            Err(error) => {
                fail(&self.status, &format!("{error:#}"));
                slot.take();
                flight.done = true;
                Err(error)
            }
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        let mut failures = Vec::new();
        let mut setup = self.setup.subscribe();
        {
            let mut status = self.status.lock().expect("outbound status lock");
            status.state = "stopped";
            if let Some((cancel, _)) = &status.owner {
                if let Err(error) = cancel.cancel() {
                    failures.push(format!("outbound cancel: {error:#}"));
                }
            }
        }
        while *setup.borrow_and_update() {
            if let Err(error) = setup.changed().await {
                failures.push(format!("outbound setup completion unavailable: {error}"));
                break;
            }
        }
        let owner = self
            .status
            .lock()
            .expect("outbound status lock")
            .owner
            .clone();
        if let Some((cancel, mut completion)) = owner {
            if let Err(error) = cancel.cancel() {
                failures.push(format!("outbound cancel: {error:#}"));
            }
            match completion.wait().await {
                Ok(terminal) if terminal.cleanup_complete => (),
                Ok(_) => failures.push("outbound cleanup incomplete".into()),
                Err(error) => failures.push(format!("outbound completion unavailable: {error:#}")),
            }
        }
        if let Ok(mut session) = self.session.try_lock() {
            session.take();
        }
        if let Some(error) = self
            .status
            .lock()
            .expect("outbound status lock")
            .setup_cleanup_error
            .clone()
        {
            failures.push(error);
        }
        ensure!(
            failures.is_empty(),
            "outbound shutdown failed: {}",
            failures.join("; ")
        );
        Ok(())
    }
}
impl Session {
    async fn initialize(&mut self) -> Result<()> {
        self.idle()?;
        let request = self.protocol.initialize_request()?;
        self.io.send(request).await?;
        let frame = self.response().await?;
        let notification = self.protocol.accept_initialize(&frame)?;
        self.idle()?;
        self.io.send(notification).await?;
        loop {
            self.idle()?;
            let request = self.protocol.list_request()?;
            self.io.send(request).await?;
            let frame = self.response().await?;
            if self.protocol.accept_list(&frame)? {
                break;
            }
        }
        Ok(())
    }
    fn idle(&mut self) -> Result<()> {
        while let Some(event) = self.io.try_recv_event()? {
            match event {
                DuplexEvent::Stderr(_) => (),
                DuplexEvent::StderrEof => (),
                _ => bail!("unsolicited outbound stdout or EOF"),
            }
        }
        Ok(())
    }
    async fn response(&mut self) -> Result<Vec<u8>> {
        let mut frame = Vec::new();
        loop {
            match self.io.recv().await?.context("outbound stream closed")? {
                DuplexEvent::Stdout(bytes) => {
                    ensure!(
                        frame.len() + bytes.len() <= MAX_FRAME + 1,
                        "outbound frame exceeds limit"
                    );
                    if let Some(end) = bytes.iter().position(|b| *b == b'\n') {
                        ensure!(end + 1 == bytes.len(), "unsolicited extra outbound output");
                        frame.extend_from_slice(&bytes[..end]);
                        return Ok(frame);
                    }
                    frame.extend_from_slice(&bytes);
                }
                DuplexEvent::Stderr(_) => (),
                DuplexEvent::StderrEof => (),
                DuplexEvent::StdoutEof => bail!("outbound stdout closed"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    fn status() -> Arc<Mutex<Status>> {
        Arc::new(Mutex::new(Status {
            state: "not_started",
            launches: 0,
            calls: 0,
            error: None,
            scratch: None,
            cgroup: None,
            owner: None,
            setup_cleanup_error: None,
        }))
    }
    fn scratch(path: PathBuf, status: Arc<Mutex<Status>>) -> Scratch {
        Scratch {
            scratch: path,
            record: PathBuf::new(),
            cgroup: PathBuf::new(),
            status,
            armed: false,
        }
    }
    #[test]
    fn abandoned_scratch_reports_removal_failure_but_accepts_absence() {
        let temp = tempfile::tempdir().unwrap();
        let status = status();
        drop(scratch(temp.path().join("missing"), status.clone()));
        assert!(status.lock().unwrap().setup_cleanup_error.is_none());
        let regular = temp.path().join("file");
        fs::write(&regular, b"retained").unwrap();
        drop(scratch(regular.clone(), status.clone()));
        let error = status.lock().unwrap().setup_cleanup_error.clone().unwrap();
        assert!(error.contains("scratch preparation cleanup failed"));
        assert_eq!(fs::read(regular).unwrap(), b"retained");
    }
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires explicit delegated FORGE_TEST_CGROUP_PARENT and Landlock ABI 6"]
    async fn stopped_prepared_endpoint_cleans_group_and_scratch_without_launch() {
        let temp = tempfile::tempdir().unwrap();
        let scratch_path = temp.path().join("scratch");
        fs::create_dir(&scratch_path).unwrap();
        fs::set_permissions(&scratch_path, fs::Permissions::from_mode(0o700)).unwrap();
        let sandbox = sandbox::Sandbox::prepare(sandbox::Policy {
            read_only: vec![PathBuf::from("/usr")],
            scratch: scratch_path.clone(),
            working_directory: scratch_path.clone(),
            environment: BTreeMap::new(),
        })
        .unwrap();
        let plan = sandbox.command(Path::new("/usr/bin/true")).unwrap();
        let limits = crate::cgroup::Limits {
            cpu_millis: 1000,
            memory_mib: 128,
            processes: 16,
        };
        let parent = PathBuf::from(
            std::env::var_os("FORGE_TEST_CGROUP_PARENT")
                .expect("explicit cgroup delegation required"),
        );
        let group = Cgroup::create(&parent, limits).unwrap();
        let group_path = group.report().path.clone();
        let status = status();
        status.lock().unwrap().state = "stopped";
        let schema =
            r#"{"type":"object","properties":{},"required":[],"additionalProperties":false}"#;
        let protocol = OutboundProtocol::new("remote", schema, None).unwrap();
        let capability: OutboundCapability = serde_json::from_value(json!({
            "public_name":"outbound_test", "remote_name":"remote",
            "executable":{"path":"/usr/bin/true", "sha256":"0".repeat(64)},
            "fixed_arguments":[], "pinned_inputs":[], "read_only":["/usr"],
            "input_schema_json":schema, "resources":limits, "max_wall_seconds":1
        }))
        .unwrap();
        let resource = scratch(scratch_path.clone(), status.clone());
        let prepared = Prepared {
            plan,
            group,
            resource,
            protocol,
        };
        assert!(
            start_prepared(prepared, &capability, &status, &AtomicBool::new(true))
                .await
                .is_err()
        );
        assert_eq!(
            fs::symlink_metadata(group_path).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        assert_eq!(
            fs::symlink_metadata(scratch_path).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
        let status = status.lock().unwrap();
        assert_eq!(status.launches, 0);
        assert!(status.owner.is_none() && status.setup_cleanup_error.is_none());
    }
}
