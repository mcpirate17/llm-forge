//! Fixed, bounded Git discovery subprocess. No shell or inherited environment.

use super::paths::{decode_hex, encode_hex};
use super::{NativeError, NativeResult};
use serde_json::{json, Value};
use std::ffi::{CString, OsString};
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const GIT: &str = "/usr/bin/git";
const TIMEOUT: Duration = Duration::from_secs(3);
const STDOUT_CAP: usize = 32 * 1024;
const STDERR_CAP: usize = 4 * 1024;
const ENV: [(&str, &str); 5] = [
    ("PATH", "/usr/bin:/bin"),
    ("LC_ALL", "C"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_OPTIONAL_LOCKS", "0"),
];

fn error(code: &'static str, message: &'static str) -> NativeError {
    NativeError::new(code, None, message)
}

fn trusted_git() -> NativeResult<&'static Path> {
    let path = Path::new(GIT);
    let executable = CString::new(GIT).unwrap();
    if !fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
        || unsafe { libc::access(executable.as_ptr(), libc::X_OK) } != 0
    {
        return Err(error(
            "GIT_UNAVAILABLE",
            "trusted /usr/bin/git is not an executable regular file",
        ));
    }
    Ok(path)
}

fn set_nonblocking(descriptor: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn stop_group(child: &mut Child) -> io::Result<()> {
    let result = unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
    if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
        return Err(io::Error::last_os_error());
    }
    child.wait()?;
    Ok(())
}

fn spawn(executable: &Path, argv: &[OsString]) -> NativeResult<Child> {
    let mut command = Command::new(executable);
    command
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.env_clear();
    for (key, value) in ENV {
        command.env(key, value);
    }
    // setsid is async-signal-safe in the forked child and gives us one group to reap.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    command.spawn().map_err(|_| {
        error(
            "GIT_UNAVAILABLE",
            "trusted /usr/bin/git could not be started",
        )
    })
}

fn read_ready(descriptor: i32, output: &mut Vec<u8>, cap: usize) -> NativeResult<bool> {
    let mut chunk = [0_u8; 8_192];
    let count = unsafe { libc::read(descriptor, chunk.as_mut_ptr().cast(), chunk.len()) };
    if count == 0 {
        return Ok(false);
    }
    if count < 0 {
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::EINTR | libc::EAGAIN)) {
            return Ok(true);
        }
        return Err(super::NativeError::new(
            "GIT_DISCOVERY_FAILED",
            None,
            "Git discovery pipe I/O failed",
        ));
    }
    let count = count as usize;
    if count > cap.saturating_sub(output.len()) {
        return Err(super::NativeError::new(
            "GIT_OUTPUT_LIMIT",
            None,
            if cap == STDOUT_CAP {
                "Git stdout exceeded its output limit"
            } else {
                "Git stderr exceeded its output limit"
            },
        ));
    }
    output.extend_from_slice(&chunk[..count]);
    Ok(true)
}

fn gather(
    child: &mut Child,
    timeout: Duration,
    stdout_cap: usize,
    stderr_cap: usize,
) -> NativeResult<Vec<u8>> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| error("GIT_DISCOVERY_FAILED", "Git discovery pipe I/O failed"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| error("GIT_DISCOVERY_FAILED", "Git discovery pipe I/O failed"))?;
    let descriptors = [stdout.as_raw_fd(), stderr.as_raw_fd()];
    for descriptor in descriptors {
        set_nonblocking(descriptor)
            .map_err(|_| error("GIT_DISCOVERY_FAILED", "Git discovery pipe I/O failed"))?;
    }
    let mut open = [true, true];
    let mut output = [Vec::new(), Vec::new()];
    let deadline = Instant::now() + timeout;
    while open.iter().any(|item| *item) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(error(
                "GIT_TIMEOUT",
                "Git discovery exceeded its 3-second deadline",
            ));
        }
        let mut poll = [
            libc::pollfd {
                fd: descriptors[0],
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: descriptors[1],
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        for index in 0..2 {
            if !open[index] {
                poll[index].fd = -1;
            }
        }
        let milliseconds = remaining.as_millis().min(i32::MAX as u128).max(1) as i32;
        let ready =
            unsafe { libc::poll(poll.as_mut_ptr(), poll.len() as libc::nfds_t, milliseconds) };
        if ready < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error(
                "GIT_DISCOVERY_FAILED",
                "Git discovery pipe I/O failed",
            ));
        }
        for index in 0..2 {
            if !open[index] || poll[index].revents == 0 {
                continue;
            }
            let cap = if index == 0 { stdout_cap } else { stderr_cap };
            open[index] = read_ready(descriptors[index], &mut output[index], cap)?;
        }
    }
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(output[0].clone()),
            Ok(Some(_)) => {
                return Err(error(
                    "GIT_DISCOVERY_FAILED",
                    "Git discovery failed for the selected project",
                ))
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                return Err(error(
                    "GIT_TIMEOUT",
                    "Git discovery exceeded its 3-second deadline",
                ))
            }
            Err(_) => {
                return Err(error(
                    "GIT_DISCOVERY_FAILED",
                    "Git discovery pipe I/O failed",
                ))
            }
        }
    }
}

