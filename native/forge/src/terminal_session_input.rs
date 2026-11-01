use super::*;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use tokio::time::Instant;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WriteState {
    Pending,
    Complete,
    TimedOut,
    Closed,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct WriteResult {
    pub sequence: u64,
    pub accepted_bytes: usize,
    pub state: WriteState,
    pub next_sequence: u64,
}

pub(super) struct Pending {
    pub bytes: Vec<u8>,
    pub accepted: usize,
    pub deadline: Instant,
    sequence: u64,
    hash: [u8; 32],
    reply: oneshot::Sender<Result<WriteResult>>,
}

pub(super) struct Input {
    pub pending: Option<Pending>,
    pub next: u64,
    history: VecDeque<([u8; 32], WriteResult)>,
}

impl Input {
    pub fn new() -> Self {
        Self {
            pending: None,
            next: 1,
            history: VecDeque::with_capacity(64),
        }
    }

    pub fn accept(
        &mut self,
        sequence: u64,
        bytes: Vec<u8>,
        wait: Duration,
        reply: oneshot::Sender<Result<WriteResult>>,
        running: bool,
    ) {
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        match self.lookup(sequence, hash) {
            Err(error) => {
                let _ = reply.send(Err(error));
            }
            Ok(Some(result)) => {
                let _ = reply.send(Ok(result));
            }
            Ok(None) if !running => {
                let _ = reply.send(Err(anyhow::anyhow!("terminal is complete")));
            }
            Ok(None) => {
                self.pending = Some(Pending {
                    bytes,
                    accepted: 0,
                    deadline: Instant::now() + wait,
                    sequence,
                    hash,
                    reply,
                })
            }
        }
    }

    fn lookup(&self, sequence: u64, hash: [u8; 32]) -> Result<Option<WriteResult>> {
        ensure!(
            sequence > 0 && sequence < u64::MAX,
            "write sequence out of range"
        );
        ensure!(
            sequence <= self.next,
            "expected write sequence {}",
            self.next
        );
        if sequence < self.next {
            let (previous_hash, result) = self
                .history
                .iter()
                .find(|(_, r)| r.sequence == sequence)
                .context("write sequence history expired; delivery must not be retried")?;
            ensure!(
                *previous_hash == hash,
                "write sequence reused with another payload"
            );
            return Ok(Some(*result));
        }
        if let Some(pending) = &self.pending {
            ensure!(
                pending.hash == hash,
                "pending write sequence reused with another payload"
            );
            return Ok(Some(WriteResult {
                sequence,
                accepted_bytes: pending.accepted,
                state: WriteState::Pending,
                next_sequence: self.next,
            }));
        }
        Ok(None)
    }

    pub fn finish(&mut self, state: WriteState) {
        if let Some(pending) = self.pending.take() {
            self.next += 1; // lookup rejects u64::MAX; the following request fails closed.
            let result = WriteResult {
                sequence: pending.sequence,
                accepted_bytes: pending.accepted,
                state,
                next_sequence: self.next,
            };
            if self.history.len() == 64 {
                self.history.pop_front();
            }
            self.history.push_back((pending.hash, result));
            let _ = pending.reply.send(Ok(result));
        }
    }
}
