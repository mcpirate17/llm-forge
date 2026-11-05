//! Persistent bounded byte streams driven by the existing execution owner.
use super::*;
use std::{
    fs::File,
    io::{self, Read},
    os::fd::{AsRawFd, OwnedFd, RawFd},
    os::unix::net::UnixStream,
};
use tokio::sync::{mpsc, oneshot};

const CHUNK: usize = 8192;
#[derive(Clone, Copy)]
pub struct DuplexLimits {
    pub message_bytes: usize,
    pub input_bytes: usize,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    /// Capacity counts raw chunks and EOF events; saturation fails closed.
    pub output_chunks: usize,
}
impl DuplexLimits {
    fn validate(self) -> Result<()> {
        ensure!(
            (1..=1024 * 1024).contains(&self.message_bytes),
            "duplex message bound invalid"
        );
        ensure!(
            (self.message_bytes..=16 * 1024 * 1024).contains(&self.input_bytes),
            "duplex input bound invalid"
        );
        ensure!(
            (1..=16 * 1024 * 1024).contains(&self.stdout_bytes),
            "duplex stdout bound invalid"
        );
        ensure!(
            (1..=1024 * 1024).contains(&self.stderr_bytes),
            "duplex stderr bound invalid"
        );
        ensure!(
            (1..=64).contains(&self.output_chunks),
            "duplex output queue bound invalid"
        );
        Ok(())
    }
}
#[derive(Debug)]
pub enum DuplexEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    StdoutEof,
    StderrEof,
}
enum Command {
    Send(Vec<u8>, oneshot::Sender<Result<()>>),
    Close(oneshot::Sender<Result<()>>),
}
struct Guard {
    cancellation: Arc<Cancellation>,
    complete: bool,
}
impl Drop for Guard {
    fn drop(&mut self) {
        if !self.complete {
            let _ = self.cancellation.cancel();
        }
    }
}
pub struct Duplex {
    execution: Option<Execution>,
    commands: mpsc::Sender<Command>,
    events: mpsc::Receiver<DuplexEvent>,
    wake: UnixStream,
    maximum: usize,
    stdout_eof: bool,
    stderr_eof: bool,
    stream_closed: bool,
}
impl Duplex {
    fn guard(&self) -> Guard {
        Guard {
            cancellation: self
                .execution
                .as_ref()
                .expect("duplex owner")
                .cancellation(),
            complete: false,
        }
    }
    pub fn completion(&self) -> Completion {
        self.execution.as_ref().expect("duplex owner").completion()
    }
    pub fn cancellation(&self) -> Arc<Cancellation> {
        self.execution
            .as_ref()
            .expect("duplex owner")
            .cancellation()
    }
    /// Return only after all bytes were accepted. Dropping this future cancels the owner.
    pub async fn send(&mut self, bytes: Vec<u8>) -> Result<()> {
        let mut guard = self.guard();
        ensure!(
            !bytes.is_empty() && bytes.len() <= self.maximum,
            "duplex message exceeds bound"
        );
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Send(bytes, tx))
            .await
            .context("duplex input closed")?;
        wake(&self.wake)?;
        rx.await.context("duplex owner stopped during write")??;
        guard.complete = true;
        Ok(())
    }
    /// Shut down only stdin after prior sends complete; output remains readable.
    pub async fn close_stdin(&mut self) -> Result<()> {
        let mut guard = self.guard();
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(Command::Close(tx))
            .await
            .context("duplex input closed")?;
        wake(&self.wake)?;
        rx.await.context("duplex owner stopped during close")??;
        guard.complete = true;
        Ok(())
    }
    /// One consumer receives raw stream chunks. EOF is not cleanup evidence.
    pub async fn recv(&mut self) -> Result<Option<DuplexEvent>> {
        let mut guard = self.guard();
        let event = self.events.recv().await;
        self.note_event(event.as_ref());
        guard.complete = true;
        Ok(event)
    }
    fn note_event(&mut self, event: Option<&DuplexEvent>) {
        match event {
            Some(DuplexEvent::StdoutEof) => self.stdout_eof = true,
            Some(DuplexEvent::StderrEof) => self.stderr_eof = true,
            None => self.stream_closed = true,
            _ => (),
        }
    }
    /// Return only an event already queued by the owner; an empty queue does not
    /// prove the peer will not send unsolicited bytes after this observation.
    pub fn try_recv_event(&mut self) -> Result<Option<DuplexEvent>> {
        match self.events.try_recv() {
            Ok(event) => {
                self.note_event(Some(&event));
                Ok(Some(event))
            }
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => {
                self.note_event(None);
                anyhow::bail!("duplex event channel closed")
            }
        }
    }
    /// Drain and observe both stream EOF events before waiting. An early wait
    /// cancels the endpoint rather than silently discarding buffered output.
    pub async fn wait(mut self) -> Result<Report> {
        ensure!(
            self.stream_closed || (self.stdout_eof && self.stderr_eof),
            "duplex output must be drained to both EOF events before wait"
        );
        self.execution
            .take()
            .context("duplex owner unavailable")?
            .wait()
            .await
    }
}
fn wake(socket: &UnixStream) -> Result<()> {
    let byte = [1u8];
    let written = unsafe {
        libc::send(
            socket.as_raw_fd(),
            byte.as_ptr().cast(),
            1,
            libc::MSG_NOSIGNAL,
        )
    };
    if written == 1 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        return Ok(());
    }
    Err(error).context("wake duplex owner")
}
#[derive(Debug)]
struct ClientGone;
impl std::fmt::Display for ClientGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("duplex client departed")
    }
}
impl std::error::Error for ClientGone {}

