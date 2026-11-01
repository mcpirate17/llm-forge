//! Bounded synchronous management probes using kernel readiness, not sleep loops
//! or waiter threads. Async services call this through a bounded blocking pool.
use crate::process_group::ProcessGroup;
use crate::process_io::{make_nonblocking, open_pidfd};
use crate::process_signals::Interrupts;
use anyhow::{ensure, Context, Result};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

pub fn run(command: &mut Command, timeout: Duration, output_limit: usize) -> Result<Output> {
    run_inner(command, None, timeout, output_limit, None)
}

#[derive(Debug)]
pub struct Interrupted;
impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("probe interrupted")
    }
}
impl std::error::Error for Interrupted {}

#[derive(Debug)]
pub struct TimedOut(pub Duration);
impl std::fmt::Display for TimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "probe timed out after {} ms", self.0.as_millis())
    }
}
impl std::error::Error for TimedOut {}

/// A foreground supervisor can cancel a probe through its owned signalfd.
/// Cancellation tears down only this probe's process group before returning.
pub fn run_interruptible(
    command: &mut Command,
    timeout: Duration,
    output_limit: usize,
    interrupts: &Interrupts,
) -> Result<Output> {
    if interrupts.interrupted()? {
        return Err(Interrupted.into());
    }
    interrupts.configure_child(command);
    run_inner(command, None, timeout, output_limit, Some(interrupts))
}

/// Deliver bounded stdin while draining both output streams on the same kernel
/// readiness loop. A Unix socketpair provides byte-stream stdin and MSG_NOSIGNAL
/// writes; callers do not need to change a process-wide signal disposition.
pub fn run_with_input(
    command: &mut Command,
    input: &[u8],
    timeout: Duration,
    output_limit: usize,
) -> Result<Output> {
    ensure!(input.len() <= 1024 * 1024, "probe stdin exceeds 1 MiB");
    run_inner(command, Some(input), timeout, output_limit, None)
}

fn run_inner(
    command: &mut Command,
    input: Option<&[u8]>,
    timeout: Duration,
    output_limit: usize,
    interrupts: Option<&Interrupts>,
) -> Result<Output> {
    ensure!(!timeout.is_zero(), "probe timeout must be positive");
    ensure!(
        (1..=1024 * 1024).contains(&output_limit),
        "probe output limit must be 1..1048576 bytes"
    );
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("probe timeout is too large")?;
    let mut input = Input::prepare(command, input)?;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut group = ProcessGroup::spawn(command)
        .with_context(|| format!("starting probe {:?}", command.get_program()))?;
    // Release Command's parent-side copy of the supplied socket receiver. The
    // child now owns the only receiver, so early stdin closure is observable.
    command.stdin(Stdio::null());
    let pidfd = open_pidfd(group.child())?;
    let mut out = Stream::new(group.take_stdout().context("probe stdout")?.into())?;
    let mut err = Stream::new(group.take_stderr().context("probe stderr")?.into())?;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(TimedOut(timeout))?;
        let mut descriptors = [
            out.poll(),
            err.poll(),
            libc::pollfd {
                fd: pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            input.poll(),
            libc::pollfd {
                fd: interrupts.map_or(-1, AsRawFd::as_raw_fd),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        poll(&mut descriptors, remaining)?;
        if let Some(interrupts) = interrupts {
            if interrupts.interrupted()? {
                return Err(Interrupted.into());
            }
        }
        let exited = descriptors[2].revents & (libc::POLLIN | libc::POLLHUP) != 0;
        let status = if exited { Some(group.stop()?) } else { None };
        if !exited {
            input.write()?;
        }
        out.drain(output_limit, err.bytes.len())?;
        err.drain(output_limit, out.bytes.len())?;
        if let Some(status) = status {
            ensure!(
                input.remaining.is_empty(),
                "probe exited before accepting all stdin"
            );
            return Ok(Output {
                status,
                stdout: out.bytes,
                stderr: err.bytes,
            });
        }
    }
}

fn poll(descriptors: &mut [libc::pollfd], remaining: Duration) -> Result<()> {
    let millis = remaining.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
    // SAFETY: poll updates exactly the initialized descriptor slice.
    if unsafe {
        libc::poll(
            descriptors.as_mut_ptr(),
            descriptors.len() as libc::nfds_t,
            millis,
        )
    } < 0
    {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error).context("wait for probe readiness");
        }
    }
    ensure!(
        descriptors.iter().all(|p| p.revents & libc::POLLNVAL == 0),
        "probe descriptor became invalid"
    );
    Ok(())
}

struct Input<'a> {
    socket: Option<UnixStream>,
    remaining: &'a [u8],
}
impl<'a> Input<'a> {
    fn prepare(command: &mut Command, bytes: Option<&'a [u8]>) -> Result<Self> {
        let Some(bytes) = bytes else {
            command.stdin(Stdio::null());
            return Ok(Self {
                socket: None,
                remaining: &[],
            });
        };
        let (sender, receiver) = UnixStream::pair()?;
        sender.set_nonblocking(true)?;
        command.stdin(Stdio::from(std::os::fd::OwnedFd::from(receiver)));
        Ok(Self {
            socket: (!bytes.is_empty()).then_some(sender),
            remaining: bytes,
        })
    }
    fn poll(&self) -> libc::pollfd {
        libc::pollfd {
            fd: self.socket.as_ref().map_or(-1, AsRawFd::as_raw_fd),
            events: libc::POLLOUT,
            revents: 0,
        }
    }
    fn write(&mut self) -> Result<()> {
        let Some(socket) = &self.socket else {
            return Ok(());
        };
        let bytes = &self.remaining[..self.remaining.len().min(65536)];
        // SAFETY: borrowed bytes and socket are valid. MSG_NOSIGNAL prevents a
        // closed child stdin from terminating the caller through SIGPIPE.
        let count = unsafe {
            libc::send(
                socket.as_raw_fd(),
                bytes.as_ptr().cast(),
                bytes.len(),
                libc::MSG_NOSIGNAL,
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Ok(());
            }
            return Err(error).context("write probe stdin");
        }
        ensure!(count > 0, "probe stdin accepted zero bytes");
        self.remaining = &self.remaining[count as usize..];
        if self.remaining.is_empty() {
            self.socket.take();
        }
        Ok(())
    }
}

struct Stream {
    file: File,
    bytes: Vec<u8>,
    eof: bool,
}

impl Stream {
    fn new(fd: std::os::fd::OwnedFd) -> Result<Self> {
        make_nonblocking(&fd)?;
        Ok(Self {
            file: fd.into(),
            bytes: Vec::new(),
            eof: false,
        })
    }
    fn poll(&self) -> libc::pollfd {
        libc::pollfd {
            fd: if self.eof { -1 } else { self.file.as_raw_fd() },
            events: libc::POLLIN,
            revents: 0,
        }
    }
    fn drain(&mut self, limit: usize, other: usize) -> Result<()> {
        let mut buffer = [0; 4096];
        loop {
            match self.file.read(&mut buffer) {
                Ok(0) => {
                    self.eof = true;
                    return Ok(());
                }
                Ok(count) => {
                    ensure!(
                        self.bytes.len() + other + count <= limit,
                        "probe output exceeds {limit} bytes"
                    );
                    self.bytes.extend_from_slice(&buffer[..count]);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    return Ok(())
                }
                Err(error) => return Err(error).context("read probe output"),
            }
        }
    }
}
