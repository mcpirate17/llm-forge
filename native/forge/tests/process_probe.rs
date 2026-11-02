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

struct CancelFixture {
    child: std::process::Child,
    worker_pidfd: Option<std::os::fd::OwnedFd>,
    tid_path: std::path::PathBuf,
}
impl Drop for CancelFixture {
    fn drop(&mut self) {
        let still_owned = self.child.try_wait().ok().flatten().is_none();
        if still_owned {
            if let Ok(tid) = std::fs::read_to_string(&self.tid_path) {
                if let Ok(tid) = tid.trim().parse::<i32>() {
                    unsafe {
                        libc::syscall(libc::SYS_tgkill, self.child.id() as i32, tid, libc::SIGTERM);
                    }
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while Instant::now() < deadline {
                        if self.child.try_wait().ok().flatten().is_some() {
                            return;
                        }
                        std::thread::yield_now();
                    }
                }
            }
        }
        if let Some(fd) = &self.worker_pidfd {
            use std::os::fd::AsRawFd;
            // A pidfd remains bound to the fixture child even if its PID is reused.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn interruptible_input_probe_restores_child_mask_and_reaps_on_signal() {
    if let Some(dir) = std::env::var_os("FORGE_PROBE_CANCEL_CHILD") {
        let owner = forge::process_signals::Interrupts::install().unwrap();
        use std::os::unix::process::ExitStatusExt;
        let root = std::path::PathBuf::from(dir);
        let signal_check = process_probe::run_interruptible(
            Command::new("/bin/sh").args(["-c", "kill -TERM $$; exit 99"]),
            Duration::from_secs(2),
            4096,
            &owner,
        )
        .unwrap();
        assert_eq!(signal_check.status.signal(), Some(libc::SIGTERM));
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        std::fs::write(root.join("tid"), tid.to_string()).unwrap();
        let error = process_probe::run_with_input_interruptible(
            Command::new("/bin/sh")
                .arg("-c")
                .arg("echo $$ > \"$1.tmp\"; /bin/mv \"$1.tmp\" \"$1\"; while :; do :; done")
                .arg("probe")
                .arg(root.join("pid")),
            b"payload",
            Duration::from_secs(30),
            4096,
            &owner,
        )
        .unwrap_err();
        assert!(error.is::<process_probe::Interrupted>());
        return;
    }
    let host = support::Host::new();
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "interruptible_input_probe_restores_child_mask_and_reaps_on_signal",
            "--nocapture",
        ])
        .env("FORGE_PROBE_CANCEL_CHILD", &host.0)
        .spawn()
        .unwrap();
    let mut fixture = CancelFixture {
        child,
        worker_pidfd: None,
        tid_path: host.0.join("tid"),
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid_path = host.0.join("pid");
    let tid_path = host.0.join("tid");
    while !(pid_path.exists() && tid_path.exists()) {
        assert!(Instant::now() < deadline, "probe did not start");
        std::thread::yield_now();
    }
    let pid: i32 = std::fs::read_to_string(pid_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let tid: i32 = std::fs::read_to_string(tid_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    use std::os::fd::FromRawFd;
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    assert!(pidfd >= 0, "could not pin probe child");
    fixture.worker_pidfd = Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(pidfd as i32) });
    let sent = unsafe {
        libc::syscall(
            libc::SYS_tgkill,
            fixture.child.id() as i32,
            tid,
            libc::SIGTERM,
        )
    };
    assert_eq!(sent, 0);
    let status = loop {
        if let Some(status) = fixture.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "probe cancellation did not finish"
        );
        std::thread::yield_now();
    };
    assert!(status.success());
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    fixture.worker_pidfd = None;
}
