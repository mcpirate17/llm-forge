//! Bounded child processes for a mutation run.
//!
//! Each child owns a session, dies with this process, and is recorded in the
//! live-pgid registry for exactly its lifetime, so a run orphaned by a killed
//! forge stays findable. The leader stays unreaped until its group is killed:
//! a zombie leader pins the pgid, so the kill cannot reach a recycled group.
//! Spawning stays on the calling thread because `PR_SET_PDEATHSIG` binds a child
//! to the thread that forked it; reader threads only drain pipes.

use crate::json_canon::python_pretty;
use crate::mutation_campaign::{Step, Stop};
use crate::mutation_write::atomic_write;
use crate::process_group::ProcessGroup;
use crate::process_signals::Interrupts;
use conductor_native::project_paths;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const OUTPUT_TAIL_CHARS: usize = 4000;
const GROUP_DRAIN_SECONDS: u64 = 30;
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const MEMORY_CAP_ENV: &str = "MUTATION_MEMORY_CAP_GIB";
const DEFAULT_MEMORY_CAP_GIB: f64 = 8.0;
const LIVE_PGIDS_FILENAME: &str = "live_pgids.json";
const BYTES_PER_GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// One finished command: enough for a receipt, bounded by the output tails.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandResult {
    pub returncode: Option<i32>,
    pub timed_out: bool,
    pub duration_seconds: f64,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

/// `round(seconds, 6)`.
pub fn rounded(seconds: f64) -> f64 {
    (seconds * 1e6).round() / 1e6
}

impl CommandResult {
    pub fn as_value(&self) -> Value {
        json!({
            "returncode": self.returncode,
            "timed_out": self.timed_out,
            "duration_seconds": rounded(self.duration_seconds),
            "stdout_tail": self.stdout_tail,
            "stderr_tail": self.stderr_tail,
        })
    }

    /// Exited 0 within its bound.
    pub fn succeeded(&self) -> bool {
        !self.timed_out && self.returncode == Some(0)
    }

    /// The exit code as a refusal prints it; `None` is a child a signal ended.
    pub fn rc_text(&self) -> String {
        self.returncode
            .map_or_else(|| "None".to_string(), |code| code.to_string())
    }
}

/// The last `OUTPUT_TAIL_CHARS` characters, as Python slices a `str`.
fn tail(text: &str) -> String {
    let count = text.chars().count();
    text.chars()
        .skip(count.saturating_sub(OUTPUT_TAIL_CHARS))
        .collect()
}

/// `RLIMIT_DATA` for every child: the soft limit, and the hard limit it keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryCap {
    pub limit: u64,
    pub hard: u64,
}

/// The configured per-process data cap; `MUTATION_MEMORY_CAP_GIB<=0` disables it.
pub fn memory_cap() -> Result<Option<MemoryCap>, String> {
    cap_from(std::env::var(MEMORY_CAP_ENV).ok().as_deref())
}

fn cap_from(raw: Option<&str>) -> Result<Option<MemoryCap>, String> {
    let gib = match raw {
        None => DEFAULT_MEMORY_CAP_GIB,
        Some(text) => text
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .ok_or_else(|| {
                format!(
                    "{MEMORY_CAP_ENV} is not a number: {}",
                    crate::mutation_campaign::py_repr(text)
                )
            })?,
    };
    if gib <= 0.0 {
        return Ok(None);
    }
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `current` is a properly sized, initialized out-parameter.
    if unsafe { libc::getrlimit(libc::RLIMIT_DATA, &mut current) } != 0 {
        return Err(format!(
            "cannot read RLIMIT_DATA: {}",
            io::Error::last_os_error()
        ));
    }
    let wanted = (gib * BYTES_PER_GIB) as u64;
    let limit = if current.rlim_max == libc::RLIM_INFINITY {
        wanted
    } else {
        wanted.min(current.rlim_max)
    };
    Ok(Some(MemoryCap {
        limit,
        hard: current.rlim_max,
    }))
}

