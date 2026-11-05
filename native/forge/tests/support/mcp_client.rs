#![allow(dead_code)]
//! Shared bounded raw MCP stdio client for native integration tests.
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
        use std::process::Command;
        let mut command = Command::new(binary);
        command.args(["harness", "stdio", "--socket"]).arg(socket);
        Self::from_command(command)
    }
    pub fn from_command(mut command: std::process::Command) -> Self {
        use std::process::Stdio;
        command
            .env("PATH", "")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut process = forge::process_group::ProcessGroup::spawn(&mut command).unwrap();
        let input =
            forge::process_io::Descriptor::new(process.take_stdin().unwrap().into()).unwrap();
        let output =
            forge::process_io::Descriptor::new(process.take_stdout().unwrap().into()).unwrap();
        Self {
            process,
            input: Some(input),
            output: BufReader::new(output),
        }
    }
    pub async fn send(&mut self, value: Value) {
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        self.input
            .as_mut()
            .unwrap()
            .write_all(&bytes)
            .await
            .unwrap();
    }
    pub async fn receive(&mut self) -> Value {
        let mut frame = Vec::new();
        assert!(tokio::time::timeout(
            Duration::from_secs(5),
            transport::read_frame(&mut self.output, &mut frame, 4 * 1024 * 1024)
        )
        .await
        .unwrap()
        .unwrap());
        serde_json::from_slice(&frame).unwrap()
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

    pub async fn finish(mut self) {
        self.input.take();
        let exit = forge::process_io::ChildExit::new(self.process.child()).unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(3), exit.wait())
            .await
            .unwrap()
            .unwrap()
            .success());
        assert!(self.process.stop().unwrap().success());
    }
}
