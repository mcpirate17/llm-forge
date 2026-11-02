//! Pipe-level fixtures: no cgroup delegation or sandbox permissions required.
use super::*;
use crate::process_io::open_pidfd;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, ExitStatus};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn capture(command: &mut Command, bytes: Vec<u8>, policy: CapturePolicy) -> Result<Captured> {
    let (captured, status, consumed) = supervise(command, bytes, policy)?;
    ensure!(status.success(), "pipe fixture failed");
    ensure!(consumed, "probe exited before accepting all stdin");
    Ok(captured)
}

fn supervise(
    command: &mut Command,
    bytes: Vec<u8>,
    policy: CapturePolicy,
) -> Result<(Captured, ExitStatus, bool)> {
    let input = Input::prepare_owned(command, bytes)?;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = ChildGuard(command.spawn()?);
    // Command otherwise retains the child's socket endpoint after spawn.
    command.stdin(Stdio::null());
    let pidfd = open_pidfd(&child.0)?;
    let mut monitor = Monitor::new(
        child.0.stdout.take().context("fixture stdout")?.into(),
        child.0.stderr.take().context("fixture stderr")?.into(),
        input,
        policy,
    )?;
    let timeout = Duration::from_secs(5);
    let observed = monitor.observe(pidfd.as_raw_fd(), deadline(timeout)?, None, || Ok(false))?;
    observation_error(observed, timeout)?;
    let consumed = monitor.require_stdin_consumed().is_ok();
    let status = child.0.wait()?;
    monitor.drain_to_eof(timeout)?;
    Ok((monitor.finish(), status, consumed))
}

fn echo(bytes: Vec<u8>) -> Result<Captured> {
    capture(
        &mut Command::new("/bin/cat"),
        bytes,
        CapturePolicy::StrictCombined(1024 * 1024),
    )
}

#[test]
fn owned_input_preserves_crlf_blank_lines_and_unterminated_tail() {
    let input = b"first\r\n\r\nsecond\n  trailing  ".to_vec();
    let captured = echo(input.clone()).unwrap();
    assert_eq!(captured.stdout, input);
    assert_eq!(captured.stdout_bytes, input.len() as u64);
    assert!(captured.stderr.is_empty());
    assert_eq!(captured.stderr_bytes, 0);
}

#[test]
fn owned_empty_input_delivers_eof() {
    let captured = echo(Vec::new()).unwrap();
    assert!(captured.stdout.is_empty());
    assert!(captured.stderr.is_empty());
    assert_eq!(captured.stdout_bytes, 0);
}

#[test]
fn owned_input_streams_full_mebibyte_while_draining_output() {
    let input: Vec<_> = (0..1024 * 1024).map(|index| (index % 256) as u8).collect();
    let captured = echo(input.clone()).unwrap();
    assert_eq!(captured.stdout, input);
    assert_eq!(captured.stdout_bytes, 1024 * 1024);
}

#[test]
fn owned_input_rejects_more_than_one_mebibyte() {
    let mut command = Command::new("/bin/cat");
    let error = Input::prepare_owned(&mut command, vec![0; 1024 * 1024 + 1])
        .err()
        .expect("oversized stdin must fail admission");
    assert!(error.to_string().contains("stdin exceeds 1 MiB"));
}

#[test]
fn borrowed_input_keeps_original_slice_without_copying() {
    let mut command = Command::new("/bin/cat");
    let bytes = b"borrowed\r\n";
    let input = Input::prepare(&mut command, Some(bytes)).unwrap();
    assert!(matches!(&input.bytes, Cow::Borrowed(_)));
    assert_eq!(input.bytes.as_ptr(), bytes.as_ptr());
    assert_eq!(input.offset, 0);
}

#[test]
fn strict_output_counts_combined_raw_bytes_at_exact_boundary() {
    let captured = capture(
        Command::new("/bin/sh").args(["-c", "printf '\\303\\251'; printf '\\r\\n' >&2"]),
        Vec::new(),
        CapturePolicy::StrictCombined(4),
    )
    .unwrap();
    assert_eq!(captured.stdout, "é".as_bytes());
    assert_eq!(captured.stderr, b"\r\n");
    assert_eq!(captured.stdout_bytes + captured.stderr_bytes, 4);
}

#[test]
fn strict_output_rejects_combined_overflow_across_streams() {
    let error = capture(
        Command::new("/bin/sh").args(["-c", "printf 'abc'; printf 'de' >&2"]),
        Vec::new(),
        CapturePolicy::StrictCombined(4),
    )
    .err()
    .expect("combined output beyond the cap must fail");
    assert!(error.to_string().contains("output exceeds 4 bytes"));
}

#[test]
fn strict_output_rejects_zero_and_oversized_limits() {
    assert!(CapturePolicy::StrictCombined(0).validate().is_err());
    assert!(CapturePolicy::StrictCombined(1024 * 1024 + 1)
        .validate()
        .is_err());
    assert!(CapturePolicy::StrictCombined(1).validate().is_ok());
    assert!(CapturePolicy::StrictCombined(1024 * 1024)
        .validate()
        .is_ok());
}

#[test]
fn tail_capture_keeps_existing_character_and_byte_count_semantics() {
    let captured = capture(
        &mut Command::new("/bin/cat"),
        "abcé\r\n".as_bytes().to_vec(),
        CapturePolicy::TailChars(3),
    )
    .unwrap();
    assert_eq!(captured.stdout, "cé\n".as_bytes());
    assert_eq!(captured.stdout_bytes, 7);
}

#[test]
fn exited_fixture_rejects_incomplete_stdin() {
    let error = capture(
        &mut Command::new("/bin/true"),
        vec![b'x'; 1024 * 1024],
        CapturePolicy::StrictCombined(16),
    )
    .err()
    .expect("early exit must not silently discard stdin");
    assert!(
        error.to_string().contains("stdin") || error.to_string().contains("Broken pipe"),
        "{error:#}"
    );
}

#[test]
fn nonreading_owned_filter_keeps_natural_nonzero_status() {
    let (_, status, consumed) = supervise(
        Command::new("/bin/sh").args(["-c", "exit 7"]),
        vec![b'x'; 1024 * 1024],
        CapturePolicy::StrictCombined(16),
    )
    .unwrap();
    assert_eq!(status.code(), Some(7));
    assert!(!consumed);
}

#[test]
fn nonreading_owned_filter_keeps_natural_signal_status() {
    let (_, status, consumed) = supervise(
        Command::new("/bin/sh").args(["-c", "kill -KILL $$"]),
        vec![b'x'; 1024 * 1024],
        CapturePolicy::StrictCombined(16),
    )
    .unwrap();
    assert_eq!(status.signal(), Some(libc::SIGKILL));
    assert!(!consumed);
}
