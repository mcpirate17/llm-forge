use super::*;
use crate::process_io::ChildExit;
use crate::pty::Terminal;
use crate::sandbox::ConfinedProcess;
use input::{Input, WriteState};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use tokio::time::Instant;

pub(super) struct Actor {
    terminal: Terminal,
    process: ConfinedProcess,
    group: Cgroup,
    replay: ReplayBuffer,
    log: Option<LogWriter>,
    requests: mpsc::Receiver<Request>,
    updates: watch::Sender<Status>,
    input: Input,
    config: Config,
    close_reply: Option<oneshot::Sender<Result<Status>>>,
    completed: Option<Receipt>,
}

impl Actor {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        terminal: Terminal,
        process: ConfinedProcess,
        group: Cgroup,
        replay: ReplayBuffer,
        log: LogWriter,
        requests: mpsc::Receiver<Request>,
        updates: watch::Sender<Status>,
        config: Config,
    ) -> Self {
        Self {
            terminal,
            process,
            group,
            replay,
            log: Some(log),
            requests,
            updates,
            input: Input::new(),
            config,
            close_reply: None,
            completed: None,
        }
    }

    pub async fn run(mut self) -> Result<Receipt> {
        let outcome = self.active().await;
        let receipt = self.complete(outcome).await;
        self.completed = Some(receipt.clone());
        if let Some(reply) = self.close_reply.take() {
            let _ = reply.send(Ok(receipt.status));
        }
        let until = Instant::now() + self.config.completed_retention;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(until) => break,
                request = self.requests.recv() => match request {
                    Some(request) => { self.handle(request, false)?; }
                    None => break,
                }
            }
        }
        Ok(receipt)
    }

    async fn active(&mut self) -> Result<(Phase, Option<ExitStatus>)> {
        let exit = ChildExit::new(self.process.child())?;
        let until = Instant::now() + self.config.timeout;
        let mut buffer = [0u8; 8192];
        let mut eof = false;
        loop {
            let write_deadline = self.input.pending.as_ref().map_or(until, |p| p.deadline);
            tokio::select! {
                _ = tokio::time::sleep_until(until) => return Ok((Phase::TimedOut, None)),
                status = exit.wait() => return Ok((Phase::Exited, Some(status?))),
                _ = tokio::time::sleep_until(write_deadline), if self.input.pending.is_some() => {
                    self.input.finish(WriteState::TimedOut); self.publish();
                }
                count = async {
                    match &self.input.pending { Some(p) => self.terminal.write(&p.bytes[p.accepted..]).await,
                        None => std::future::pending().await }
                } => {
                    let count = match count {
                        Ok(count) => count,
                        Err(error) if error.raw_os_error() == Some(libc::EIO) => {
                            self.input.finish(WriteState::Closed); self.publish(); continue;
                        }
                        Err(error) => return Err(error.into()),
                    };
                    ensure!(count > 0, "terminal accepted an empty write");
                    let pending = self.input.pending.as_mut().context("pending terminal input disappeared")?;
                    pending.accepted += count;
                    if pending.accepted == pending.bytes.len() { self.input.finish(WriteState::Complete); }
                    self.publish();
                }
                count = self.terminal.read(&mut buffer), if !eof => {
                    let count = count?;
                    eof = count == 0;
                    if !eof { self.output(&buffer[..count]).await?; }
                }
                request = self.requests.recv() => match request {
                    Some(request) => { if self.handle(request, true)? { return Ok((Phase::Closed, None)); } }
                    None => return Ok((Phase::Closed, None)),
                }
            }
        }
    }

    fn handle(&mut self, request: Request, running: bool) -> Result<bool> {
        let status = *self.updates.borrow();
        match request {
            Request::Receipt { reply } => {
                let _ = reply.send(Ok(self.completed.clone()));
            }
            Request::Read {
                cursor,
                limit,
                reply,
            } => {
                let _ = reply.send(self.replay.read(cursor, limit));
            }
            Request::Write {
                sequence,
                bytes,
                wait,
                reply,
            } => self.input.accept(sequence, bytes, wait, reply, running),
            Request::Resize {
                rows,
                columns,
                reply,
            } => {
                let result = if running {
                    self.terminal
                        .resize(rows, columns)
                        .map(|()| status)
                        .map_err(Into::into)
                } else {
                    Err(anyhow::anyhow!("terminal is complete"))
                };
                let _ = reply.send(result);
            }
            Request::Interrupt { reply } => {
                let result = if running {
                    self.terminal
                        .interrupt()
                        .map(|()| status)
                        .map_err(Into::into)
                } else {
                    Err(anyhow::anyhow!("terminal is complete"))
                };
                let _ = reply.send(result);
            }
            Request::Close { reply } => {
                if running {
                    self.close_reply = Some(reply);
                    return Ok(true);
                }
                let _ = reply.send(Ok(status));
            }
        }
        Ok(false)
    }

    async fn output(&mut self, bytes: &[u8]) -> Result<()> {
        self.replay.append(bytes)?;
        self.log
            .as_mut()
            .context("terminal log already finalized")?
            .append(bytes)
            .await?;
        self.publish();
        Ok(())
    }

    fn publish(&self) {
        self.updates.send_modify(|s| {
            s.retained_start = self.replay.retained_start();
            s.output_end = self.replay.end();
            s.next_write_sequence = self.input.next;
        });
    }

    async fn complete(&mut self, outcome: Result<(Phase, Option<ExitStatus>)>) -> Receipt {
        let (mut phase, observed, mut errors) = match outcome {
            Ok((phase, status)) => (phase, status, Vec::new()),
            Err(error) => (Phase::Failed, None, vec![format!("execution: {error:#}")]),
        };
        self.input.finish(WriteState::Closed);
        if let Err(error) = self.group.finish(Duration::from_secs(2)).await {
            errors.push(format!("cgroup cleanup: {error:#}"));
        }
        let status = match self.process.stop().await {
            Ok(status) => observed.or(Some(status)),
            Err(error) => {
                errors.push(format!("process cleanup: {error:#}"));
                observed
            }
        };
        if let Err(error) = self.drain().await {
            errors.push(format!("output drain: {error:#}"));
        }
        let log = match self.log.take() {
            Some(log) => match log.finish().await {
                Ok(summary) => Some(summary),
                Err(error) => {
                    errors.push(format!("log durability: {error:#}"));
                    None
                }
            },
            None => {
                errors.push("missing terminal log".into());
                None
            }
        };
        if !errors.is_empty() {
            phase = Phase::Failed;
        }
        self.publish();
        self.updates.send_modify(|s| {
            s.phase = phase;
            s.exit_code = status.and_then(|s| s.code());
            s.signal = status.and_then(|s| s.signal());
        });
        Receipt {
            status: *self.updates.borrow(),
            log,
            resources: self.group.report().clone(),
            error: (!errors.is_empty()).then(|| errors.join("; ")),
        }
    }

    async fn drain(&mut self) -> Result<()> {
        let until = Instant::now() + Duration::from_millis(200);
        let mut buffer = [0u8; 8192];
        loop {
            let count = tokio::select! {
                _ = tokio::time::sleep_until(until) => anyhow::bail!("terminal remained open after cleanup"),
                count = self.terminal.read(&mut buffer) => count?,
            };
            if count == 0 {
                return Ok(());
            }
            self.output(&buffer[..count]).await?;
        }
    }
}
