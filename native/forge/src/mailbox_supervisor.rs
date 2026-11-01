//! Explicit finite delivery retries; pre-existing receivers are never owned or stopped.
use super::events as io;
use super::{registry, startup, store::Store};
use anyhow::{ensure, Context, Result};
use clap::Args;
use forge::process_group::ProcessGroup;
use forge::process_probe::{self, Interrupted, TimedOut};
use forge::process_signals::Interrupts;
use serde_json::{json, Value};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Args)]
pub struct SupervisorArgs {
    #[arg(long)]
    as_name: String,
    /// Finite lifetime in seconds (1..3600).
    #[arg(long, default_value_t = 300.0)]
    duration: f64,
    /// Retry reconciliation interval in seconds (0.1..60); idle queues wait up to 60s.
    #[arg(long, default_value_t = 5.0)]
    interval: f64,
    #[arg(long, default_value_t = 100)]
    max_messages: usize,
    /// Start and own a receiver only if no valid registered endpoint is running.
    #[arg(long)]
    serve: bool,
    #[arg(long, default_value_t = 3)]
    max_restarts: usize,
}

struct Receiver {
    group: ProcessGroup,
    pidfd: OwnedFd,
}

struct Supervisor<'a> {
    args: &'a SupervisorArgs,
    root: &'a Path,
    state_path: PathBuf,
    deadline: Instant,
    interrupts: Interrupts,
    watch: io::Watch,
    receiver: Option<Receiver>,
    reader: Option<Store>,
    state: Value,
    starts: usize,
}

impl SupervisorArgs {
    fn validate(&self) -> Result<()> {
        super::store::validate_identity(&self.as_name)?;
        ensure!(
            (1.0..=3600.0).contains(&self.duration),
            "duration must be 1..3600 seconds"
        );
        ensure!(
            (0.1..=60.0).contains(&self.interval),
            "interval must be 0.1..60 seconds"
        );
        ensure!(
            (1..=1000).contains(&self.max_messages),
            "max_messages must be 1..1000"
        );
        ensure!(self.max_restarts <= 10, "max_restarts must be 0..10");
        Ok(())
    }
}

fn command(root: &Path) -> Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["mailbox", "--state-dir"])
        .arg(root)
        .current_dir(root);
    Ok(command)
}

fn summarize_flush(output: std::process::Output) -> Result<Value> {
    let code = output
        .status
        .code()
        .context("flush terminated without an exit code")?;
    ensure!(
        matches!(code, 0 | 3),
        "flush exited {code}: {}",
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(500)
            .collect::<String>()
    );
    let value = forge::tool_schema::parse_document(std::str::from_utf8(&output.stdout)?)?;
    let rows = value
        .as_array()
        .context("flush returned a non-array receipt")?;
    let mut counts = json!({"delivered":0,"queued":0,"failed":0});
    for row in rows {
        let status = row["status"]
            .as_str()
            .context("flush receipt has no status")?;
        let count = counts
            .get_mut(status)
            .context("flush receipt has an invalid status")?;
        *count = json!(count.as_u64().context("invalid flush counter")? + 1);
    }
    Ok(json!({"status":"complete","exit_code":code,"counts":counts}))
}