struct Pending {
    bytes: Vec<u8>,
    offset: usize,
    ack: oneshot::Sender<Result<()>>,
}
pub(super) struct Seed {
    input: Option<UnixStream>,
    wake: UnixStream,
    commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<DuplexEvent>,
    limits: DuplexLimits,
}
pub(super) fn prepare(
    plan: &mut CommandPlan,
    limits: DuplexLimits,
) -> Result<(Seed, DuplexClient)> {
    limits.validate()?;
    let (input, child) = UnixStream::pair()?;
    input.set_nonblocking(true)?;
    plan.command().stdin(Stdio::from(OwnedFd::from(child)));
    let (wake_client, wake_owner) = UnixStream::pair()?;
    wake_client.set_nonblocking(true)?;
    wake_owner.set_nonblocking(true)?;
    let (commands, receiver) = mpsc::channel(1);
    let (events, output) = mpsc::channel(limits.output_chunks);
    Ok((
        Seed {
            input: Some(input),
            wake: wake_owner,
            commands: receiver,
            events,
            limits,
        },
        DuplexClient {
            commands,
            events: output,
            wake: wake_client,
            maximum: limits.message_bytes,
        },
    ))
}
pub(super) struct DuplexClient {
    commands: mpsc::Sender<Command>,
    events: mpsc::Receiver<DuplexEvent>,
    wake: UnixStream,
    maximum: usize,
}
impl DuplexClient {
    pub(super) fn bind(self, execution: Execution) -> Duplex {
        Duplex {
            execution: Some(execution),
            commands: self.commands,
            events: self.events,
            wake: self.wake,
            maximum: self.maximum,
            stdout_eof: false,
            stderr_eof: false,
            stream_closed: false,
        }
    }
}
pub(super) struct Driver {
    seed: Seed,
    out: Option<File>,
    err: Option<File>,
    pending: Option<Pending>,
    input_bytes: usize,
    stdout_bytes: usize,
    stderr_bytes: usize,
}
impl Driver {
    pub(super) fn new(seed: Seed, out: OwnedFd, err: OwnedFd) -> Result<Self> {
        let out = File::from(out);
        let err = File::from(err);
        crate::process_io::make_nonblocking(&out)?;
        crate::process_io::make_nonblocking(&err)?;
        Ok(Self {
            seed,
            out: Some(out),
            err: Some(err),
            pending: None,
            input_bytes: 0,
            stdout_bytes: 0,
            stderr_bytes: 0,
        })
    }
    fn commands(&mut self) -> Result<()> {
        let mut bytes = [0; 128];
        loop {
            match (&self.seed.wake).read(&mut bytes) {
                Ok(0) => break,
                Ok(_) => (),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        if self.pending.is_some() {
            return Ok(());
        }
        match self.seed.commands.try_recv() {
            Ok(Command::Send(bytes, ack)) => {
                ensure!(self.seed.input.is_some(), "duplex stdin is closed");
                self.input_bytes = self
                    .input_bytes
                    .checked_add(bytes.len())
                    .context("duplex input overflow")?;
                ensure!(
                    self.input_bytes <= self.seed.limits.input_bytes,
                    "duplex total input exceeds bound"
                );
                self.pending = Some(Pending {
                    bytes,
                    offset: 0,
                    ack,
                });
            }
            Ok(Command::Close(ack)) => {
                self.seed.input.take().context("duplex stdin is closed")?;
                let _ = ack.send(Ok(()));
            }
            Err(mpsc::error::TryRecvError::Empty) => (),
            Err(mpsc::error::TryRecvError::Disconnected) => return Err(ClientGone.into()),
        }
        Ok(())
    }
    fn write(&mut self) -> Result<()> {
        let Some(pending) = self.pending.as_mut() else {
            return Ok(());
        };
        let input = self.seed.input.as_ref().context("duplex input closed")?;
        let bytes = &pending.bytes[pending.offset..];
        let len = bytes.len().min(CHUNK);
        let n = unsafe {
            libc::send(
                input.as_raw_fd(),
                bytes.as_ptr().cast(),
                len,
                libc::MSG_NOSIGNAL,
            )
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Ok(());
            }
            return Err(e).context("duplex write");
        }
        ensure!(n > 0, "duplex stdin wrote zero bytes");
        pending.offset += n as usize;
        if pending.offset == pending.bytes.len() {
            let done = self.pending.take().unwrap();
            let _ = done.ack.send(Ok(()));
        }
        Ok(())
    }
    fn drain_one(
        &mut self,
        stdout: bool,
        discard_after_cleanup: Option<&Cancellation>,
    ) -> Result<()> {
        let stream = if stdout { &mut self.out } else { &mut self.err };
        let Some(file) = stream.as_mut() else {
            return Ok(());
        };
        let mut data = vec![0; CHUNK];
        let n = match file.read(&mut data) {
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(())
            }
            Err(e) => return Err(e.into()),
        };
        let event = if n == 0 {
            stream.take();
            if stdout {
                DuplexEvent::StdoutEof
            } else {
                DuplexEvent::StderrEof
            }
        } else {
            data.truncate(n);
            let (total, maximum) = if stdout {
                (&mut self.stdout_bytes, self.seed.limits.stdout_bytes)
            } else {
                (&mut self.stderr_bytes, self.seed.limits.stderr_bytes)
            };
            *total = total.checked_add(n).context("duplex output overflow")?;
            ensure!(*total <= maximum, "duplex output exceeds bound");
            if stdout {
                DuplexEvent::Stdout(data)
            } else {
                DuplexEvent::Stderr(data)
            }
        };
        match self.seed.events.try_send(event) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_))
                if discard_after_cleanup.is_some_and(Cancellation::is_cancelled) =>
            {
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(ClientGone.into()),
            Err(mpsc::error::TrySendError::Full(_)) => anyhow::bail!("duplex output queue full"),
        }
    }
    pub(super) fn observe(
        &mut self,
        pidfd: RawFd,
        deadline: Instant,
        cancel: &Cancellation,
    ) -> Result<Observation> {
        loop {
            if cancel.is_cancelled() {
                return Ok(Observation::Cancelled);
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(Observation::TimedOut);
            };
            let fds = [
                self.out.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                self.err.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                pidfd,
                self.seed
                    .input
                    .as_ref()
                    .filter(|_| self.pending.is_some())
                    .map_or(-1, AsRawFd::as_raw_fd),
                self.seed.wake.as_raw_fd(),
                cancel.as_raw_fd(),
            ];
            let mut p = [
                readiness::descriptor(fds[0], libc::POLLIN),
                readiness::descriptor(fds[1], libc::POLLIN),
                readiness::descriptor(fds[2], libc::POLLIN),
                readiness::descriptor(fds[3], libc::POLLOUT),
                readiness::descriptor(fds[4], libc::POLLIN),
                readiness::descriptor(fds[5], libc::POLLIN),
            ];
            readiness::poll(&mut p, remaining)?;
            if cancel.is_cancelled() {
                return Ok(Observation::Cancelled);
            }
            if p[2].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
                return Ok(Observation::Exited);
            }
            if let Err(error) = self.commands() {
                if error.is::<ClientGone>() && cancel.is_cancelled() {
                    return Ok(Observation::Cancelled);
                }
                return Err(error);
            }
            self.write()?;
            for stdout in [true, false] {
                if let Err(error) = self.drain_one(stdout, None) {
                    if error.is::<ClientGone>() && cancel.is_cancelled() {
                        return Ok(Observation::Cancelled);
                    }
                    return Err(error);
                }
            }
        }
    }
    pub(super) fn drain(
        &mut self,
        timeout: Duration,
        cancel: &Cancellation,
        cleanup_safe: bool,
    ) -> Result<()> {
        let deadline = readiness::deadline(timeout)?;
        while self.out.is_some() || self.err.is_some() {
            let discard = cleanup_safe.then_some(cancel);
            self.drain_one(true, discard)?;
            self.drain_one(false, discard)?;
            if self.out.is_none() && self.err.is_none() {
                break;
            }
            let left = deadline
                .checked_duration_since(Instant::now())
                .context("duplex drain timed out")?;
            let mut p = [
                readiness::descriptor(
                    self.out.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                    libc::POLLIN,
                ),
                readiness::descriptor(
                    self.err.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                    libc::POLLIN,
                ),
            ];
            readiness::poll(&mut p, left)?;
        }
        Ok(())
    }
    pub(super) fn require_input_complete(&mut self) -> Result<()> {
        ensure!(
            self.pending.is_none(),
            "duplex exited during stdin delivery"
        );
        match self.seed.commands.try_recv() {
            Err(mpsc::error::TryRecvError::Empty | mpsc::error::TryRecvError::Disconnected) => {
                Ok(())
            }
            Ok(_) => anyhow::bail!("duplex exited before accepting an input command"),
        }
    }
    pub(super) fn bytes(&self) -> (u64, u64) {
        (self.stdout_bytes as u64, self.stderr_bytes as u64)
    }
}
