//! Operator-enabled isolated Python test report; requests cannot choose paths or commands.
use super::policy::{Policy, TestReportCapability};
use crate::{
    cgroup::Cgroup,
    confined_execution::{self, Outcome, Resources},
    sandbox,
};
use anyhow::{bail, ensure, Context, Result};
use serde::Deserialize;
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
use tokio::sync::{oneshot, watch, Semaphore};

type Bindings = crate::mutation_execution_grants::OutboundBindings;

fn bindings(capability: &TestReportCapability, policy: &Policy, state: &Path) -> Result<Bindings> {
    let git = policy.source_root.join(".git");
    ensure!(
        std::fs::symlink_metadata(&git)?.is_dir() && git.canonicalize()? == git,
        "test report requires an in-root Git directory"
    );
    Bindings::capture_report(
        &capability.executable.path,
        &capability.executable.sha256,
        &capability.read_only,
        &policy.source_root,
        state,
        &policy.read_only,
    )
}
pub(super) fn validate(
    capability: &TestReportCapability,
    policy: &Policy,
    state: &Path,
) -> Result<()> {
    bindings(capability, policy, state).map(|_| ())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    max_output_bytes: usize,
}

struct Scratch {
    path: PathBuf,
    armed: bool,
    status: Arc<Mutex<Status>>,
}
impl Resources for Scratch {
    fn arm(&mut self) {
        self.armed = true;
    }
    fn finish(self: Box<Self>, safe: bool) -> Result<()> {
        if safe {
            std::fs::remove_dir_all(&self.path)?;
        }
        Ok(())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if !self.armed {
            if let Err(error) = std::fs::remove_dir_all(&self.path) {
                eprintln!(
                    "forge: retained report scratch {}: {error}",
                    self.path.display()
                );
                self.status.lock().unwrap().fatal = true;
            }
        }
    }
}
struct Prepared {
    plan: sandbox::CommandPlan,
    group: Cgroup,
    scratch: Scratch,
}
fn prepare(
    capability: &TestReportCapability,
    bound: &Bindings,
    source: &Path,
    state: &Path,
    parent: &Path,
    max: usize,
    status: Arc<Mutex<Status>>,
) -> Result<Prepared> {
    bound.verify()?;
    let scratch = state.join(crate::resource_id("test-report")?);
    std::fs::DirBuilder::new().mode(0o700).create(&scratch)?;
    status.lock().unwrap().scratch = Some(scratch.clone());
    let resource = Scratch {
        path: scratch.clone(),
        armed: false,
        status: status.clone(),
    };
    let mut reads = capability.read_only.clone();
    reads.push(source.to_owned());
    let sandbox = sandbox::Sandbox::prepare(sandbox::Policy {
        read_only: reads,
        scratch: scratch.clone(),
        working_directory: scratch.clone(),
        environment: BTreeMap::new(),
    })?;
    let config = scratch.join("git-empty-config");
    std::fs::DirBuilder::new().mode(0o700).create(&config)?;
    let mut plan = sandbox.command(&capability.executable.path)?;
    plan.command()
        .args(["harness", "test-report-worker", "--root"])
        .arg(source)
        .arg("--config-root")
        .arg(&config)
        .arg("--max-bytes")
        .arg(max.to_string());
    bound.verify()?;
    let group = Cgroup::create(parent, capability.resources)?;
    status.lock().unwrap().cgroup = Some(group.report().path.clone());
    Ok(Prepared {
        plan,
        group,
        scratch: resource,
    })
}

#[derive(Default)]
struct Status {
    launches: u64,
    calls: u64,
    error: Option<String>,
    fatal: bool,
    scratch: Option<PathBuf>,
    cgroup: Option<PathBuf>,
}
pub(super) struct Reports {
    capability: TestReportCapability,
    bindings: Arc<Bindings>,
    source: PathBuf,
    state: PathBuf,
    parent: PathBuf,
    stopping: Arc<AtomicBool>,
    slot: Arc<Semaphore>,
    shutdown: watch::Sender<bool>,
    status: Arc<Mutex<Status>>,
}
struct Flight {
    status: Arc<Mutex<Status>>,
    done: bool,
}
impl Drop for Flight {
    fn drop(&mut self) {
        if !self.done {
            self.status.lock().unwrap().fatal = true;
        }
    }
}
struct CancelOnDrop(watch::Sender<bool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.send_replace(true);
    }
}