impl Supervisor<'_> {
    fn remaining(&self) -> Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .context("supervisor lifetime expired during receiver startup")
    }

    fn flush(&self) -> Result<Value> {
        if !self
            .reader
            .as_ref()
            .context("supervisor reader missing")?
            .has_outbound(None)?
        {
            return Ok(json!({"status":"complete","exit_code":0,
                "counts":{"delivered":0,"queued":0,"failed":0}}));
        }
        let mut command = command(self.root)?;
        command
            .args([
                "flush",
                "--defer-busy",
                "--as-name",
                &self.args.as_name,
                "--max-messages",
            ])
            .arg(self.args.max_messages.to_string());
        let timeout = self.remaining()?.min(Duration::from_secs(15));
        match process_probe::run_interruptible(&mut command, timeout, 1024 * 1024, &self.interrupts)
        {
            Ok(output) => summarize_flush(output),
            Err(error) if error.is::<TimedOut>() => Ok(json!({"status":"timeout",
                "detail":"pending sends retained for retry; delivery may have reached the peer"})),
            Err(error) => Err(error),
        }
    }

    fn ensure_receiver(&mut self) -> Result<()> {
        if !self.args.serve || self.receiver.is_some() {
            return Ok(());
        }
        let records = registry::load(self.root)?;
        let record = records
            .get(&self.args.as_name)
            .context("supervised identity was removed")?;
        let _lock = startup::startup_lock(self.root, &self.args.as_name)?;
        if registry::probe_for(record, self.remaining()?.min(Duration::from_secs(1)))["status"]
            == "up"
        {
            return Ok(());
        }
        if self.interrupts.interrupted()? {
            return Err(Interrupted.into());
        }
        ensure!(
            self.starts <= self.args.max_restarts,
            "owned receiver exhausted restart budget"
        );
        let log = startup::owned_file(&self.state_path.with_file_name("supervised-serve.log"))?;
        ensure!(
            log.metadata()?.len() <= 1024 * 1024,
            "supervised-serve.log exceeds 1 MiB; rotate it"
        );
        let mut command = command(self.root)?;
        command
            .args(["serve", "--name", &self.args.as_name, "--lifetime-seconds"])
            .arg(self.remaining()?.as_secs_f64().ceil().max(1.0).to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(log);
        self.interrupts.configure_child(&mut command);
        let mut group = ProcessGroup::spawn(&mut command)?;
        let ready = startup::ready_record(
            &mut group,
            self.remaining()?.min(Duration::from_secs(3)),
            Some(&self.interrupts),
        )?;
        ensure!(
            ready["pid"].as_u64() == Some(u64::from(group.id()))
                && ready["name"].as_str() == Some(&self.args.as_name)
                && ready["port"].as_u64() == Some(u64::from(record.port)),
            "receiver readiness identity mismatch"
        );
        let pidfd = io::pidfd(group.child())?;
        self.receiver = Some(Receiver { group, pidfd });
        self.starts += 1;
        self.state["endpoint_pid"] = json!(self.receiver.as_ref().map(|r| r.group.id()));
        self.state["endpoint_starts"] = json!(self.starts);
        self.publish()?;
        Ok(())
    }

    fn stop_receiver(&mut self) -> Result<()> {
        if let Some(mut receiver) = self.receiver.take() {
            receiver.group.stop().context("stop owned receiver")?;
        }
        self.state["endpoint_pid"] = Value::Null;
        Ok(())
    }

    fn publish(&mut self) -> Result<()> {
        self.state["updated_at"] = json!(crate::instant::isoformat_utc(crate::instant::now()));
        registry::atomic_json(&self.state_path, &self.state)
    }

    fn cycles(&mut self) -> Result<()> {
        let mut last_written = Instant::now();
        let mut previous = Value::Null;
        while Instant::now() < self.deadline {
            if self.interrupts.interrupted()? {
                return Err(Interrupted.into());
            }
            self.ensure_receiver()?;
            if self.reader.is_none() {
                self.reader = Some(Store::required(self.root, &self.args.as_name, false)?);
            }
            if Instant::now() >= self.deadline {
                break;
            }
            let flush = self.flush()?;
            self.state["cycles"] = json!(self.state["cycles"].as_u64().unwrap_or(0) + 1);
            self.state["last_flush"] = flush.clone();
            if previous != flush || last_written.elapsed() >= Duration::from_secs(60) {
                self.publish()?;
                last_written = Instant::now();
            }
            previous = flush;
            if Instant::now() >= self.deadline {
                break;
            }
            if self.interrupts.interrupted()? {
                return Err(Interrupted.into());
            }
            // Drain our own SQLite writes, then inspect pending work read-only.
            // Enqueues before this read are seen in SQLite; later ones wake inotify.
            self.watch.drain()?;
            let pending = self
                .reader
                .as_ref()
                .context("supervisor reader missing")?
                .has_outbound(None)?
                || previous["exit_code"] == 3
                || previous["status"] == "timeout";
            let interval = Duration::from_secs_f64(if pending { self.args.interval } else { 60.0 });
            let until = self.deadline.min(Instant::now() + interval);
            match self.watch.wait(
                until,
                &self.interrupts,
                self.receiver.as_ref().map(|r| &r.pidfd),
            )? {
                io::Event::Interrupted => return Err(Interrupted.into()),
                io::Event::Exited => self.stop_receiver()?,
                io::Event::Changed | io::Event::Timer => {}
            }
        }
        Ok(())
    }
}

pub fn run(args: SupervisorArgs, root: &Path) -> Result<(Value, u8)> {
    args.validate()?;
    let root = root.canonicalize()?;
    ensure!(
        registry::load(&root)?.contains_key(&args.as_name),
        "unknown supervisor identity"
    );
    let directory = root.join(&args.as_name);
    let startup_guard = startup::startup_lock(&root, &args.as_name)?;
    let _lease = forge::file_lock::Exclusive::try_new(
        startup::owned_file(&directory.join(".supervisor.lock"))?,
        "mailbox supervisor",
    )?;
    let state_path = directory.join("supervisor.json");
    drop(startup::owned_file(&state_path)?);
    let watch = io::Watch::new(&directory)?;
    drop(startup_guard);
    let interrupts = Interrupts::install()?;
    let mut supervisor = Supervisor {
        args: &args,
        root: &root,
        state_path,
        deadline: Instant::now() + Duration::from_secs_f64(args.duration),
        interrupts,
        watch,
        receiver: None,
        reader: None,
        starts: 0,
        state: json!({"schema_version":1,"identity":args.as_name,"pid":std::process::id(),
            "started_at":crate::instant::isoformat_utc(crate::instant::now()),
            "duration_seconds":args.duration,"status":"running","cycles":0,
            "endpoint_pid":null,"endpoint_starts":0}),
    };
    supervisor.publish()?;
    let mut result = supervisor.cycles();
    if let Err(error) = supervisor.stop_receiver() {
        result = Err(error);
    }
    let interrupted = result
        .as_ref()
        .is_err_and(|error| error.is::<Interrupted>());
    supervisor.state["status"] = json!(if interrupted {
        "interrupted"
    } else if result.is_ok() {
        "completed"
    } else {
        "failed"
    });
    if let Err(error) = &result {
        supervisor.state["error"] =
            json!(format!("{error:#}").chars().take(500).collect::<String>());
    }
    supervisor.state["ended_at"] = json!(crate::instant::isoformat_utc(crate::instant::now()));
    supervisor.publish()?;
    if !interrupted {
        result?;
    }
    Ok((supervisor.state, if interrupted { 130 } else { 0 }))
}