/// The pgid registry: the receipts' own `.iterations` scratch directory.
pub fn live_pgids_path(repo_root: &Path) -> Result<PathBuf, String> {
    let registry = project_paths::resolve(repo_root)?
        .get(1)
        .map(|entry| entry.0.clone())
        .ok_or_else(|| "project paths did not resolve a mutation registry".to_string())?;
    let campaigns = Path::new(&registry).parent().unwrap_or(Path::new(""));
    Ok(repo_root
        .join(campaigns)
        .join("receipts")
        .join(".iterations")
        .join(LIVE_PGIDS_FILENAME))
}

fn live_entries(path: &Path) -> Result<Vec<Value>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|payload| payload.get("live").and_then(Value::as_array).cloned())
        .ok_or_else(|| {
            format!(
                "live pgid registry {} is not a JSON object with a list under 'live'",
                path.display()
            )
        })
}

fn write_entries(path: &Path, entries: Vec<Value>) -> Result<(), String> {
    let body = python_pretty(json!({"live": entries})).map_err(|error| error.to_string())?;
    atomic_write(path, &body)
}

fn record_live_pgid(path: &Path, pgid: u32, argv0: &str) -> Result<(), String> {
    let mut entries = live_entries(path)?;
    entries.push(json!({"argv0": argv0, "engine_pid": std::process::id(), "pgid": pgid}));
    write_entries(path, entries)
}

fn forget_live_pgid(path: &Path, pgid: u32) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let remaining: Vec<Value> = live_entries(path)?
        .into_iter()
        .filter(|entry| entry.get("pgid").and_then(Value::as_u64) != Some(u64::from(pgid)))
        .collect();
    if !remaining.is_empty() {
        return write_entries(path, remaining);
    }
    fs::remove_file(path).map_err(|error| format!("cannot remove {}: {error}", path.display()))?;
    if let Some(parent) = path.parent() {
        // Another run may still own the directory; emptiness is its only claim.
        let _ = fs::remove_dir(parent);
    }
    Ok(())
}

fn leader_exited(pid: u32) -> io::Result<bool> {
    // SAFETY: zeroed siginfo is a valid out-parameter; WNOWAIT leaves the zombie,
    // so the pgid stays pinned until `ProcessGroup::stop` reaps it.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    if unsafe { libc::waitid(libc::P_PID, pid, &mut info, flags) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { info.si_pid() } != 0)
}

fn returncode(status: ExitStatus) -> Option<i32> {
    status
        .code()
        .or_else(|| status.signal().map(|signal| -signal))
}

struct Drain {
    stdout: JoinHandle<Vec<u8>>,
    stderr: JoinHandle<Vec<u8>>,
}

fn drain<R: Read + Send + 'static>(mut pipe: R) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        // A read error ends the capture early; the exit status still reports the run.
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    })
}

impl Drain {
    fn finished(&self) -> bool {
        self.stdout.is_finished() && self.stderr.is_finished()
    }

    /// Wait for EOF on both pipes after the group died, within the drain bound.
    fn settle(&self, argv: &[String]) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(GROUP_DRAIN_SECONDS);
        while !self.finished() {
            if Instant::now() >= deadline {
                return Err(format!(
                    "pgid still held its pipes {GROUP_DRAIN_SECONDS}s after SIGKILL; \
                     surviving descendants must be killed by hand: {argv:?}"
                ));
            }
            thread::sleep(POLL_INTERVAL);
        }
        Ok(())
    }

    fn collect(self) -> (String, String) {
        let text = |handle: JoinHandle<Vec<u8>>| {
            String::from_utf8_lossy(&handle.join().unwrap_or_default()).into_owned()
        };
        (text(self.stdout), text(self.stderr))
    }
}

enum Ending {
    Finished,
    TimedOut,
    Cancelled(i32),
}

/// Runs each mutation child under one interrupt owner, registry and memory cap.
pub struct Supervisor<'a> {
    pub interrupts: &'a Interrupts,
    pub registry: PathBuf,
    pub memory_cap: Option<MemoryCap>,
}

