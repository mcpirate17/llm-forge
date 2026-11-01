#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use forge::{process_io::ChildExit, pty::Terminal};
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::time::Duration;

fn command() -> Command {
    let mut command = Command::new(support::task_worker());
    command.arg("terminal");
    command
}

async fn write(terminal: &Terminal, text: &str) {
    let mut offset = 0;
    while offset < text.len() {
        let count = tokio::time::timeout(
            Duration::from_secs(2),
            terminal.write(&text.as_bytes()[offset..]),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(count > 0);
        offset += count;
    }
}

async fn read_until(terminal: &Terminal, needle: &str) -> String {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 512];
        loop {
            let output = String::from_utf8_lossy(&bytes);
            if output.contains(needle) {
                return output.into_owned();
            }
            let count = terminal.read(&mut buffer).await.unwrap();
            assert!(count > 0, "terminal closed before {needle:?}: {output}");
            bytes.extend_from_slice(&buffer[..count]);
            assert!(bytes.len() < 64 * 1024);
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn terminal_has_job_control_input_resize_and_eof() {
    let (terminal, mut group) = Terminal::spawn(command(), 24, 80).unwrap();
    let exited = ChildExit::new(group.child()).unwrap();
    read_until(&terminal, "READY").await;
    write(&terminal, "size\n").await;
    read_until(&terminal, "SIZE 24 80").await;
    terminal.resize(60, 140).unwrap();
    write(&terminal, "size\n").await;
    read_until(&terminal, "SIZE 60 140").await;
    write(&terminal, "café\n").await;
    read_until(&terminal, "ECHO café").await;
    write(&terminal, "exit\n").await;
    assert!(tokio::time::timeout(Duration::from_secs(2), exited.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    assert!(group.stop().unwrap().success());
    let mut output = [0u8; 512];
    tokio::time::timeout(Duration::from_secs(2), async {
        while terminal.read(&mut output).await.unwrap() != 0 {}
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn interrupt_targets_the_terminal_foreground_job() {
    let (terminal, mut group) = Terminal::spawn(command(), 24, 80).unwrap();
    let exited = ChildExit::new(group.child()).unwrap();
    read_until(&terminal, "READY").await;
    terminal.interrupt().unwrap();
    let status = tokio::time::timeout(Duration::from_secs(2), exited.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.signal(), Some(libc::SIGINT));
    assert_eq!(group.stop().unwrap().signal(), Some(libc::SIGINT));
}

#[tokio::test(flavor = "current_thread")]
async fn invalid_dimensions_are_rejected_before_launch_and_cleanup_is_idempotent() {
    for (rows, columns) in [(0, 80), (24, 0), (1001, 80), (24, 1001)] {
        assert!(Terminal::spawn(command(), rows, columns).is_err());
    }
    let (terminal, mut group) = Terminal::spawn(command(), 1, 1000).unwrap();
    read_until(&terminal, "READY").await;
    assert!(terminal.resize(0, 80).is_err());
    assert_eq!(group.stop().unwrap().signal(), Some(libc::SIGKILL));
    assert_eq!(group.stop().unwrap().signal(), Some(libc::SIGKILL));
}

#[tokio::test(flavor = "current_thread")]
async fn many_terminals_share_one_reactor_without_crossed_streams() {
    let worker = support::task_worker();
    let mut sessions = tokio::task::JoinSet::new();
    for id in 0..32 {
        sessions.spawn(async move {
            let mut command = Command::new(worker);
            command.arg("terminal");
            let (terminal, mut group) = Terminal::spawn(command, 24, 80).unwrap();
            let exited = ChildExit::new(group.child()).unwrap();
            read_until(&terminal, "READY").await;
            write(&terminal, &format!("session-{id}\n")).await;
            read_until(&terminal, &format!("ECHO session-{id}\r\n")).await;
            write(&terminal, "exit\n").await;
            assert!(exited.wait().await.unwrap().success());
            assert!(group.stop().unwrap().success());
        });
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(result) = sessions.join_next().await {
            result.unwrap();
        }
    })
    .await
    .unwrap();
}
