#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use forge::process_io::{ChildExit, Descriptor};
use std::os::unix::net::UnixStream;
use std::process::Command;
use std::time::Duration;

#[tokio::test(flavor = "current_thread")]
async fn tokio_stream_traits_preserve_backpressured_bytes_and_eof() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (left, right) = UnixStream::pair().unwrap();
    let mut sender = Descriptor::new(left.into()).unwrap();
    let mut receiver = Descriptor::new(right.into()).unwrap();
    let expected: Vec<_> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
    let send = async {
        sender.write_all(&expected).await.unwrap();
        sender.flush().await.unwrap();
        drop(sender);
    };
    let read = async {
        let mut actual = Vec::new();
        receiver.read_to_end(&mut actual).await.unwrap();
        actual
    };
    let (_, actual) =
        tokio::time::timeout(Duration::from_secs(3), async { tokio::join!(send, read) })
            .await
            .unwrap();
    assert_eq!(actual, expected);
}

#[tokio::test(flavor = "current_thread")]
async fn reactor_handles_backpressure_eof_and_cancelled_read_without_data_loss() {
    let (left, right) = UnixStream::pair().unwrap();
    let sender = Descriptor::new(left.into()).unwrap();
    let receiver = Descriptor::new(right.into()).unwrap();
    let mut pending = [0u8; 8];
    assert!(
        tokio::time::timeout(Duration::from_millis(5), receiver.read(&mut pending))
            .await
            .is_err()
    );
    let payload: Vec<_> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
    let write = async {
        let mut offset = 0;
        while offset < payload.len() {
            let count = sender.write(&payload[offset..]).await.unwrap();
            assert!(count > 0);
            offset += count;
        }
        drop(sender);
    };
    let read = async {
        let mut actual = Vec::new();
        let mut buffer = [0u8; 4096];
        loop {
            let count = receiver.read(&mut buffer).await.unwrap();
            if count == 0 {
                break;
            }
            actual.extend_from_slice(&buffer[..count]);
            assert!(actual.len() <= payload.len());
            tokio::task::yield_now().await;
        }
        actual
    };
    let (_, actual) =
        tokio::time::timeout(Duration::from_secs(3), async { tokio::join!(write, read) })
            .await
            .unwrap();
    assert_eq!(actual, payload);
}

#[tokio::test(flavor = "current_thread")]
async fn pidfd_reports_exit_without_reaping_and_remains_bound_to_the_child() {
    let mut child = Command::new(support::task_worker())
        .args(["exit", "17"])
        .spawn()
        .unwrap();
    let watch = ChildExit::new(&child).unwrap();
    let status = tokio::time::timeout(Duration::from_secs(3), watch.wait())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.code(), Some(17));
    assert_eq!(watch.wait().await.unwrap().code(), Some(17));
    assert_eq!(child.wait().unwrap().code(), Some(17));
    assert!(watch.wait().await.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn child_readiness_wait_is_cancellable_before_exit() {
    let mut child = Command::new(support::task_worker())
        .args(["sleep", "100"])
        .spawn()
        .unwrap();
    let watch = ChildExit::new(&child).unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(5), watch.wait())
        .await
        .is_err());
    let status = tokio::time::timeout(Duration::from_secs(3), watch.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    child.wait().unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn capture_drains_a_flood_with_bounded_storage_and_stops_on_inherited_open_fd() {
    let host = support::Host::new();
    let path = host.0.join("capture.log");
    let (sender, receiver) = UnixStream::pair().unwrap();
    let capture = forge::process_capture::Capture::start(
        receiver.into(),
        std::fs::File::create(&path).unwrap(),
        17,
    )
    .unwrap();
    let sender = Descriptor::new(sender.into()).unwrap();
    let payload = vec![b'x'; 1024 * 1024];
    let mut offset = 0;
    while offset < payload.len() {
        offset += tokio::time::timeout(Duration::from_secs(2), sender.write(&payload[offset..]))
            .await
            .unwrap()
            .unwrap();
    }
    // Keep the writer open as an inherited descriptor would be after leader exit.
    let summary = tokio::time::timeout(Duration::from_secs(2), capture.finish())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.retained_bytes, 17);
    assert_eq!(summary.discarded_bytes, payload.len() as u64 - 17);
    assert_eq!(std::fs::read(path).unwrap(), vec![b'x'; 17]);
}

#[tokio::test(flavor = "current_thread")]
async fn capture_reports_output_errors_instead_of_successful_empty_logs() {
    let (sender, receiver) = UnixStream::pair().unwrap();
    let capture = forge::process_capture::Capture::start(
        receiver.into(),
        std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap(),
        100,
    )
    .unwrap();
    let sender = Descriptor::new(sender.into()).unwrap();
    assert_eq!(sender.write(b"test").await.unwrap(), 4);
    drop(sender);
    assert!(capture.finish().await.is_err());
}
