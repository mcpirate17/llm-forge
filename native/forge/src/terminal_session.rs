//! Bounded persistent terminal actor. Clients reconnect through independent byte
//! cursors; sequenced writes cannot be accidentally delivered twice on retry.
#[path = "terminal_session_input.rs"]
mod input;
#[path = "terminal_session_worker.rs"]
mod worker;
use crate::cgroup::{Cgroup, Report};
use crate::process_capture::{LogSummary, LogWriter};
use crate::sandbox::CommandPlan;
use crate::terminal_buffer::{Chunk, ReplayBuffer};
use anyhow::{ensure, Context, Result};
pub use input::{WriteResult, WriteState};
use serde::Serialize;
use std::fs::File;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Running,
    Exited,
    TimedOut,
    Closed,
    Failed,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Status {
    pub pid: u32,
    pub phase: Phase,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub retained_start: u64,
    pub output_end: u64,
    pub next_write_sequence: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Receipt {
    pub status: Status,
    pub log: Option<LogSummary>,
    pub resources: Report,
    pub error: Option<String>,
}

pub struct Config {
    pub rows: u16,
    pub columns: u16,
    pub timeout: Duration,
    pub replay_bytes: usize,
    pub log_bytes: u64,
    pub completed_retention: Duration,
}

#[derive(Clone)]
pub struct Session {
    requests: mpsc::Sender<Request>,
    view: watch::Receiver<Status>,
}

enum Request {
    Receipt {
        reply: oneshot::Sender<Result<Option<Receipt>>>,
    },
    Read {
        cursor: u64,
        limit: usize,
        reply: oneshot::Sender<Result<Chunk>>,
    },
    Write {
        sequence: u64,
        bytes: Vec<u8>,
        wait: Duration,
        reply: oneshot::Sender<Result<WriteResult>>,
    },
    Resize {
        rows: u16,
        columns: u16,
        reply: oneshot::Sender<Result<Status>>,
    },
    Interrupt {
        reply: oneshot::Sender<Result<Status>>,
    },
    Close {
        reply: oneshot::Sender<Result<Status>>,
    },
}

impl Session {
    pub fn status(&self) -> Status {
        *self.view.borrow()
    }

    pub async fn wait_complete(&self) -> Result<Status> {
        let mut view = self.view.clone();
        let status = *view
            .wait_for(|s| s.phase != Phase::Running)
            .await
            .context("terminal actor stopped before completion")?;
        Ok(status)
    }

    pub async fn wait_output(&self, cursor: u64, timeout: Duration) -> Result<Status> {
        ensure!(
            cursor <= self.status().output_end,
            "output cursor lies beyond stream"
        );
        ensure!(
            !timeout.is_zero() && timeout <= Duration::from_secs(30),
            "output wait must be (0,30s]"
        );
        let mut view = self.view.clone();
        let waited = tokio::time::timeout(
            timeout,
            view.wait_for(|s| s.output_end > cursor || s.phase != Phase::Running),
        )
        .await;
        match waited {
            Ok(status) => Ok(*status.context("terminal output closed")?),
            Err(_) => Ok(self.status()),
        }
    }

    pub async fn receipt(&self) -> Result<Option<Receipt>> {
        let (reply, result) = oneshot::channel();
        self.send(Request::Receipt { reply })?;
        result.await.context("terminal receipt reader stopped")?
    }

    pub async fn read(&self, cursor: u64, limit: usize) -> Result<Chunk> {
        let (reply, result) = oneshot::channel();
        self.send(Request::Read {
            cursor,
            limit,
            reply,
        })?;
        result.await.context("terminal reader stopped")?
    }

    /// Sequence begins at one. A repeated sequence with the same payload returns
    /// its original accepted prefix, never writes it again, and never resets its
    /// deadline. Expired history is an error; callers must not invent a new ID to retry.
    pub async fn write(
        &self,
        sequence: u64,
        bytes: Vec<u8>,
        wait: Duration,
    ) -> Result<WriteResult> {
        ensure!(
            !bytes.is_empty() && bytes.len() <= 65536,
            "terminal write must be 1..65536 bytes"
        );
        ensure!(
            !wait.is_zero() && wait <= Duration::from_secs(2),
            "write wait must be (0,2s]"
        );
        let (reply, result) = oneshot::channel();
        self.send(Request::Write {
            sequence,
            bytes,
            wait,
            reply,
        })?;
        result.await.context("terminal writer stopped")?
    }

    pub async fn resize(&self, rows: u16, columns: u16) -> Result<Status> {
        let (reply, result) = oneshot::channel();
        self.send(Request::Resize {
            rows,
            columns,
            reply,
        })?;
        result.await.context("terminal resize stopped")?
    }

    pub async fn interrupt(&self) -> Result<Status> {
        let (reply, result) = oneshot::channel();
        self.send(Request::Interrupt { reply })?;
        result.await.context("terminal interrupt stopped")?
    }

    pub async fn close(&self) -> Result<Status> {
        if self.status().phase != Phase::Running {
            return Ok(self.status());
        }
        let (reply, result) = oneshot::channel();
        self.send(Request::Close { reply })?;
        result.await.context("terminal close stopped")?
    }

    fn send(&self, request: Request) -> Result<()> {
        self.requests
            .try_send(request)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => {
                    anyhow::anyhow!("terminal request queue is full")
                }
                mpsc::error::TrySendError::Closed(_) => {
                    anyhow::anyhow!("terminal replay retention ended")
                }
            })
    }
}

pub fn spawn(
    mut plan: CommandPlan,
    mut group: Cgroup,
    output: File,
    config: Config,
) -> Result<(Session, JoinHandle<Result<Receipt>>)> {
    ensure!(
        !config.timeout.is_zero() && config.timeout <= Duration::from_secs(86400),
        "terminal timeout must be (0,86400s]"
    );
    ensure!(
        !config.completed_retention.is_zero()
            && config.completed_retention <= Duration::from_secs(3600),
        "terminal replay retention must be (0,3600s]"
    );
    let replay = ReplayBuffer::new(config.replay_bytes)?;
    let log = LogWriter::new(output, config.log_bytes)?;
    group.configure_command(plan.command())?;
    let (terminal, process) = plan.spawn_terminal(config.rows, config.columns)?;
    let status = Status {
        pid: process.id(),
        phase: Phase::Running,
        exit_code: None,
        signal: None,
        retained_start: 0,
        output_end: 0,
        next_write_sequence: 1,
    };
    let (updates, view) = watch::channel(status);
    let (send, receive) = mpsc::channel(16);
    let actor = worker::Actor::new(
        terminal, process, group, replay, log, receive, updates, config,
    );
    Ok((
        Session {
            requests: send,
            view,
        },
        tokio::spawn(actor.run()),
    ))
}