impl Supervisor<'_> {
    fn spawn(
        &self,
        argv: &[String],
        cwd: &Path,
        environment: &BTreeMap<String, String>,
    ) -> Result<ProcessGroup, String> {
        let (program, arguments) = argv
            .split_first()
            .ok_or_else(|| "refusing to run an empty command".to_string())?;
        ProcessGroup::verify_reaper().map_err(|error| error.to_string())?;
        let mut command = Command::new(program);
        command
            .args(arguments)
            .current_dir(cwd)
            .envs(environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: getpid has no preconditions; the value is compared in the child.
        let parent = unsafe { libc::getpid() };
        let cap = self.memory_cap;
        // SAFETY: only async-signal-safe calls run between fork and exec, and the
        // closure allocates nothing: every value was computed before the spawn.
        unsafe {
            command.pre_exec(move || bind_child(parent, cap));
        }
        self.interrupts.configure_child(&mut command);
        let child = command
            .spawn()
            .map_err(|error| format!("cannot start {program}: {error}"))?;
        ProcessGroup::from_child(child)
            .map_err(|error| format!("cannot isolate {program}: {error}"))
    }

    fn wait(&self, pgid: u32, drain: &Drain, deadline: Instant) -> Result<Ending, String> {
        loop {
            let signal = self
                .interrupts
                .signal()
                .map_err(|error| format!("cannot read interrupts: {error}"))?;
            if let Some(signal) = signal {
                return Ok(Ending::Cancelled(signal));
            }
            let exited = leader_exited(pgid)
                .map_err(|error| format!("cannot observe the leader: {error}"))?;
            if exited && drain.finished() {
                return Ok(Ending::Finished);
            }
            if Instant::now() >= deadline {
                return Ok(Ending::TimedOut);
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Run `argv` bounded by `timeout_seconds`; the full stdout rides along.
    ///
    /// A timeout is a result, not an error. A signal is `Stop::Cancelled`, after
    /// the child's whole group is dead.
    pub fn run(
        &self,
        argv: &[String],
        cwd: &Path,
        timeout_seconds: i64,
        environment: &BTreeMap<String, String>,
    ) -> Step<(CommandResult, String)> {
        let started = Instant::now();
        let mut group = self.spawn(argv, cwd, environment)?;
        let pgid = group.id();
        if let Err(error) = record_live_pgid(&self.registry, pgid, &argv[0]) {
            // A run that cannot be reaped must not start. Stop kills and reaps.
            let _ = group.stop();
            return Err(Stop::Refused(error));
        }
        let drain = Drain {
            stdout: drain(group.take_stdout().expect("stdout is piped")),
            stderr: drain(group.take_stderr().expect("stderr is piped")),
        };
        let deadline = started + Duration::from_secs(timeout_seconds.max(0) as u64);
        let ending = self.wait(pgid, &drain, deadline);
        // Kills what the leader left behind, then reaps it, in that order.
        let status = group.stop();
        let settled = drain.settle(argv);
        let forgotten = forget_live_pgid(&self.registry, pgid);
        let ending = ending?;
        settled?;
        forgotten?;
        let status = status.map_err(|error| format!("cannot reap {}: {error}", argv[0]))?;
        let (stdout, stderr) = drain.collect();
        let timed_out = matches!(ending, Ending::TimedOut);
        if let Ending::Cancelled(signal) = ending {
            return Err(Stop::Cancelled(signal));
        }
        let result = CommandResult {
            returncode: if timed_out { None } else { returncode(status) },
            timed_out,
            duration_seconds: started.elapsed().as_secs_f64(),
            stdout_tail: tail(&stdout),
            stderr_tail: tail(&stderr),
        };
        Ok((result, stdout))
    }
}

/// Child-side setup between fork and exec. Async-signal-safe and allocation-free.
fn bind_child(parent: libc::pid_t, cap: Option<MemoryCap>) -> io::Result<()> {
    // SAFETY: plain syscalls with valid arguments; see `Supervisor::spawn`.
    unsafe {
        if libc::setsid() < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong) != 0 {
            return Err(io::Error::last_os_error());
        }
        // The parent may have died before prctl took effect; nothing would fire.
        if libc::getppid() != parent {
            return Err(io::Error::from_raw_os_error(libc::ESRCH));
        }
        if let Some(cap) = cap {
            let limit = libc::rlimit {
                rlim_cur: cap.limit,
                rlim_max: cap.hard,
            };
            if libc::setrlimit(libc::RLIMIT_DATA, &limit) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "mutation_process_tests.rs"]
mod tests;
