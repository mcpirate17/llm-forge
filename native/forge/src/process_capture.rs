//! Bounded asynchronous log capture shared by native process supervisors.
use super::process_io::Descriptor;
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::os::fd::OwnedFd;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;

#[derive(Clone, Debug, Default, Serialize)]
pub struct LogSummary {
    pub retained_bytes: u64,
    pub discarded_bytes: u64,
}

/// Shared sink for pipe capture and PTY replay. The limit applies to retained
/// bytes; excess output is counted and drained without growing memory or disk.
pub struct LogWriter {
    output: tokio::io::BufWriter<tokio::fs::File>,
    summary: LogSummary,
    limit: u64,
}

impl LogWriter {
    pub fn new(output: std::fs::File, limit: u64) -> Result<Self> {
        ensure!(limit <= 64 * 1024 * 1024, "log limit exceeds 64 MiB");
        Ok(Self {
            output: tokio::io::BufWriter::with_capacity(
                64 * 1024,
                tokio::fs::File::from_std(output),
            ),
            summary: LogSummary::default(),
            limit,
        })
    }

    pub async fn append(&mut self, bytes: &[u8]) -> Result<()> {
        let keep = self
            .limit
            .saturating_sub(self.summary.retained_bytes)
            .min(bytes.len() as u64) as usize;
        self.output.write_all(&bytes[..keep]).await?;
        self.summary.retained_bytes += keep as u64;
        self.summary.discarded_bytes = self
            .summary
            .discarded_bytes
            .saturating_add((bytes.len() - keep) as u64);
        Ok(())
    }

    pub fn summary(&self) -> &LogSummary {
        &self.summary
    }

    pub async fn finish(mut self) -> Result<LogSummary> {
        self.output.flush().await?;
        self.output.get_ref().sync_all().await?;
        Ok(self.summary)
    }
}

pub struct Capture {
    completion: JoinHandle<Result<LogSummary>>,
    stop: Option<oneshot::Sender<()>>,
}

impl Capture {
    pub fn start(input: OwnedFd, output: std::fs::File, limit: u64) -> Result<Self> {
        let input = Descriptor::new(input)?;
        let output = LogWriter::new(output, limit)?;
        let (send, receive) = oneshot::channel();
        Ok(Self {
            completion: tokio::spawn(drain(input, output, receive)),
            stop: Some(send),
        })
    }

    pub async fn finish(mut self) -> Result<LogSummary> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        (&mut self.completion)
            .await
            .context("output capture task failed")?
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.completion.abort();
    }
}

async fn drain(
    input: Descriptor,
    mut output: LogWriter,
    mut stop: oneshot::Receiver<()>,
) -> Result<LogSummary> {
    let mut buffer = [0u8; 8192];
    let mut stopping = false;
    let mut deadline = Instant::now();
    loop {
        let count = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline), if stopping => break,
            _ = &mut stop, if !stopping => {
                stopping = true;
                deadline = Instant::now() + Duration::from_millis(200);
                continue;
            }
            count = input.read(&mut buffer) => match count {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                other => other?,
            },
        };
        if count == 0 {
            break;
        }
        output.append(&buffer[..count]).await?;
        // A permanently readable flood must not starve deadlines or other tasks.
        tokio::task::yield_now().await;
    }
    output.finish().await
}
