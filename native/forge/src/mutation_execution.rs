//! Shared command ownership for confined mutation engines.
use crate::{
    cgroup::{Cgroup, Limits as GroupLimits},
    confined_execution::{self, Outcome, Resources},
    mutation_campaign::{Step, Stop},
    mutation_process::CommandResult,
    mutation_run::EngineRun,
    sandbox::Sandbox,
};
use serde_json::{json, Value};
use std::{
    cell::Cell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) enum Capture {
    Strict,
    Metadata,
    Complete,
}
/// Borrow the owning runtime's sandbox and cleanup state for one command.
pub(crate) struct CommandSession<'a> {
    pub sandbox: &'a Sandbox,
    pub reactor: &'a tokio::runtime::Runtime,
    pub cgroup_parent: &'a Path,
    pub limits: GroupLimits,
    pub cleanup_complete: &'a Cell<bool>,
    pub signal: &'a Cell<Option<i32>>,
    pub programs: Option<&'a BTreeMap<String, PathBuf>>,
    pub scratch: &'a Path,
    pub max_wall_seconds: u64,
    pub cargo_value: bool,
}
struct NoResources;
impl Resources for NoResources {
    fn arm(&mut self) {}
    fn finish(self: Box<Self>, _cleanup_safe: bool) -> anyhow::Result<()> {
        Ok(())
    }
}
fn error(value: impl std::fmt::Display) -> String {
    value.to_string()
}
async fn read_interrupt(signal: &crate::process_io::Descriptor) -> Result<i32, String> {
    let mut bytes = [0u8; std::mem::size_of::<libc::signalfd_siginfo>()];
    let count = signal.read(&mut bytes).await.map_err(error)?;
    if count != bytes.len() {
        return Err("short interrupt event".into());
    }
    let number = u32::from_ne_bytes(bytes[..4].try_into().map_err(error)?) as i32;
    if !matches!(number, libc::SIGINT | libc::SIGTERM) {
        return Err("unexpected interrupt event".into());
    }
    Ok(number)
}
impl CommandSession<'_> {
    fn sync(&self, run: &mut EngineRun<'_>) {
        if self.cargo_value {
            let commands = run.receipt["process_commands"].clone();
            let cleanup = run.receipt["process_cleanup"].clone();
            run.receipt.insert("cargo_value_commands".into(), commands);
            run.receipt
                .insert("cargo_value_process_cleanup".into(), cleanup);
        }
    }
    fn program(&self, value: &str) -> Result<PathBuf, String> {
        let Some(programs) = self.programs else {
            return Ok(PathBuf::from(value));
        };
        if let Some(path) = programs.get(value) {
            return Ok(path.clone());
        }
        let path = Path::new(value);
        if path.is_absolute() && programs.values().any(|allowed| allowed == path) {
            return Ok(path.to_owned());
        }
        Err(format!("mutation program is not operator-granted: {value}"))
    }
    async fn observe(
        &self,
        plan: crate::sandbox::CommandPlan,
        run: &mut EngineRun<'_>,
        seconds: u64,
        index: usize,
        capture: Capture,
    ) -> Result<confined_execution::Report, String> {
        let signal = run
            .supervisor
            .interrupts
            .register_reactor()
            .map_err(error)?;
        let pending = run.supervisor.interrupts.signal().map_err(error)?;
        let group = Cgroup::create(self.cgroup_parent, self.limits).map_err(error)?;
        run.receipt["process_commands"][index]["pending_cgroup"] = json!(group.report());
        run.receipt["process_commands"][index]["cgroup_identity_scope"] =
            json!("diagnostic only; not future kill permission");
        self.sync(run);
        run.write_receipt()?;
        let limits = confined_execution::Limits {
            timeout: Duration::from_secs(seconds),
            max_output_chars: 0,
            cpu_seconds: None,
            address_space_bytes: None,
        };
        let execution = match capture {
            Capture::Strict => confined_execution::start_filter_with_resources(
                plan,
                group,
                limits,
                Box::new(NoResources),
                Vec::new(),
                1024 * 1024,
            ),
            Capture::Metadata => confined_execution::start_metadata_with_resources(
                plan,
                group,
                limits,
                Box::new(NoResources),
            ),
            Capture::Complete => confined_execution::start_capture_with_resources(
                plan,
                group,
                limits,
                Box::new(NoResources),
            ),
        }
        .map_err(error)?;
        let cancel = execution.cancellation();
        let initial_wake = pending.map(|signal| {
            self.signal.set(Some(signal));
            cancel.cancel()
        });
        let wait = execution.wait();
        tokio::pin!(wait);
        let report = tokio::select! {
            result=&mut wait => result.map_err(error),
            received=read_interrupt(&signal) => {
                let wake=cancel.cancel(); let report=wait.await.map_err(error)?; wake.map_err(error)?;
                self.signal.set(Some(received?)); Ok(report)
            }
        }?;
        if let Some(wake) = initial_wake {
            wake.map_err(error)?;
        }
        Ok(report)
    }
    pub fn command(
        &self,
        run: &mut EngineRun<'_>,
        argv: &[String],
        cwd: &Path,
        timeout: i64,
        capture: Capture,
    ) -> Step<(CommandResult, String, Value)> {
        if !self.cleanup_complete.get() {
            return Err("previous confined command cleanup is unresolved".into());
        }
        let seconds = u64::try_from(timeout).map_err(error)?;
        if seconds == 0 || seconds > self.max_wall_seconds {
            return Err("mutation command exceeds granted wall bound".into());
        }
        let cwd = cwd.canonicalize().map_err(error)?;
        let scratch = self.scratch.canonicalize().map_err(error)?;
        if !cwd.starts_with(&scratch) {
            return Err("mutation command cwd escapes owned scratch".into());
        }
        let program = self.program(argv.first().ok_or("empty mutation command")?)?;
        let mut plan = self.sandbox.command(&program).map_err(error)?;
        plan.command().args(&argv[1..]).current_dir(&cwd);
        run.supervisor
            .interrupts
            .child_mask()
            .configure(plan.command());
        let commands = run
            .receipt
            .entry("process_commands")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("confined command ledger invalid")?;
        let index = commands.len();
        commands.push(json!({"argv":argv,"cwd":cwd,"state":"pending"}));
        run.receipt.insert("process_cleanup".into(), json!({"schema_version":1,"complete":false,"commands_observed":index,"scope":"actor disposition pending"}));
        self.sync(run);
        self.cleanup_complete.set(false);
        let observed = self
            .reactor
            .block_on(self.observe(plan, run, seconds, index, capture))?;
        self.cleanup_complete.set(observed.cleanup_complete);
        run.receipt.insert("process_cleanup".into(), json!({"schema_version":1,"complete":observed.cleanup_complete,"commands_observed":index+1,"scope":"owned cgroup cleanup"}));
        let provenance = json!({"outcome":observed.outcome,"exit_code":observed.exit_code,"signal":observed.signal,"duration_ms":observed.duration_ms,"errors":observed.errors,"cleanup_complete":observed.cleanup_complete,"resources":observed.resources,"stdout_bytes":observed.stdout_bytes,"stderr_bytes":observed.stderr_bytes});
        run.receipt["process_commands"][index] =
            json!({"argv":argv,"cwd":cwd,"state":"observed","confinement":provenance});
        self.sync(run);
        if let Some(signal) = self.signal.get() {
            return Err(Stop::Cancelled(signal));
        }
        if !observed.cleanup_complete
            || !observed.errors.is_empty()
            || !matches!(observed.outcome, Outcome::Exited | Outcome::TimedOut)
        {
            return Err(format!("confined mutation command failed: {provenance}").into());
        }
        let result = CommandResult {
            returncode: observed.exit_code,
            timed_out: observed.outcome == Outcome::TimedOut,
            duration_seconds: observed.duration_ms as f64 / 1000.0,
            stdout_tail: crate::mutation_run::last_chars(&observed.stdout, 4000),
            stderr_tail: crate::mutation_run::last_chars(&observed.stderr, 4000),
        };
        Ok((result, observed.stdout, provenance))
    }
}