impl Reports {
    pub(super) fn new(
        policy: &Policy,
        state: &Path,
        parent: PathBuf,
        stopping: Arc<AtomicBool>,
    ) -> Result<Option<Self>> {
        let Some(capability) = &policy.test_report else {
            return Ok(None);
        };
        let bound = bindings(capability, policy, state)?;
        let (shutdown, _) = watch::channel(false);
        Ok(Some(Self {
            capability: capability.clone(),
            bindings: Arc::new(bound),
            source: policy.source_root.clone(),
            state: state.to_owned(),
            parent,
            stopping,
            slot: Arc::new(Semaphore::new(1)),
            shutdown,
            status: Arc::new(Mutex::new(Status::default())),
        }))
    }
    pub(super) fn status(&self) -> Value {
        let s = self.status.lock().expect("test report status lock");
        json!({"launches":s.launches,"calls":s.calls,"fatal":s.fatal,"error":s.error,
            "scratch_path":s.scratch,"cgroup_path":s.cgroup})
    }
    pub(super) async fn call(
        &self,
        args: &str,
        mut cancel: watch::Receiver<bool>,
    ) -> Result<Value> {
        let args: Args = serde_json::from_str(args)?;
        ensure!(
            (512..=self.capability.max_output_bytes).contains(&args.max_output_bytes),
            "invalid report output budget"
        );
        ensure!(
            !self.stopping.load(Ordering::Acquire) && !*self.shutdown.borrow(),
            "report service stopping"
        );
        ensure!(
            !self.status.lock().expect("test report status lock").fatal,
            "report cleanup unverified"
        );
        let permit = tokio::select! {
            result = self.slot.clone().acquire_owned() => result?,
            _ = cancel.changed() => bail!("report cancelled"),
        };
        ensure!(
            !self.status.lock().expect("test report status lock").fatal,
            "report cleanup unverified"
        );
        ensure!(
            !self.stopping.load(Ordering::Acquire) && !*self.shutdown.borrow() && !*cancel.borrow(),
            "report cancelled or stopping"
        );
        let (drop_sender, drop_receiver) = watch::channel(false);
        let _drop_guard = CancelOnDrop(drop_sender);
        let (sender, receiver) = oneshot::channel();
        let capability = self.capability.clone();
        let bound = self.bindings.clone();
        let source = self.source.clone();
        let state = self.state.clone();
        let parent = self.parent.clone();
        let status = self.status.clone();
        let shutdown = self.shutdown.subscribe();
        tokio::spawn(async move {
            let _permit = permit;
            let mut flight = Flight {
                status: status.clone(),
                done: false,
            };
            let job = Job {
                capability,
                bound,
                source,
                state,
                parent,
                max: args.max_output_bytes,
                status,
            };
            let result = execute(job, cancel, drop_receiver, shutdown).await;
            if let Err(error) = &result {
                flight.status.lock().unwrap().error =
                    Some(format!("{error:#}").chars().take(512).collect());
            }
            flight.done = true;
            let _ = sender.send(result);
        });
        receiver.await.context("report owner stopped")?
    }
    pub(super) async fn shutdown(&self) -> Result<()> {
        self.shutdown.send_replace(true);
        let _permit = self.slot.clone().acquire_owned().await?;
        ensure!(
            !self.status.lock().expect("test report status lock").fatal,
            "report cleanup unverified"
        );
        Ok(())
    }
}
async fn cancelled(
    cancel: &mut watch::Receiver<bool>,
    dropped: &mut watch::Receiver<bool>,
    shutdown: &mut watch::Receiver<bool>,
) {
    if *cancel.borrow() || *dropped.borrow() || *shutdown.borrow() {
        return;
    }
    tokio::select! { _ = cancel.changed() => (), _ = dropped.changed() => (), _ = shutdown.changed() => () }
}
struct Job {
    capability: TestReportCapability,
    bound: Arc<Bindings>,
    source: PathBuf,
    state: PathBuf,
    parent: PathBuf,
    max: usize,
    status: Arc<Mutex<Status>>,
}
async fn execute(
    job: Job,
    mut cancel: watch::Receiver<bool>,
    mut dropped: watch::Receiver<bool>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<Value> {
    let Job {
        capability,
        bound,
        source,
        state,
        parent,
        max,
        status,
    } = job;
    let config = capability.clone();
    let held = bound.clone();
    let root = source.clone();
    let prep_status = status.clone();
    let prepared = match tokio::task::spawn_blocking(move || {
        prepare(&config, &held, &root, &state, &parent, max, prep_status)
    })
    .await
    {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            status.lock().unwrap().fatal = true;
            return Err(error).context("report preparation cleanup unverified");
        }
        Err(error) => {
            status.lock().unwrap().fatal = true;
            return Err(error).context("report preparation worker failed; cleanup unverified");
        }
    };
    let Prepared {
        plan,
        mut group,
        mut scratch,
    } = prepared;
    if *cancel.borrow() || *dropped.borrow() || *shutdown.borrow() {
        if let Err(error) = group.finish(Duration::from_secs(3)).await {
            status.lock().unwrap().fatal = true;
            scratch.armed = true;
            return Err(error).context("report preparation cleanup failed");
        }
        bail!("report cancelled before launch");
    }
    let watches = Watches {
        cancel: &mut cancel,
        dropped: &mut dropped,
        shutdown: &mut shutdown,
    };
    let report = run_owned(plan, group, scratch, &capability, &status, watches).await?;
    let verify = bound.clone();
    tokio::task::spawn_blocking(move || verify.verify())
        .await
        .context("report verification worker failed")??;
    ensure!(
        !*cancel.borrow() && !*dropped.borrow() && !*shutdown.borrow(),
        "report cancelled"
    );
    let value = envelope(&report.stdout, &source, max)?;
    status.lock().unwrap().calls += 1;
    Ok(value)
}

