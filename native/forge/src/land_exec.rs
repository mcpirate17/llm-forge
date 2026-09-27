//! Process plumbing for `forge land`: git calls that fail loud, and shell
//! checks bounded by a wall clock that kill their whole process group.
//!
//! `bounded_child::run_bounded` re-invokes forge and kills one pid; a check
//! here is `sh -c <cmd>` whose children (pytest workers, cargo, rustc) must
//! die with it, so each check leads its own process group and the group is
//! SIGKILLed on overrun and again after exit to reap stragglers.

use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

/// Runs `git <args>` in `dir`; returns trimmed stdout, bails with stderr.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("spawning git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

/// Like [`git`] but reports success instead of bailing, for probes.
pub fn git_ok(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed(i32),
    TimedOut,
}

pub struct ShellRun<'a> {
    pub command: &'a str,
    pub cwd: &'a Path,
    pub env: &'a [(String, String)],
    pub log: &'a Path,
    pub timeout: Duration,
}

/// Runs `sh -c command` with stdout and stderr appended to `log`.
pub fn run_shell(run: &ShellRun) -> Result<Outcome> {
    let log = File::create(run.log).with_context(|| format!("creating {}", run.log.display()))?;
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(run.command)
        .current_dir(run.cwd)
        .envs(run.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0)
        .spawn()
        .with_context(|| format!("spawning sh -c {:?}", run.command))?;
    let pgid = child.id() as libc::pid_t;
    let deadline = Instant::now() + run.timeout;
    let outcome = loop {
        if let Some(status) = child.try_wait()? {
            break match status.code() {
                Some(0) => Outcome::Passed,
                Some(code) => Outcome::Failed(code),
                None => Outcome::Failed(-1),
            };
        }
        if Instant::now() >= deadline {
            kill_group(pgid);
            child.wait()?;
            break Outcome::TimedOut;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    kill_group(pgid);
    Ok(outcome)
}

fn kill_group(pgid: libc::pid_t) {
    // SAFETY: kill(2) with a negative pid signals the group we created;
    // ESRCH once the group is empty is the expected, ignorable result.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

/// The last `lines` lines of `path`, for a failure report.
pub fn tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("forge-land-exec-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn shell(dir: &Path, command: &str, timeout: Duration) -> Outcome {
        let log = dir.join("log");
        let env = vec![("LAND_PROBE".to_string(), "seen".to_string())];
        run_shell(&ShellRun {
            command,
            cwd: dir,
            env: &env,
            log: &log,
            timeout,
        })
        .unwrap()
    }

    #[test]
    fn exit_codes_and_env_reach_the_outcome_and_log() {
        let dir = scratch("codes");
        assert_eq!(
            shell(&dir, "echo $LAND_PROBE", Duration::from_secs(5)),
            Outcome::Passed
        );
        assert_eq!(tail(&dir.join("log"), 5), "seen");
        assert_eq!(
            shell(&dir, "exit 3", Duration::from_secs(5)),
            Outcome::Failed(3)
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn overrun_kills_the_whole_group() {
        let dir = scratch("group");
        let marker = dir.join("survived");
        let command = format!("(sleep 2; touch {}) & sleep 30", marker.display());
        let started = Instant::now();
        assert_eq!(
            shell(&dir, &command, Duration::from_millis(300)),
            Outcome::TimedOut
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(2500));
        assert!(!marker.exists(), "a grandchild outlived the timeout");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tail_keeps_only_the_last_lines() {
        let dir = scratch("tail");
        std::fs::write(dir.join("f"), "a\nb\nc\n").unwrap();
        assert_eq!(tail(&dir.join("f"), 2), "b\nc");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
