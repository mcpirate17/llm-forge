#![allow(dead_code)]
//! Shared bounded raw MCP stdio client for native integration tests and benches.
use anyhow::{ensure, Context, Result};
use forge::harness::transport;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};

pub struct Mcp {
    process: forge::process_group::ProcessGroup,
    input: Option<forge::process_io::Descriptor>,
    output: BufReader<forge::process_io::Descriptor>,
}
impl Mcp {
    pub fn new(binary: &Path, socket: &Path) -> Self {
        Self::try_new(binary, socket).unwrap()
    }
    pub fn try_new(binary: &Path, socket: &Path) -> Result<Self> {
        let mut command = std::process::Command::new(binary);
        command.args(["harness", "stdio", "--socket"]).arg(socket);
        Self::try_from_command(command)
    }
    pub fn from_command(command: std::process::Command) -> Self {
        Self::try_from_command(command).unwrap()
    }
    pub fn try_from_command(mut command: std::process::Command) -> Result<Self> {
        use std::process::Stdio;
        command
            .env("PATH", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut process = forge::process_group::ProcessGroup::spawn(&mut command)?;
        let input = forge::process_io::Descriptor::new(
            process.take_stdin().context("MCP stdin missing")?.into(),
        )?;
        let output = forge::process_io::Descriptor::new(
            process.take_stdout().context("MCP stdout missing")?.into(),
        )?;
        Ok(Self {
            process,
            input: Some(input),
            output: BufReader::new(output),
        })
    }
    pub fn pid(&self) -> u32 {
        self.process.id()
    }
    pub async fn send(&mut self, value: Value) {
        self.try_send(value).await.unwrap();
    }
    pub async fn try_send(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        ensure!(
            bytes.len() <= transport::MAX_FRAME,
            "MCP request exceeds frame limit"
        );
        bytes.push(b'\n');
        tokio::time::timeout(
            Duration::from_secs(5),
            self.input
                .as_mut()
                .context("MCP stdin closed")?
                .write_all(&bytes),
        )
        .await??;
        Ok(())
    }
    pub async fn receive(&mut self) -> Value {
        self.try_receive().await.unwrap()
    }
    pub async fn try_receive(&mut self) -> Result<Value> {
        let mut frame = Vec::new();
        ensure!(
            tokio::time::timeout(
                Duration::from_secs(5),
                transport::read_frame(&mut self.output, &mut frame, 4 * 1024 * 1024)
            )
            .await??,
            "MCP stdout EOF"
        );
        serde_json::from_slice(&frame).context("invalid MCP JSON reply")
    }
    pub async fn assert_no_frame(&mut self, timeout: Duration) {
        let mut frame = Vec::new();
        assert!(tokio::time::timeout(
            timeout,
            transport::read_frame(&mut self.output, &mut frame, 4 * 1024 * 1024)
        )
        .await
        .is_err());
    }
    pub async fn finish(self) {
        self.try_finish().await.unwrap();
    }
    pub async fn try_finish(mut self) -> Result<()> {
        self.input.take();
        let exit = forge::process_io::ChildExit::new(self.process.child())?;
        ensure!(
            tokio::time::timeout(Duration::from_secs(3), exit.wait())
                .await??
                .success(),
            "MCP adapter failed after EOF"
        );
        ensure!(self.process.stop()?.success(), "MCP adapter cleanup failed");
        Ok(())
    }
}
