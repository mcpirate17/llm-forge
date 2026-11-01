#![cfg(target_os = "linux")]
#[allow(dead_code)]
mod support;
use forge::process_probe;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn probe_output_cap_includes_both_real_streams_and_preserves_status() {
    let worker = support::task_worker();
    let output = process_probe::run(
        Command::new(worker).arg("output"),
        Duration::from_secs(2),
        16384,
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, vec![b'x'; 8192]);
    assert_eq!(output.stderr, b"error detail");
    let error = process_probe::run(
        Command::new(worker).arg("output"),
        Duration::from_secs(2),
        8192,
    )
    .unwrap_err();
    assert!(error.to_string().contains("probe output exceeds"));
    let output = process_probe::run(
        Command::new(worker).args(["exit", "7"]),
        Duration::from_secs(2),
        100,
    )
    .unwrap();
    assert_eq!(output.status.code(), Some(7));
}

#[test]
fn deadline_kills_and_reaps_without_waiter_threads_or_external_kill() {
    let worker = support::task_worker();
    let host = support::Host::new();
    let pid_path = host.0.join("pid");
    let started = Instant::now();
    let error = process_probe::run(
        Command::new(worker)
            .args(["pid-sleep", "10000"])
            .arg(&pid_path)
            .env("PATH", ""),
        Duration::from_millis(100),
        1024,
    )
    .unwrap_err();
    assert!(error.to_string().contains("timed out"));
    assert!(error.is::<process_probe::TimedOut>());
    assert!(started.elapsed() < Duration::from_secs(2));
    let pid: i32 = std::fs::read_to_string(&pid_path).unwrap().parse().unwrap();
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn invalid_bounds_never_launch_the_command() {
    for (timeout, limit) in [
        (Duration::ZERO, 1024),
        (Duration::from_secs(1), 0),
        (Duration::from_secs(1), 1024 * 1024 + 1),
    ] {
        let error =
            process_probe::run(&mut Command::new("/not/a/program"), timeout, limit).unwrap_err();
        assert!(!error.to_string().contains("starting probe"));
    }
}

#[test]
fn input_and_both_output_streams_progress_without_pipe_deadlock() {
    let input = "🦀\0text\n".repeat(32768).into_bytes();
    let output = process_probe::run_with_input(
        Command::new(support::task_worker())
            .arg("stdin-duplex")
            .env("PATH", ""),
        &input,
        Duration::from_secs(3),
        1024 * 1024,
    )
    .unwrap();
    assert!(output.status.success());
    assert_eq!(&output.stdout[..256 * 1024], vec![b'p'; 256 * 1024]);
    assert_eq!(&output.stdout[256 * 1024..], input);
    assert_eq!(output.stderr, b"stdin consumed\n");
    let empty = process_probe::run_with_input(
        Command::new(support::task_worker()).arg("stdin-echo"),
        &[],
        Duration::from_secs(1),
        1024,
    )
    .unwrap();
    assert!(empty.status.success());
    assert!(empty.stdout.is_empty());
}

#[test]
fn blocked_or_early_closed_input_obeys_deadlines_and_oversized_input_never_launches() {
    let input = vec![b'x'; 1024 * 1024];
    let started = Instant::now();
    let result = process_probe::run_with_input(
        Command::new(support::task_worker()).args(["sleep", "10000"]),
        &input,
        Duration::from_millis(100),
        1024,
    );
    assert!(result.unwrap_err().to_string().contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(2));
    let result = process_probe::run_with_input(
        Command::new(support::task_worker()).args(["exit", "0"]),
        &input,
        Duration::from_secs(1),
        1024,
    );
    assert!(result.unwrap_err().to_string().contains("stdin"));
    let result = process_probe::run_with_input(
        &mut Command::new("/not/a/program"),
        &vec![0; 1024 * 1024 + 1],
        Duration::from_secs(1),
        1024,
    );
    assert!(result.unwrap_err().to_string().contains("stdin exceeds"));
}
