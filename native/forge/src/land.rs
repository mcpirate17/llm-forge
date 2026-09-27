//! `forge land <branch>`: land a branch on the integration line from a
//! persistent local clone, with the host's own checks, and no CI service.
//!
//! One lander at a time (flock beside the clone). The clone keeps its
//! ignored state -- a venv, cargo `target/` -- between landings, so setup is
//! incremental rather than a reinstall. The branch is rebased onto the
//! target, the host's declared checks run bounded, and the rebased head is
//! pushed as a fast-forward of the target: the remote refusing a non-ff push
//! is the race check against anyone who landed meanwhile. Then the branch is
//! deleted, leased to the sha that was checked.
//!
//! The host owns every policy: `.forge/land.toml` names the clone, the
//! setup, the checks, which paths select them, their bounds, which are
//! blocking, and which commit trailers are required. Forge supplies only
//! the mechanism.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Args;
use regex::Regex;
use serde::Deserialize;

use crate::land_exec::{git, git_ok, run_shell, tail, Outcome, ShellRun};

const CONFIG_PATH: &str = ".forge/land.toml";
const WORK_BRANCH: &str = "forge-land";

#[derive(Args)]
pub struct LandArgs {
    /// The remote branch to land.
    pub branch: String,
    /// Run everything except the push and the branch delete.
    #[arg(long)]
    pub dry_run: bool,
    /// Config to bootstrap from; defaults to `.forge/land.toml` in the
    /// current checkout. Checks are re-read from the target branch's copy
    /// when it has one, so a branch cannot loosen its own gate.
    #[arg(long)]
    pub config: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_remote")]
    pub remote: String,
    pub target: String,
    /// The persistent clone; created on first use.
    pub clone_dir: PathBuf,
    /// Trailer keys every landed commit must carry, e.g. `["Agent"]`.
    #[serde(default)]
    pub required_trailers: Vec<String>,
    /// Directories (relative to the clone) prepended to PATH for every step.
    #[serde(default)]
    pub path_prepend: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Always run, in order, before any check; any failure blocks.
    #[serde(default)]
    pub setup: Vec<Step>,
    #[serde(default, rename = "check")]
    pub checks: Vec<Step>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub name: String,
    pub run: String,
    /// Regexes over changed paths; the step runs when any path matches one.
    /// Empty means always.
    #[serde(default)]
    pub paths: Vec<String>,
    pub timeout_s: u64,
    #[serde(default = "default_true")]
    pub blocking: bool,
}

fn default_remote() -> String {
    "origin".to_string()
}

fn default_true() -> bool {
    true
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        let config: Config = toml::from_str(text).context("parsing land config")?;
        for step in config.setup.iter().chain(&config.checks) {
            for pattern in &step.paths {
                Regex::new(pattern)
                    .with_context(|| format!("step {}: bad path regex", step.name))?;
            }
            if step.timeout_s == 0 {
                bail!("step {}: timeout_s must be positive", step.name);
            }
        }
        Ok(config)
    }
}

/// True when the step has no path filter or any changed path matches it.
pub fn selected(step: &Step, changed: &[String]) -> bool {
    if step.paths.is_empty() {
        return true;
    }
    let patterns: Vec<Regex> = step
        .paths
        .iter()
        .map(|p| Regex::new(p).expect("validated"))
        .collect();
    changed
        .iter()
        .any(|path| patterns.iter().any(|re| re.is_match(path)))
}

/// Commits (`sha subject`) missing any required trailer, given
/// `git log --format=%H%x1f%s%x1f%(trailers:only,unfold)%x1e` output.
pub fn missing_trailers(log: &str, required: &[String]) -> Vec<String> {
    let mut missing = Vec::new();
    for record in log.split('\u{1e}').map(str::trim).filter(|r| !r.is_empty()) {
        let mut fields = record.splitn(3, '\u{1f}');
        let sha = fields.next().unwrap_or_default();
        let subject = fields.next().unwrap_or_default();
        let trailers = fields.next().unwrap_or_default();
        for key in required {
            let prefix = format!("{key}:");
            let present = trailers.lines().any(|line| {
                line.strip_prefix(&prefix)
                    .is_some_and(|v| !v.trim().is_empty())
            });
            if !present {
                missing.push(format!(
                    "{} {subject} (no {key}:)",
                    &sha[..sha.len().min(10)]
                ));
            }
        }
    }
    missing
}

pub fn run(args: LandArgs) -> Result<u8> {
    let here = std::env::current_dir()?;
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| here.join(CONFIG_PATH));
    let text = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading {}", config_path.display()))?;
    let boot = Config::parse(&text)?;
    let url = git(&here, &["remote", "get-url", &boot.remote])?;
    let _lock = acquire_lock(&boot.clone_dir)?;
    let clone = prepare_clone(&boot, &url)?;
    let config = target_config(&clone, &boot)?.unwrap_or(boot);
    let landing = rebase(&clone, &config, &args.branch)?;
    let failed = run_steps(&clone, &config, &landing)?;
    if failed > 0 {
        eprintln!(
            "land: {failed} blocking step(s) failed; {} not landed",
            args.branch
        );
        return Ok(1);
    }
    if args.dry_run {
        println!(
            "land: dry run passed; {} would land as {}",
            args.branch, landing.head
        );
        return Ok(0);
    }
    publish(&clone, &config, &args.branch, &landing)?;
    Ok(0)
}

