//! Process plumbing for `forge land`: git calls that fail loud, and shell
//! checks bounded by a wall clock that kill their whole process group.
//!
//! `bounded_child::run_bounded` re-invokes forge and kills one pid; a check
//! here is `sh -c <cmd>` whose children (pytest workers, cargo, rustc) must
//! die with it, so each check leads its own process group and the group is
//! SIGKILLed on overrun and again after exit to reap stragglers.

use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

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

#[path = "land_process.rs"]
mod process;

pub use process::{run_measured, Usage};

/// Executes one bounded process group; logs retain at most 16 MiB.
pub fn run_shell(run: &ShellRun) -> Result<Outcome> {
    Ok(run_measured(run, 16 * 1024 * 1024)?.outcome)
}

/// Read at most 64 KiB backwards, regardless of log size or line length.
pub fn tail(path: &Path, lines: usize) -> String {
    match bounded_tail(path, lines, 64 * 1024) {
        Ok(text) => text,
        Err(error) => format!("unable to read log {}: {error}", path.display()),
    }
}

fn bounded_tail(path: &Path, lines: usize, byte_limit: usize) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    if lines == 0 || byte_limit == 0 {
        return Ok(String::new());
    }
    let mut file = File::open(path)?;
    let mut position = file.metadata()?.len();
    let mut pieces = Vec::new();
    let mut bytes = 0;
    let mut newlines = 0;
    while position > 0 && bytes < byte_limit && newlines <= lines {
        let count = (position as usize).min(4096).min(byte_limit - bytes);
        position -= count as u64;
        file.seek(SeekFrom::Start(position))?;
        let mut piece = vec![0; count];
        file.read_exact(&mut piece)?;
        newlines += piece.iter().filter(|byte| **byte == b'\n').count();
        bytes += count;
        pieces.push(piece);
    }
    let data: Vec<u8> = pieces.into_iter().rev().flatten().collect();
    let text = String::from_utf8_lossy(&data);
    let mut output = text.lines().rev().take(lines).collect::<Vec<_>>();
    output.reverse();
    let result = output.join("\n");
    Ok(if position > 0 && newlines <= lines {
        format!("[tail clipped at {byte_limit} bytes]\n{result}")
    } else {
        result
    })
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

    #[test]
    fn sparse_large_tail_and_long_lines_have_bounded_output() {
        use std::io::{Seek, SeekFrom, Write};
        let dir = scratch("large-tail");
        let path = dir.join("sparse");
        let mut file = File::create(&path).unwrap();
        file.set_len(128 * 1024 * 1024).unwrap();
        file.seek(SeekFrom::End(-7)).unwrap();
        file.write_all(b"\na\nb\nc\n").unwrap();
        assert_eq!(tail(&path, 2), "b\nc");
        let text = tail(&path, 20);
        assert!(text.len() < 66 * 1024);
        assert!(text.starts_with("[tail clipped"));
        assert_eq!(tail(&path, 0), "");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn measured_output_is_drained_capped_and_accounted() {
        let dir = scratch("measured");
        let measured = run_measured(
            &ShellRun {
                command: "head -c 131072 /dev/zero",
                cwd: &dir,
                env: &[],
                log: &dir.join("log"),
                timeout: Duration::from_secs(5),
            },
            1024,
        )
        .unwrap();
        assert_eq!(measured.outcome, Outcome::Passed);
        assert_eq!(measured.usage.retained_bytes, 1024);
        assert_eq!(measured.usage.discarded_bytes, 130048);
        assert!(measured.usage.max_rss_bytes > 0);
        assert!(measured.usage.wall_ms > 0.0);
        assert_eq!(std::fs::metadata(dir.join("log")).unwrap().len(), 1024);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
