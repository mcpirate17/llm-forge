#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use forge::cgroup::{Cgroup, Limits};
use forge::sandbox::{Policy, Sandbox};
use forge::terminal_session::{self, Config, Phase, Receipt, Session, WriteState};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::task::JoinHandle;

fn launch(
    host: &support::Host,
    program: &Path,
    args: &[&str],
    timeout: Duration,
) -> (Session, JoinHandle<anyhow::Result<Receipt>>) {
    let scratch = host.0.join("scratch");
    fs::create_dir(&scratch).unwrap();
    fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700)).unwrap();
    let sandbox = Sandbox::prepare(Policy {
        read_only: vec![
            "/usr".into(),
            "/lib".into(),
            "/lib64".into(),
            "/etc/ld.so.cache".into(),
            program.to_path_buf(),
        ],
        scratch: scratch.clone(),
        working_directory: scratch,
        environment: BTreeMap::from([("FORGE_RAW_TERMINAL_FIXTURE".into(), "1".into())]),
    })
    .unwrap();
    let mut plan = sandbox.command(program).unwrap();
    plan.command().args(args);
    let parent = PathBuf::from(
        std::env::var_os("FORGE_TEST_CGROUP_PARENT").expect("explicit cgroup delegation required"),
    );
    let group = Cgroup::create(
        &parent,
        Limits {
            cpu_millis: 1000,
            memory_mib: 64,
            processes: 64,
        },
    )
    .unwrap();
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(host.0.join("output.log"))
        .unwrap();
    terminal_session::spawn(
        plan,
        group,
        output,
        Config {
            rows: 24,
            columns: 80,
            timeout,
            replay_bytes: 1024,
            log_bytes: 4096,
            completed_retention: Duration::from_secs(2),
        },
    )
    .unwrap()
}

async fn until(session: &Session, cursor: &mut u64, needle: &str) -> String {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut output = Vec::new();
        loop {
            let chunk = session.read(*cursor, 65536).await.unwrap();
            *cursor = chunk.next;
            output.extend_from_slice(&chunk.bytes);
            let text = String::from_utf8_lossy(&output);
            if text.contains(needle) {
                return text.into_owned();
            }
            assert!(output.len() < 4096, "marker missing: {needle}");
            assert_eq!(session.status().phase, Phase::Running, "{text}");
            session
                .wait_output(*cursor, Duration::from_secs(1))
                .await
                .unwrap();
        }
    })
    .await
    .unwrap()
}