/// The returned File holds the lock until it is dropped.
fn acquire_lock(clone_dir: &Path) -> Result<File> {
    let lock_path = clone_dir.with_extension("lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&lock_path)
        .with_context(|| format!("opening {}", lock_path.display()))?;
    let fd = file.as_raw_fd();
    // SAFETY: flock(2) on an fd this process owns; the lock is released
    // when the guard's File closes.
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        eprintln!(
            "land: another landing holds {}; waiting",
            lock_path.display()
        );
        if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
            bail!(
                "flock {}: {}",
                lock_path.display(),
                std::io::Error::last_os_error()
            );
        }
    }
    Ok(file)
}

fn prepare_clone(config: &Config, url: &str) -> Result<PathBuf> {
    let clone = config.clone_dir.clone();
    if !clone.join(".git").exists() {
        let parent = clone.parent().context("clone_dir has no parent")?;
        std::fs::create_dir_all(parent)?;
        let dest = clone.to_string_lossy().to_string();
        git(
            parent,
            &["clone", "--quiet", "--origin", &config.remote, url, &dest],
        )?;
    }
    git(&clone, &["remote", "set-url", &config.remote, url])?;
    // A previous landing killed mid-rebase leaves state behind; clear it.
    // `clean -fd` without `-x` keeps ignored state (venv, target/).
    let _ = git_ok(&clone, &["rebase", "--abort"]);
    git(&clone, &["reset", "--quiet", "--hard"])?;
    git(&clone, &["clean", "-fdq"])?;
    git(&clone, &["fetch", "--quiet", "--prune", &config.remote])?;
    Ok(clone)
}

/// The target branch's own config, when it has one.
fn target_config(clone: &Path, boot: &Config) -> Result<Option<Config>> {
    let spec = format!("{}/{}:{CONFIG_PATH}", boot.remote, boot.target);
    if !git_ok(clone, &["cat-file", "-e", &spec]) {
        eprintln!(
            "land: {} has no {CONFIG_PATH}; using the bootstrap config",
            boot.target
        );
        return Ok(None);
    }
    Config::parse(&git(clone, &["show", &spec])?).map(Some)
}

struct Landing {
    branch_sha: String,
    base: String,
    head: String,
    changed: Vec<String>,
}

fn rebase(clone: &Path, config: &Config, branch: &str) -> Result<Landing> {
    let remote_branch = format!("{}/{branch}", config.remote);
    let target = format!("{}/{}", config.remote, config.target);
    let branch_sha = git(
        clone,
        &[
            "rev-parse",
            "--verify",
            &format!("{remote_branch}^{{commit}}"),
        ],
    )
    .with_context(|| format!("{remote_branch} does not exist; push it first"))?;
    let base = git(clone, &["rev-parse", &target])?;
    git(
        clone,
        &["checkout", "--quiet", "-B", WORK_BRANCH, &branch_sha],
    )?;
    if !git_ok(clone, &["rebase", "--quiet", &base]) {
        let _ = git_ok(clone, &["rebase", "--abort"]);
        bail!("{branch} does not rebase cleanly onto {target}; rebase it yourself and push");
    }
    let head = git(clone, &["rev-parse", "HEAD"])?;
    let range = format!("{base}..{head}");
    let count = git(clone, &["rev-list", "--count", &range])?;
    if count == "0" {
        bail!("{branch} has nothing that is not already on {target}");
    }
    let log = git(
        clone,
        &[
            "log",
            "--format=%H%x1f%s%x1f%(trailers:only,unfold)%x1e",
            &range,
        ],
    )?;
    let missing = missing_trailers(&log, &config.required_trailers);
    if !missing.is_empty() {
        bail!(
            "commits missing required trailers:\n  {}",
            missing.join("\n  ")
        );
    }
    let changed = git(clone, &["diff", "--name-only", &format!("{base}...{head}")])?
        .lines()
        .map(str::to_string)
        .collect();
    println!("land: {branch} rebased onto {target} ({count} commit(s), head {head})");
    Ok(Landing {
        branch_sha,
        base,
        head,
        changed,
    })
}

/// Runs setup then the selected checks; returns the blocking failure count.
fn run_steps(clone: &Path, config: &Config, landing: &Landing) -> Result<usize> {
    let log_dir = clone.with_extension("logs");
    std::fs::create_dir_all(&log_dir)?;
    let changed_file = log_dir.join("changed.txt");
    std::fs::write(&changed_file, landing.changed.join("\n") + "\n")?;
    let env = step_env(clone, config, landing, &changed_file)?;
    for step in &config.setup {
        if !run_step(clone, step, &env, &log_dir)? {
            eprintln!("land: setup {} failed; no checks run", step.name);
            return Ok(1);
        }
    }
    let mut failed = 0;
    for step in &config.checks {
        if !selected(step, &landing.changed) {
            println!("  skip  {} (no matching path)", step.name);
            continue;
        }
        if !run_step(clone, step, &env, &log_dir)? && step.blocking {
            failed += 1;
        }
    }
    Ok(failed)
}

