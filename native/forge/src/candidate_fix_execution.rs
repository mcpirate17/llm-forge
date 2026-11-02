//! Sequential confined filters; owned scratch outlives every process owner.
use super::*;
use crate::{
    candidate_execution_workspace::Scratch,
    cgroup::Cgroup,
    confined_execution::{self, Limits, Outcome, Resources},
    sandbox::{Policy, Sandbox},
};
use serde_json::Value;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
struct Lease(Arc<Mutex<Scratch>>);
impl Resources for Lease {
    fn arm(&mut self) {
        // Poisoning never enables deletion while a child may be live.
        self.0.lock().unwrap_or_else(|p| p.into_inner()).retain();
    }
    fn finish(self: Box<Self>, cleanup_safe: bool) -> Result<()> {
        let mut scratch = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("fix scratch lock poisoned"))?;
        if cleanup_safe {
            scratch.release();
        } else {
            scratch.retain();
            eprintln!(
                "forge: unsafe cleanup; fix scratch retained at {}",
                scratch.path().display()
            );
        }
        Ok(())
    }
}
struct Session {
    scratch: Arc<Mutex<Scratch>>,
    sandbox: Sandbox,
    deadline: Instant,
}
impl Session {
    fn new(prepared: &Prepared) -> Result<Self> {
        let scratch = Scratch::new(&prepared.grants.scratch_parent)?;
        let mut environment = prepared.grants.environment.clone();
        environment.insert(
            "XDG_CONFIG_HOME".into(),
            scratch
                .path()
                .join("config")
                .to_str()
                .context("scratch config path must be UTF-8")?
                .to_owned(),
        );
        environment.insert(
            "XDG_CONFIG_DIRS".into(),
            scratch
                .path()
                .join("config-dirs")
                .to_str()
                .context("scratch config path must be UTF-8")?
                .to_owned(),
        );
        let sandbox = Sandbox::prepare(Policy {
            read_only: prepared.grants.read_only.clone(),
            scratch: scratch.path().to_owned(),
            working_directory: prepared.source.root().to_owned(),
            environment,
        })?;
        Ok(Self {
            scratch: Arc::new(Mutex::new(scratch)),
            sandbox,
            deadline: Instant::now() + Duration::from_secs(prepared.grants.max_wall_seconds),
        })
    }
    fn retain(&self) -> Result<()> {
        let mut scratch = self
            .scratch
            .lock()
            .map_err(|_| anyhow::anyhow!("fix scratch lock poisoned"))?;
        scratch.retain();
        eprintln!(
            "forge: failed fix scratch retained at {}",
            scratch.path().display()
        );
        Ok(())
    }
    async fn filter(
        &self,
        prepared: &Prepared,
        interrupts: &Interrupts,
        argv: &[String],
        input: Vec<u8>,
    ) -> Result<(u8, confined_execution::Report)> {
        prepared.verify()?;
        self.scratch
            .lock()
            .map_err(|_| anyhow::anyhow!("fix scratch lock poisoned"))?
            .verify()?;
        let timeout = self
            .deadline
            .checked_duration_since(Instant::now())
            .context("fix batch wall budget exhausted")?;
        let limits = Limits {
            timeout,
            max_output_chars: 65536,
            cpu_seconds: Some(prepared.grants.max_cpu_seconds),
            address_space_bytes: Some(
                prepared
                    .grants
                    .max_address_space_mib
                    .checked_mul(1024 * 1024)
                    .context("address limit overflow")?,
            ),
        };
        let mut plan = self.sandbox.command(&prepared.tool_path)?;
        plan.command().args(argv);
        interrupts.configure_child(plan.command());
        let descriptor = interrupts.register_reactor()?;
        let pending = interrupts.signal()?;
        let group = Cgroup::create(&prepared.grants.cgroup_parent, prepared.grants.resources)?;
        let execution = confined_execution::start_filter_with_resources(
            plan,
            group,
            limits,
            Box::new(Lease(self.scratch.clone())),
            input,
            MAX_FILE,
        )?;
        let cancellation = execution.cancellation();
        if pending.is_some() {
            cancellation.cancel()?;
        }
        let mut wait = Box::pin(execution.wait());
        let mut signal = pending;
        let mut bytes = [0u8; std::mem::size_of::<libc::signalfd_siginfo>()];
        let report = tokio::select! {
            report = &mut wait => report?,
            received = descriptor.read(&mut bytes), if signal.is_none() => {
                let received = received.context("read fix interrupt");
                let wake = cancellation.cancel();
                let report = wait.await?;
                ensure!(received? == bytes.len(), "short fix interrupt read after cleanup");
                signal = Some(u32::from_ne_bytes(bytes[..4].try_into()?) as i32);
                wake.context("cancel fix execution")?;
                report
            }
        };
        let code = if let Some(signal) = signal.or(interrupts.signal()?) {
            signal_code(signal)
        } else if !report.cleanup_complete || !report.errors.is_empty() {
            2
        } else {
            match report.outcome {
                Outcome::Exited => report
                    .exit_code
                    .map(|c| u8::try_from(c).unwrap_or(2))
                    .or_else(|| report.signal.map(signal_code))
                    .unwrap_or(2),
                Outcome::TimedOut => 124,
                Outcome::Cancelled => 130,
                Outcome::Failed => 2,
            }
        };
        // Revalidation is necessary even after a successful child exit. A tool
        // cannot authorize publication of changed source/config/operator inputs.
        if code == 0 {
            prepared.verify()?;
        }
        Ok((code, report))
    }
}
pub(super) fn signal_code(signal: i32) -> u8 {
    u8::try_from(128 + signal).unwrap_or(2)
}
pub(super) async fn execute(
    prepared: &Prepared,
    interrupts: &Interrupts,
    keep_failed: bool,
) -> Result<(u8, Vec<Value>, Vec<String>)> {
    let session = Session::new(prepared)?;
    let result = filters(&session, prepared, interrupts).await;
    if keep_failed && result.as_ref().map_or(true, |(code, _, _)| *code != 0) {
        session.retain()?;
    }
    result
}
async fn filters(
    session: &Session,
    prepared: &Prepared,
    interrupts: &Interrupts,
) -> Result<(u8, Vec<Value>, Vec<String>)> {
    let mut stages = Vec::new();
    let (code, version) = session
        .filter(prepared, interrupts, &["--version".to_owned()], Vec::new())
        .await?;
    let supported = version.stdout.trim() == RUFF_VERSION && version.stderr.is_empty();
    stages.push(evidence("version", None, &version));
    if code != 0 {
        return Ok((code, stages, vec![]));
    }
    ensure!(
        supported,
        "unsupported Ruff version; expected exactly {RUFF_VERSION}"
    );
    let mut candidates: Vec<String> = prepared
        .files
        .iter()
        .map(|f| f.snapshot.content().to_owned())
        .collect();
    let original_bytes: usize = prepared
        .files
        .iter()
        .map(|file| file.snapshot.content().len())
        .sum();
    let mut candidate_bytes = original_bytes;
    for stage in ["check", "format"] {
        for (file, candidate) in prepared.files.iter().zip(candidates.iter_mut()) {
            let mut argv = vec![
                stage.to_owned(),
                "--no-cache".to_owned(),
                "--color".to_owned(),
                "never".to_owned(),
            ];
            if stage == "check" {
                argv.push("--fix".to_owned());
            }
            argv.extend([
                "--stdin-filename".to_owned(),
                prepared
                    .source
                    .root()
                    .join(file.snapshot.path())
                    .to_str()
                    .context("source pathname must be UTF-8")?
                    .to_owned(),
                "--".to_owned(),
                "-".to_owned(),
            ]);
            let (code, report) = session
                .filter(prepared, interrupts, &argv, candidate.as_bytes().to_owned())
                .await?;
            stages.push(evidence(stage, Some(file.snapshot.path()), &report));
            if code != 0 {
                return Ok((code, stages, vec![]));
            }
            validate_encoding(report.stdout.as_bytes())?;
            candidate_bytes = candidate_bytes
                .checked_sub(candidate.len())
                .and_then(|bytes| bytes.checked_add(report.stdout.len()))
                .context("candidate byte overflow")?;
            ensure!(
                original_bytes + candidate_bytes <= MAX_TOTAL,
                "fix batch original/candidate bytes exceed 32 MiB"
            );
            *candidate = report.stdout;
        }
    }
    Ok((0, stages, candidates))
}
fn evidence(stage: &str, path: Option<&Path>, report: &confined_execution::Report) -> Value {
    if !report.stderr.is_empty() {
        eprint!("{}", report.stderr);
    }
    json!({"stage":stage,"path":path,"outcome":report.outcome,"exit_code":report.exit_code,
        "signal":report.signal,"stdout_bytes":report.stdout_bytes,"stderr_bytes":report.stderr_bytes,
        "stdout_sha256":conductor_native::syntax::fingerprint(&report.stdout),
        "duration_ms":report.duration_ms,"cleanup_complete":report.cleanup_complete,"errors":report.errors})
}