async fn finish(session: Session, task: JoinHandle<anyhow::Result<Receipt>>) -> Receipt {
    drop(session);
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn reconnect_cursors_and_write_retries_preserve_exact_delivery() {
    let host = support::Host::new();
    let (session, task) = launch(
        &host,
        support::task_worker(),
        &["terminal"],
        Duration::from_secs(10),
    );
    let mut cursor = 0;
    until(&session, &mut cursor, "READY").await;
    assert!(session.receipt().await.unwrap().is_none());
    let first = session
        .write(1, b"unique-command\n".to_vec(), Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(first.state, WriteState::Complete);
    until(&session, &mut cursor, "ECHO unique-command").await;
    let retry = session
        .write(1, b"unique-command\n".to_vec(), Duration::from_millis(1))
        .await
        .unwrap();
    assert_eq!(retry.accepted_bytes, first.accepted_bytes);
    assert_eq!(retry.next_sequence, 2);
    assert!(session
        .write(1, b"different".to_vec(), Duration::from_secs(1))
        .await
        .is_err());
    assert!(session
        .write(3, b"skip".to_vec(), Duration::from_secs(1))
        .await
        .is_err());
    let reconnect = session.clone();
    let history = reconnect.read(0, 65536).await.unwrap();
    assert_eq!(
        String::from_utf8_lossy(&history.bytes)
            .matches("ECHO unique-command")
            .count(),
        1
    );
    session.resize(70, 140).await.unwrap();
    assert!(session.resize(0, 80).await.is_err());
    session
        .write(2, b"size\n".to_vec(), Duration::from_secs(1))
        .await
        .unwrap();
    until(&reconnect, &mut cursor, "SIZE 70 140").await;
    session
        .write(3, "café\n".as_bytes().to_vec(), Duration::from_secs(1))
        .await
        .unwrap();
    until(&session, &mut cursor, "ECHO café").await;
    session
        .write(4, b"exit\n".to_vec(), Duration::from_secs(1))
        .await
        .unwrap();
    let final_status = session.wait_complete().await.unwrap();
    assert_eq!(final_status.phase, Phase::Exited);
    assert_eq!(final_status.exit_code, Some(0));
    assert!(session.receipt().await.unwrap().unwrap().error.is_none());
    drop(reconnect);
    let receipt = finish(session, task).await;
    assert!(!receipt.resources.path.exists());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn completed_output_has_bounded_replay_and_durable_log_accounting() {
    let host = support::Host::new();
    let (session, task) = launch(
        &host,
        support::task_worker(),
        &["output"],
        Duration::from_secs(5),
    );
    session.wait_complete().await.unwrap();
    let chunk = session.read(0, 65536).await.unwrap();
    assert!(chunk.evicted);
    assert_eq!(chunk.bytes.len(), 1024);
    assert_eq!(chunk.end, 8192 + b"error detail".len() as u64);
    assert!(chunk.bytes.ends_with(b"error detail"));
    assert!(session.read(chunk.end + 1, 10).await.is_err());
    let receipt = session.receipt().await.unwrap().unwrap();
    assert_eq!(receipt.log.as_ref().unwrap().retained_bytes, 4096);
    assert_eq!(
        receipt.log.as_ref().unwrap().discarded_bytes,
        chunk.end - 4096
    );
    assert_eq!(fs::read(host.0.join("output.log")).unwrap().len(), 4096);
    assert_eq!(session.close().await.unwrap().phase, Phase::Exited);
    finish(session, task).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn deadlines_close_processes_and_replay_retention_is_bounded() {
    let host = support::Host::new();
    let (session, task) = launch(
        &host,
        support::task_worker(),
        &["sleep", "20000"],
        Duration::from_millis(50),
    );
    assert_eq!(
        session.wait_complete().await.unwrap().phase,
        Phase::TimedOut
    );
    let receipt = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(receipt.error.is_none(), "{:?}", receipt.error);
    assert!(!receipt.resources.path.exists());
    assert!(session.read(0, 100).await.is_err());
    assert_eq!(session.close().await.unwrap().phase, Phase::TimedOut);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn input_backpressure_reports_prefixes_and_never_replays_expired_sequences() {
    let host = support::Host::new();
    let (session, task) = launch(
        &host,
        &std::env::current_exe().unwrap(),
        &["--exact", "raw_terminal_fixture", "--nocapture"],
        Duration::from_secs(10),
    );
    until(&session, &mut 0, "RAWREADY").await;
    let payload = vec![b'x'; 65536];
    let mut sequence = 1;
    loop {
        let result = session
            .write(sequence, payload.clone(), Duration::from_millis(20))
            .await
            .unwrap();
        if result.state == WriteState::TimedOut {
            assert!(result.accepted_bytes < payload.len());
            let repeat = session
                .write(sequence, payload.clone(), Duration::from_secs(1))
                .await
                .unwrap();
            assert_eq!(repeat.accepted_bytes, result.accepted_bytes);
            assert_eq!(repeat.state, WriteState::TimedOut);
            break;
        }
        sequence += 1;
        assert!(sequence < 32, "PTY did not exert backpressure within 2 MiB");
    }
    for next in sequence + 1..=sequence + 65 {
        session
            .write(next, b"x".to_vec(), Duration::from_millis(1))
            .await
            .unwrap();
    }
    assert!(session
        .write(sequence, payload, Duration::from_secs(1))
        .await
        .unwrap_err()
        .to_string()
        .contains("history expired"));
    assert_eq!(session.close().await.unwrap().phase, Phase::Closed);
    assert_eq!(session.close().await.unwrap().phase, Phase::Closed);
    let receipt = finish(session, task).await;
    assert!(receipt.error.is_none(), "{:?}", receipt.error);
}

#[test]
fn raw_terminal_fixture() {
    if std::env::var("FORGE_RAW_TERMINAL_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let mut attributes: libc::termios = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::tcgetattr(0, &mut attributes) }, 0);
    unsafe {
        libc::cfmakeraw(&mut attributes);
    }
    assert_eq!(unsafe { libc::tcsetattr(0, libc::TCSANOW, &attributes) }, 0);
    use std::io::Write;
    println!("RAWREADY");
    std::io::stdout().flush().unwrap();
    // The parent closes this task after testing kernel backpressure; hard timeout
    // and owned cgroup cleanup independently bound the fixture's lifetime.
    std::thread::sleep(Duration::from_secs(20));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires explicitly delegated FORGE_TEST_CGROUP_PARENT"]
async fn closed_terminal_input_does_not_turn_a_successful_process_into_failure() {
    let host = support::Host::new();
    let (session, task) = launch(
        &host,
        support::task_worker(),
        &["close-terminal"],
        Duration::from_secs(2),
    );
    until(&session, &mut 0, "STDIO-CLOSING").await;
    let result = session
        .write(1, vec![b'x'; 65536], Duration::from_secs(1))
        .await
        .unwrap();
    assert!(matches!(
        result.state,
        WriteState::Closed | WriteState::Complete
    ));
    let status = session.wait_complete().await.unwrap();
    assert_eq!(status.phase, Phase::Exited);
    assert_eq!(status.exit_code, Some(0));
    let receipt = finish(session, task).await;
    assert!(receipt.error.is_none(), "{:?}", receipt.error);
}
