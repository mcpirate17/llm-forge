//! Shared non-reaping pipe observer. Process/cgroup ownership stays with callers.
use super::{Interrupted, TimedOut};
use crate::process_capture_tail::TailBuffer;
use crate::process_io::make_nonblocking;
use anyhow::{ensure, Context, Result};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const QUANTUM: usize = 64 * 1024;
#[derive(Clone, Copy)]
pub(crate) enum CapturePolicy {
    StrictCombined(usize),
    TailChars(usize),
}
impl CapturePolicy {
    pub(crate) fn validate(self) -> Result<()> {
        match self {
            Self::StrictCombined(limit) => ensure!(
                (1..=1024 * 1024).contains(&limit),
                "probe output limit must be 1..1048576 bytes"
            ),
            Self::TailChars(limit) => {
                ensure!(
                    (1..=crate::process_capture_tail::MAX_OUTPUT_CHARS).contains(&limit),
                    "tail character limit must be 1..262144"
                );
            }
        }
        Ok(())
    }
}
enum Capture {
    Bytes(Vec<u8>),
    Tail(TailBuffer),
}
impl Capture {
    fn new(policy: CapturePolicy) -> Result<Self> {
        Ok(match policy {
            CapturePolicy::StrictCombined(_) => Self::Bytes(Vec::new()),
            CapturePolicy::TailChars(n) => Self::Tail(TailBuffer::new(n)?),
        })
    }
    fn strict_len(&self) -> usize {
        match self {
            Self::Bytes(bytes) => bytes.len(),
            Self::Tail(_) => 0,
        }
    }
    fn push(&mut self, bytes: &[u8]) {
        match self {
            Self::Bytes(out) => out.extend_from_slice(bytes),
            Self::Tail(out) => out.push(bytes),
        }
    }
    fn finish(self) -> Vec<u8> {
        match self {
            Self::Bytes(out) => out,
            Self::Tail(out) => out.finish().into_bytes(),
        }
    }
}
pub(crate) struct Captured {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Observation {
    Exited,
    TimedOut,
    Cancelled,
}

pub(crate) struct Input<'a> {
    socket: Option<UnixStream>,
    remaining: &'a [u8],
}
impl<'a> Input<'a> {
    pub(crate) fn prepare(command: &mut Command, bytes: Option<&'a [u8]>) -> Result<Self> {
        let Some(bytes) = bytes else {
            command.stdin(Stdio::null());
            return Ok(Self {
                socket: None,
                remaining: &[],
            });
        };
        let (sender, receiver) = UnixStream::pair()?;
        sender.set_nonblocking(true)?;
        command.stdin(Stdio::from(OwnedFd::from(receiver)));
        Ok(Self {
            socket: (!bytes.is_empty()).then_some(sender),
            remaining: bytes,
        })
    }
    fn poll(&self) -> libc::pollfd {
        descriptor(
            self.socket.as_ref().map_or(-1, AsRawFd::as_raw_fd),
            libc::POLLOUT,
        )
    }
    fn write(&mut self) -> Result<()> {
        let Some(socket) = &self.socket else {
            return Ok(());
        };
        let bytes = &self.remaining[..self.remaining.len().min(QUANTUM)];
        // SAFETY: both slices and socket remain borrowed; SIGPIPE is suppressed.
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
    capture: Capture,
    eof: bool,
    total: u64,
}
impl Stream {
    fn new(fd: OwnedFd, policy: CapturePolicy) -> Result<Self> {
        make_nonblocking(&fd)?;
        Ok(Self {
            file: fd.into(),
            capture: Capture::new(policy)?,
            eof: false,
            total: 0,
        })
    }
    fn poll(&self) -> libc::pollfd {
        descriptor(
            if self.eof { -1 } else { self.file.as_raw_fd() },
            libc::POLLIN,
        )
    }
    fn drain(&mut self, policy: CapturePolicy, other: usize) -> Result<usize> {
        let mut buffer = [0; 4096];
        let mut read = 0;
        while !self.eof && read < QUANTUM {
            match self.file.read(&mut buffer) {
                Ok(0) => self.eof = true,
                Ok(count) => {
                    self.total = self.total.saturating_add(count as u64);
                    if let CapturePolicy::StrictCombined(limit) = policy {
                        ensure!(
                            self.capture.strict_len() + other + count <= limit,
                            "probe output exceeds {limit} bytes"
                        );
                    }
                    self.capture.push(&buffer[..count]);
                    read += count;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) =>
                {
                    break
                }
                Err(error) => return Err(error).context("read probe output"),
            }
        }
        Ok(read)
    }
}

pub(crate) struct Monitor<'a> {
    input: Input<'a>,
    out: Stream,
    err: Stream,
    policy: CapturePolicy,
}
impl<'a> Monitor<'a> {
    pub(crate) fn new(
        out: OwnedFd,
        err: OwnedFd,
        input: Input<'a>,
        policy: CapturePolicy,
    ) -> Result<Self> {
        policy.validate()?;
        Ok(Self {
            input,
            out: Stream::new(out, policy)?,
            err: Stream::new(err, policy)?,
            policy,
        })
    }
    fn drain_round(&mut self) -> Result<usize> {
        let out = self.out.drain(self.policy, self.err.capture.strict_len())?;
        let err = self.err.drain(self.policy, self.out.capture.strict_len())?;
        Ok(out + err)
    }
    /// Never kills or reaps. A pinned pidfd is owned by the enclosing supervisor.
    pub(crate) fn observe(
        &mut self,
        pidfd: RawFd,
        deadline: Instant,
        cancel_fd: Option<RawFd>,
        mut cancelled: impl FnMut() -> Result<bool>,
    ) -> Result<Observation> {
        loop {
            if cancelled()? {
                return Ok(Observation::Cancelled);
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(Observation::TimedOut);
            };
            let mut descriptors = [
                self.out.poll(),
                self.err.poll(),
                descriptor(pidfd, libc::POLLIN),
                self.input.poll(),
                descriptor(cancel_fd.unwrap_or(-1), libc::POLLIN),
            ];
            poll(&mut descriptors, remaining)?;
            if cancelled()? {
                return Ok(Observation::Cancelled);
            }
            if descriptors[2].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                return Ok(Observation::Exited);
            }
            self.input.write()?;
            self.drain_round()?;
        }
    }
    pub(crate) fn require_stdin_consumed(&self) -> Result<()> {
        ensure!(
            self.input.remaining.is_empty(),
            "probe exited before accepting all stdin"
        );
        Ok(())
    }
    /// Existing probes drain currently available bytes after group cleanup.
    pub(crate) fn drain_available(&mut self, timeout: Duration) -> Result<()> {
        let deadline = deadline(timeout)?;
        while self.drain_round()? > 0 {
            ensure!(Instant::now() < deadline, TimedOut(timeout));
        }
        Ok(())
    }
    /// Confined execution calls this only after cgroup teardown and process stop.
    pub(crate) fn drain_to_eof(&mut self, timeout: Duration) -> Result<()> {
        let deadline = deadline(timeout)?;
        loop {
            self.drain_round()?;
            if self.out.eof && self.err.eof {
                return Ok(());
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(TimedOut(timeout))?;
            poll(&mut [self.out.poll(), self.err.poll()], remaining)?;
        }
    }
    pub(crate) fn finish(self) -> Captured {
        Captured {
            stdout: self.out.capture.finish(),
            stderr: self.err.capture.finish(),
            stdout_bytes: self.out.total,
            stderr_bytes: self.err.total,
        }
    }
}
pub(crate) fn deadline(timeout: Duration) -> Result<Instant> {
    ensure!(!timeout.is_zero(), "probe timeout must be positive");
    Instant::now()
        .checked_add(timeout)
        .context("probe timeout is too large")
}
pub(crate) fn observation_error(observation: Observation, timeout: Duration) -> Result<()> {
    match observation {
        Observation::Exited => Ok(()),
        Observation::TimedOut => Err(TimedOut(timeout).into()),
        Observation::Cancelled => Err(Interrupted.into()),
    }
}
fn descriptor(fd: RawFd, events: i16) -> libc::pollfd {
    libc::pollfd {
        fd,
        events,
        revents: 0,
    }
}
fn poll(descriptors: &mut [libc::pollfd], remaining: Duration) -> Result<()> {
    let millis = remaining.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
    // SAFETY: poll mutates exactly the initialized descriptor slice.
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