fn step_env(
    clone: &Path,
    config: &Config,
    landing: &Landing,
    changed: &Path,
) -> Result<Vec<(String, String)>> {
    let mut path: Vec<String> = config
        .path_prepend
        .iter()
        .map(|dir| clone.join(dir).to_string_lossy().to_string())
        .collect();
    path.push(std::env::var("PATH").unwrap_or_default());
    let mut env: Vec<(String, String)> = config.env.clone().into_iter().collect();
    env.push(("PATH".into(), path.join(":")));
    env.push(("FORGE_LAND_BASE".into(), landing.base.clone()));
    env.push((
        "FORGE_LAND_CHANGED".into(),
        changed.to_string_lossy().to_string(),
    ));
    Ok(env)
}

/// Runs one step, prints its verdict line; true when it passed.
fn run_step(clone: &Path, step: &Step, env: &[(String, String)], log_dir: &Path) -> Result<bool> {
    let log = log_dir.join(format!("{}.log", step.name.replace(['/', ' '], "_")));
    let started = Instant::now();
    let outcome = run_shell(&ShellRun {
        command: &step.run,
        cwd: clone,
        env,
        log: &log,
        timeout: Duration::from_secs(step.timeout_s),
    })?;
    let secs = started.elapsed().as_secs_f64();
    let kind = if step.blocking { "" } else { " (advisory)" };
    match outcome {
        Outcome::Passed => {
            println!("  pass  {}{kind} {secs:.1}s", step.name);
            return Ok(true);
        }
        Outcome::Failed(code) => println!("  FAIL  {}{kind} exit {code} {secs:.1}s", step.name),
        Outcome::TimedOut => println!(
            "  FAIL  {}{kind} timed out after {}s",
            step.name, step.timeout_s
        ),
    }
    println!("        log {}\n{}", log.display(), indent(&tail(&log, 30)));
    Ok(false)
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("        | {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn publish(clone: &Path, config: &Config, branch: &str, landing: &Landing) -> Result<()> {
    let target_ref = format!("{}:refs/heads/{}", landing.head, config.target);
    git(clone, &["push", "--quiet", &config.remote, &target_ref])
        .context("fast-forward push refused (the target moved?); run land again")?;
    println!("land: {} -> {} at {}", branch, config.target, landing.head);
    let lease = format!(
        "--force-with-lease=refs/heads/{branch}:{}",
        landing.branch_sha
    );
    let delete = format!(":refs/heads/{branch}");
    if let Err(error) = git(clone, &["push", "--quiet", &lease, &config.remote, &delete]) {
        // Landed already; a branch that moved after the check is kept.
        eprintln!("land: kept {branch}: {error:#}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
target = "master"
clone_dir = "/tmp/land/x"
required_trailers = ["Agent"]

[[setup]]
name = "sync"
run = "true"
timeout_s = 60

[[check]]
name = "ruff"
run = "ruff check ."
paths = ['\.py$']
timeout_s = 300

[[check]]
name = "debt"
run = "true"
timeout_s = 60
blocking = false
"#;

    fn changed(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn config_parses_defaults_and_rejects_unknown_keys() {
        let config = Config::parse(SAMPLE).unwrap();
        assert_eq!(config.remote, "origin");
        assert!(config.checks[0].blocking);
        assert!(!config.checks[1].blocking);
        assert!(Config::parse(&format!("{SAMPLE}\nbogus = 1\n")).is_err());
    }

    #[test]
    fn config_rejects_a_bad_regex_and_a_zero_timeout() {
        assert!(Config::parse(&SAMPLE.replace(r"\.py$", "(")).is_err());
        assert!(Config::parse(&SAMPLE.replace("timeout_s = 300", "timeout_s = 0")).is_err());
    }

    #[test]
    fn selection_follows_path_regexes() {
        let config = Config::parse(SAMPLE).unwrap();
        assert!(selected(&config.checks[0], &changed(&["a/b.py"])));
        assert!(!selected(
            &config.checks[0],
            &changed(&["a/b.rs", "c.pyi.txt"])
        ));
        assert!(selected(&config.checks[1], &changed(&[])));
    }

    #[test]
    fn trailers_must_be_present_and_nonempty() {
        let log = "aaaaaaaaaaaa\u{1f}feat: one\u{1f}Agent: llm-1\n\u{1e}\
                   bbbbbbbbbbbb\u{1f}fix: two\u{1f}Co-Authored-By: x\n\u{1e}\
                   cccccccccccc\u{1f}chore: three\u{1f}Agent:  \n\u{1e}";
        let missing = missing_trailers(log, &["Agent".to_string()]);
        assert_eq!(
            missing,
            vec![
                "bbbbbbbbbb fix: two (no Agent:)",
                "cccccccccc chore: three (no Agent:)"
            ]
        );
        assert!(missing_trailers(log, &[]).is_empty());
    }
}
