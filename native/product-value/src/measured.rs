//! Linux per-child wait4 accounting; fixed workloads, no shell or inference.
use anyhow::{ensure, Context, Result};
use conductor_native::performance_receipt::Sample;
use serde_json::{json, Value};
use std::{
    fs::{self, File},
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicI32, Ordering},
        OnceLock,
    },
    time::{Duration, Instant},
};
static DEADLINE: OnceLock<Instant> = OnceLock::new();
pub fn budget(duration: Duration) -> Result<()> {
    DEADLINE
        .set(Instant::now() + duration)
        .map_err(|_| anyhow::anyhow!("budget already initialized"))
}
pub fn expired() -> bool {
    DEADLINE.get().is_some_and(|d| Instant::now() >= *d)
}
static INTERRUPT: AtomicI32 = AtomicI32::new(0);
extern "C" fn interrupted(signal: i32) {
    INTERRUPT.store(signal, Ordering::Relaxed);
}
pub fn signal() -> Option<i32> {
    let signal = INTERRUPT.load(Ordering::Relaxed);
    (signal != 0).then_some(signal)
}
pub fn install() -> Result<()> {
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
        action.sa_sigaction = interrupted as *const () as usize;
        unsafe {
            libc::sigemptyset(&mut action.sa_mask);
        }
        ensure!(
            unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } == 0,
            "install benchmark interrupt handler"
        );
    }
    Ok(())
}
pub struct Observation {
    pub sample: Sample,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
}
impl Observation {
    pub fn record(&self) -> Value {
        json!({"sample":self.sample,"exit_code":self.code,"signal":self.signal,"timed_out":self.timed_out,"cancelled":self.cancelled,
        "stdout_bytes":self.stdout.len(),"stdout_sha256":crate::fixture::hash(&self.stdout),"stderr_bytes":self.stderr.len(),"stderr":String::from_utf8_lossy(&self.stderr).chars().take(2048).collect::<String>()})
    }
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out && !self.cancelled
    }
    pub fn require_success(self) -> Result<Self> {
        ensure!(self.success(), "child failed/censored: {}", self.record());
        Ok(self)
    }
}
struct Owner {
    child: Option<Child>,
    pid: i32,
}
impl Drop for Owner {
    fn drop(&mut self) {
        if self.child.is_some() {
            unsafe {
                libc::kill(-self.pid, libc::SIGKILL);
            }
            loop {
                let got = unsafe { libc::waitpid(self.pid, std::ptr::null_mut(), 0) };
                if got < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                    continue;
                }
                break;
            }
        }
    }
}
fn bounded(path: &Path) -> Result<Vec<u8>> {
    let file = File::open(path)?;
    ensure!(
        file.metadata()?.len() <= 4 * 1024 * 1024,
        "child output exceeded4MiB"
    );
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 4 * 1024 * 1024,
        "child output grew beyond4MiB"
    );
    Ok(bytes)
}
pub fn run(
    command: &mut Command,
    logs: &Path,
    tag: &str,
    timeout: Duration,
) -> Result<Observation> {
    let timeout = timeout.min(
        DEADLINE
            .get()
            .map(|d| d.saturating_duration_since(Instant::now()))
            .unwrap_or(timeout),
    );
    ensure!(
        !timeout.is_zero() && signal().is_none(),
        "deadline/interruption before child launch"
    );
    let out = logs.join(format!("{tag}.stdout"));
    let err = logs.join(format!("{tag}.stderr"));
    command
        .stdout(Stdio::from(File::create(&out)?))
        .stderr(Stdio::from(File::create(&err)?))
        .stdin(Stdio::null())
        .process_group(0);
    let parent = unsafe { libc::getpid() };
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
            }
            Ok(())
        });
    }
    let started = Instant::now();
    let child = command.spawn()?;
    let pid = child.id() as i32;
    let mut owner = Owner {
        child: Some(child),
        pid,
    };
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
    ensure!(
        raw >= 0,
        "pidfd_open required: {}",
        std::io::Error::last_os_error()
    );
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let Completion {
        usage,
        status,
        timed_out,
        cancelled,
    } = wait_child(pid, &fd, started, timeout)?;
    owner.child.take();
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    let stdout = bounded(&out)?;
    let stderr = bounded(&err)?;
    fs::remove_file(out)?;
    fs::remove_file(err)?;
    let cpu_ms = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64 * 1000.0
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1000.0;
    let max_rss_bytes = u64::try_from(usage.ru_maxrss)?
        .checked_mul(1024)
        .context("RSS overflow")?;
    ensure!(max_rss_bytes > 0, "missing Linux child RSS accounting");
    Ok(Observation {
        sample: Sample {
            wall_ms,
            cpu_ms,
            max_rss_bytes,
        },
        stdout,
        stderr,
        code: libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status)),
        signal: libc::WIFSIGNALED(status).then(|| libc::WTERMSIG(status)),
        timed_out,
        cancelled,
    })
}

struct Completion {
    usage: libc::rusage,
    status: i32,
    timed_out: bool,
    cancelled: bool,
}
fn wait_child(pid: i32, fd: &OwnedFd, started: Instant, timeout: Duration) -> Result<Completion> {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let (timed_out, cancelled) = loop {
        let cancelled = signal().is_some();
        let timed_out = started.elapsed() >= timeout;
        if cancelled || timed_out {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
            break (timed_out, cancelled);
        }
        let millis = timeout
            .saturating_sub(started.elapsed())
            .as_millis()
            .clamp(1, 100) as i32;
        let ready = unsafe { libc::poll(&mut poll, 1, millis) };
        if ready < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        ensure!(
            ready >= 0,
            "poll failed: {}",
            std::io::Error::last_os_error()
        );
        if ready > 0 {
            break (false, false);
        }
    };
    let mut status = 0;
    let mut usage = unsafe { std::mem::zeroed::<libc::rusage>() };
    loop {
        let waited = unsafe { libc::wait4(pid, &mut status, 0, &mut usage) };
        if waited < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        ensure!(
            waited == pid,
            "wait4 failed: {}",
            std::io::Error::last_os_error()
        );
        break;
    }
    Ok(Completion {
        usage,
        status,
        timed_out,
        cancelled,
    })
}

pub fn compiler_guard() -> Result<()> {
    let mut found = Vec::new();
    for (index, entry) in fs::read_dir("/proc")?.enumerate() {
        ensure!(index < 16384, "process inventory limit exceeded");
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .bytes()
            .all(|b| b.is_ascii_digit())
        {
            let comm = match fs::read_to_string(entry.path().join("comm")) {
                Ok(comm) => comm,
                Err(error) => {
                    if fs::metadata(entry.path())
                        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                    {
                        continue;
                    }
                    return Err(error).with_context(|| {
                        format!(
                            "cannot inspect compiler admission for {}",
                            entry.path().display()
                        )
                    });
                }
            };
            if [
                "cargo",
                "rustc",
                "clippy-driver",
                "cc1",
                "cc1plus",
                "clang",
                "gcc",
                "g++",
                "ld",
                "lld",
                "rust-lld",
            ]
            .contains(&comm.trim())
            {
                found.push(format!(
                    "{}:{}",
                    entry.file_name().to_string_lossy(),
                    comm.trim()
                ));
            }
        }
    }
    ensure!(found.is_empty(), "competing compiler load: {found:?}");
    Ok(())
}