struct Watches<'a> {
    cancel: &'a mut watch::Receiver<bool>,
    dropped: &'a mut watch::Receiver<bool>,
    shutdown: &'a mut watch::Receiver<bool>,
}
async fn run_owned(
    plan: sandbox::CommandPlan,
    group: Cgroup,
    scratch: Scratch,
    capability: &TestReportCapability,
    status: &Mutex<Status>,
    watches: Watches<'_>,
) -> Result<confined_execution::Report> {
    let execution = match confined_execution::start_filter_with_resources(
        plan,
        group,
        confined_execution::Limits {
            timeout: Duration::from_secs(capability.max_wall_seconds),
            max_output_chars: 0,
            cpu_seconds: None,
            address_space_bytes: None,
        },
        Box::new(scratch),
        Vec::new(),
        1024 * 1024,
    ) {
        Ok(value) => value,
        Err(error) => {
            status.lock().unwrap().fatal = true;
            return Err(error).context("report launch cleanup unverified");
        }
    };
    status.lock().unwrap().launches += 1;
    let owner_cancel = execution.cancellation();
    let wait = execution.wait();
    tokio::pin!(wait);
    let report = tokio::select! {
        result = &mut wait => result,
        _ = cancelled(watches.cancel, watches.dropped, watches.shutdown) => {
            let wake = owner_cancel.cancel(); let result = wait.await;
            if let Err(error) = wake { status.lock().unwrap().fatal = true; return Err(error).context("report cancellation failed"); }
            result
        }
    };
    let report = match report {
        Ok(value) => value,
        Err(error) => {
            status.lock().unwrap().fatal = true;
            return Err(error).context("report owner completion unavailable");
        }
    };
    if !report.cleanup_complete {
        status.lock().unwrap().fatal = true;
        bail!("report cleanup incomplete");
    }
    ensure!(
        report.outcome == Outcome::Exited && report.exit_code == Some(0),
        "{}",
        worker_failure(report.outcome, report.exit_code, &report.stderr)
    );
    Ok(report)
}

fn worker_failure(outcome: Outcome, exit_code: Option<i32>, stderr: &str) -> String {
    let mut end = stderr.len().min(2048);
    while !stderr.is_char_boundary(end) {
        end -= 1;
    }
    format!("report worker failed: outcome={outcome:?}, exit_code={exit_code:?}, worker stderr (untrusted, at most 2048 bytes): {}", &stderr[..end])
}

fn envelope(text: &str, source: &Path, max: usize) -> Result<Value> {
    let value: Value = serde_json::from_str(text).context("invalid report worker JSON")?;
    ensure!(
        value["schema_version"] == 1
            && value["authority"] == "native-static-analysis"
            && value["advisory"] == true
            && value["root"].as_str() == source.to_str()
            && value["report"].is_object(),
        "invalid report envelope"
    );
    ensure!(
        serde_json::to_vec(&value)?.len() <= max,
        "report exceeds requested output budget"
    );
    Ok(value)
}
#[cfg(test)]
#[path = "harness_test_report_tests.rs"]
mod tests;
