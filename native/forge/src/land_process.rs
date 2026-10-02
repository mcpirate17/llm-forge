//! Bounded logs, process-group cleanup, Linux wait4 usage and pidfd waits.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use super::{Outcome, ShellRun};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub wall_ms: f64,
    pub cpu_ms: f64,
    /// Linux wait4 ru_maxrss: maximum process RSS, not a simultaneous tree sum.
    pub max_rss_bytes: u64,
    pub retained_bytes: u64,
    pub discarded_bytes: u64,
}

pub struct Measurement {
    pub outcome: Outcome,
    pub usage: Usage,
}

struct Log {
    file: File,
    retained: u64,
    discarded: u64,
    limit: u64,
}

fn drain(
    input: impl Read + AsRawFd + Send + 'static,
    log: Arc<Mutex<Log>>,
    done: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<Result<()>>> {
    // SAFETY: fcntl accesses the owned pipe descriptor, preserving existing flags.
    let flags = unsafe { libc::fcntl(input.as_raw_fd(), libc::F_GETFL) };
    ensure!(
        flags >= 0
            && unsafe { libc::fcntl(input.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                >= 0,
        "setting nonblocking pipe: {}",
        std::io::Error::last_os_error()
    );
    Ok(std::thread::spawn(move || {
        let mut input = input;
        let mut buffer = [0; 8192];
        let mut stopped_at = None;
        loop {
            if done.load(Ordering::Acquire)
                && stopped_at.get_or_insert_with(Instant::now).elapsed()
                    > std::time::Duration::from_millis(200)
            {
                return Ok(());
            }
            let count = match input.read(&mut buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if stopped_at.is_some() {
                        return Ok(());
                    }
                    let mut fd = libc::pollfd {
                        fd: input.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: poll owns one initialized descriptor for this reader.
                    let status = unsafe { libc::poll(&mut fd, 1, 50) };
                    if status < 0
                        && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                    {
                        return Err(std::io::Error::last_os_error().into());
                    }
                    continue;
                }
                result => result?,
            };
            if count == 0 {
                return Ok(());
            }
            let mut log = log
                .lock()
                .map_err(|_| anyhow::anyhow!("log writer poisoned"))?;
            let keep = (log.limit.saturating_sub(log.retained)).min(count as u64) as usize;
            log.file.write_all(&buffer[..keep])?;
            log.retained += keep as u64;
            log.discarded += (count - keep) as u64;
        }
    }))
}

struct Group {
    child: Child,
    live: bool,
}

impl Group {
    fn kill(&self) {
        if self.live {
            // SAFETY: the unreaped leader pins the process-group identity.
            unsafe {
                libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
            }
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if self.live {
            self.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(target_os = "linux")]
fn await_exit(child: &Child, timeout: std::time::Duration) -> Result<bool> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    // SAFETY: pidfd_open takes a live child pid and zero flags.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, child.id(), 0) };
    if fd >= 0 {
        // SAFETY: syscall returned a newly owned descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        let began = Instant::now();
        loop {
            let remaining = timeout.saturating_sub(began.elapsed());
            let mut poll = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = remaining.as_millis().min(i32::MAX as u128) as i32;
            // SAFETY: poll points to one initialized descriptor.
            let status = unsafe { libc::poll(&mut poll, 1, millis) };
            if status > 0 {
                return Ok(true);
            }
            if status == 0 {
                return Ok(false);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
    }
    let error = std::io::Error::last_os_error();
    ensure!(
        matches!(
            error.raw_os_error(),
            Some(libc::ENOSYS | libc::EINVAL | libc::EPERM)
        ),
        "pidfd_open: {error}"
    );
    let began = Instant::now();
    loop {
        // SAFETY: initialized result storage, child belongs to this process;
        // WNOWAIT leaves the leader unreaped until group cleanup.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if unsafe { info.si_pid() } != 0 {
            return Ok(true);
        }
        if began.elapsed() >= timeout {
            return Ok(false);
        }
        std::thread::sleep(
            std::time::Duration::from_millis(5).min(timeout.saturating_sub(began.elapsed())),
        );
    }
}

#[cfg(not(target_os = "linux"))]
fn await_exit(_: &Child, _: std::time::Duration) -> Result<bool> {
    anyhow::bail!("measured process execution requires Linux")
}

#[cfg(target_os = "linux")]
fn reap(child: &Child) -> Result<(i32, f64, u64)> {
    let mut status = 0;
    // SAFETY: wait4 writes initialized storage and reaps our owned child only.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe { libc::wait4(child.id() as i32, &mut status, 0, &mut usage) };
        if result >= 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    }
    let cpu = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as f64 * 1000.0
        + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as f64 / 1000.0;
    let code = if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        -1
    };
    Ok((code, cpu, usage.ru_maxrss.max(0) as u64 * 1024))
}

#[cfg(not(target_os = "linux"))]
fn reap(_: &Child) -> Result<(i32, f64, u64)> {
    anyhow::bail!("measured process execution requires Linux")
}

/// Native measurement without an interpreter or a profiler dependency.
pub fn run_measured(run: &ShellRun, log_bytes: u64) -> Result<Measurement> {
    ensure!(
        cfg!(target_os = "linux"),
        "measured process execution requires Linux wait4/pidfd"
    );
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(run.log)?;
    let log = Arc::new(Mutex::new(Log {
        file,
        retained: 0,
        discarded: 0,
        limit: log_bytes,
    }));
    let began = Instant::now();
    let child = Command::new("sh")
        .arg("-c")
        .arg(run.command)
        .current_dir(run.cwd)
        .envs(run.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("spawning measured process group")?;
    let mut group = Group { child, live: true };
    let done = Arc::new(AtomicBool::new(false));
    let stdout = drain(
        group.child.stdout.take().context("stdout pipe")?,
        log.clone(),
        done.clone(),
    )?;
    let stderr = drain(
        group.child.stderr.take().context("stderr pipe")?,
        log.clone(),
        done.clone(),
    )?;
    let completed = await_exit(&group.child, run.timeout)?;
    group.kill();
    let (code, cpu_ms, max_rss_bytes) = reap(&group.child)?;
    group.live = false;
    done.store(true, Ordering::Release);
    stdout
        .join()
        .map_err(|_| anyhow::anyhow!("stdout drainer panicked"))??;
    stderr
        .join()
        .map_err(|_| anyhow::anyhow!("stderr drainer panicked"))??;
    let log = log
        .lock()
        .map_err(|_| anyhow::anyhow!("log writer poisoned"))?;
    log.file.sync_all()?;
    Ok(Measurement {
        outcome: if !completed {
            Outcome::TimedOut
        } else if code == 0 {
            Outcome::Passed
        } else {
            Outcome::Failed(code)
        },
        usage: Usage {
            wall_ms: began.elapsed().as_secs_f64() * 1000.0,
            cpu_ms,
            max_rss_bytes,
            retained_bytes: log.retained,
            discarded_bytes: log.discarded,
        },
    })
}