fn run(
    executable: &Path,
    argv: &[OsString],
    timeout: Duration,
    stdout_cap: usize,
    stderr_cap: usize,
) -> NativeResult<Vec<u8>> {
    let mut child = spawn(executable, argv)?;
    let result = gather(&mut child, timeout, stdout_cap, stderr_cap);
    if result.is_err() {
        stop_group(&mut child)
            .map_err(|_| error("GIT_DISCOVERY_FAILED", "Git discovery pipe I/O failed"))?;
    }
    result
}

pub(super) fn bounded_git(payload: &Value) -> NativeResult<Value> {
    let raw = payload["argv_hex"].as_array().ok_or_else(|| {
        NativeError::new(
            "INVALID_ARGUMENT",
            None,
            "missing Git arguments in native project context",
        )
    })?;
    let argv: Vec<OsString> = raw
        .iter()
        .map(|item| {
            item.as_str()
                .ok_or_else(|| {
                    NativeError::new(
                        "INVALID_ARGUMENT",
                        None,
                        "malformed Git arguments in native project context",
                    )
                })
                .and_then(decode_hex)
                .map(OsString::from_vec)
        })
        .collect::<NativeResult<_>>()?;
    let output = run(trusted_git()?, &argv, TIMEOUT, STDOUT_CAP, STDERR_CAP)?;
    Ok(json!({"stdout_hex": encode_hex(&output)}))
}

#[cfg(test)]
mod tests {
    use super::{run, ENV, STDERR_CAP, STDOUT_CAP};
    use std::ffi::OsString;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn fixture_script(script: &str, timeout: Duration) -> (&'static str, Duration) {
        let directory = std::env::temp_dir().join(format!(
            "forge-git-child-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let pid_path = directory.join("pid");
        let args = [
            OsString::from("-c"),
            OsString::from(format!("echo $$ > \"$1\"; exec {script}")),
            OsString::from("sh"),
            pid_path.as_os_str().to_os_string(),
        ];
        let started = Instant::now();
        let error = run(Path::new("/bin/sh"), &args, timeout, STDOUT_CAP, STDERR_CAP).unwrap_err();
        let elapsed = started.elapsed();
        let pid: libc::pid_t = fs::read_to_string(&pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "child was not reaped");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        fs::remove_dir_all(directory).unwrap();
        (error.code, elapsed)
    }

    #[test]
    fn child_receives_exact_environment() {
        let output = run(
            Path::new("/usr/bin/env"),
            &[],
            Duration::from_secs(1),
            STDOUT_CAP,
            STDERR_CAP,
        )
        .unwrap();
        let mut rows: Vec<_> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        rows.sort();
        let mut expected: Vec<_> = ENV
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        expected.sort();
        assert_eq!(rows, expected);
    }

    #[test]
    fn output_cap_kills_and_reaps_the_child() {
        let (code, elapsed) = fixture_script("yes", Duration::from_secs(2));
        assert_eq!(code, "GIT_OUTPUT_LIMIT");
        assert!(elapsed < Duration::from_secs(2));
        let (code, _) = fixture_script("yes >&2", Duration::from_secs(2));
        assert_eq!(code, "GIT_OUTPUT_LIMIT");
    }

    #[test]
    fn deadline_kills_and_reaps_the_child() {
        let (code, elapsed) = fixture_script("sleep 30", Duration::from_millis(200));
        assert_eq!(code, "GIT_TIMEOUT");
        assert!(elapsed < Duration::from_secs(2));
    }
}
