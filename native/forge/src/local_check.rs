//! Local checks for the exact committed checkout. No clone, push, or remote write.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use clap::Args;
use regex::Regex;
use serde::Deserialize;

use crate::land::{selected, Step};
use crate::land_exec::{run_shell, tail, Outcome, ShellRun};
use crate::local_check_receipt::{self as evidence, Receipt, StepRecord, Verdict};

const POLICY_PATH: &str = ".forge/local-check.toml";
const DEFAULT_BASE_REF: &str = "origin/main";

#[derive(Args)]
pub struct CheckArgs {
    /// Run every declared check. Required for the pre-PR full gate.
    #[arg(long)]
    pub all: bool,
    /// Override the policy's local target ref (for offline or test use).
    #[arg(long)]
    pub base_ref: Option<String>,
}

#[derive(Args)]
pub struct VerifyArgs {
    /// Receipt to inspect; defaults to the latest local attempt.
    #[arg(long)]
    pub receipt: Option<PathBuf>,
    /// Reject a path-selected receipt, even if all selected steps passed.
    #[arg(long)]
    pub require_all: bool,
    /// Expected integration ref; defaults to origin/main.
    #[arg(long, default_value = DEFAULT_BASE_REF)]
    pub base_ref: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    base_ref: String,
    #[serde(default)]
    path_prepend: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    allowed_untracked_prefixes: Vec<String>,
    #[serde(default)]
    setup: Vec<Step>,
    #[serde(default, rename = "check")]
    checks: Vec<Step>,
}

impl Policy {
    fn parse(text: &str) -> Result<Self> {
        let policy: Self = toml::from_str(text).context("parsing local check policy")?;
        if policy.base_ref.is_empty() || policy.checks.is_empty() {
            bail!("local check policy needs base_ref and at least one check");
        }
        let mut names = HashSet::new();
        for step in policy.setup.iter().chain(&policy.checks) {
            if !names.insert(&step.name) || step.name.is_empty() || step.timeout_s == 0 {
                bail!(
                    "duplicate, empty, or unbounded local check step: {}",
                    step.name
                );
            }
            for pattern in &step.paths {
                Regex::new(pattern)
                    .with_context(|| format!("step {}: bad path regex", step.name))?;
            }
        }
        if policy.setup.iter().any(|step| !step.blocking) {
            bail!("local check setup steps must be blocking");
        }
        for prefix in &policy.allowed_untracked_prefixes {
            if prefix.is_empty() || prefix.starts_with('/') || prefix.contains("..") {
                bail!("invalid allowed_untracked_prefixes entry: {prefix}");
            }
        }
        for dir in &policy.path_prepend {
            if Path::new(dir).is_absolute() || dir.split('/').any(|p| p == "..") {
                bail!("path_prepend must stay inside the checkout: {dir}");
            }
        }
        Ok(policy)
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Source {
    head: String,
    tree: String,
    base_sha: String,
    merge_base: String,
    policy_source: String,
    policy_sha256: String,
    changed_paths: Vec<String>,
}

fn root() -> Result<PathBuf> {
    Ok(PathBuf::from(git(
        Path::new("."),
        &["rev-parse", "--show-toplevel"],
    )?))
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git_bytes(root, args)?)?
        .trim_end()
        .to_string())
}

fn git_ok(root: &Path, args: &[&str]) -> bool {
    git_bytes(root, args).is_ok()
}

fn common_dir(root: &Path) -> Result<PathBuf> {
    let dir = PathBuf::from(git(root, &["rev-parse", "--git-common-dir"])?);
    Ok(if dir.is_absolute() {
        dir
    } else {
        root.join(dir)
    })
}

fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command.args(args).current_dir(root);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output.stdout)
}

fn changed_paths(root: &Path, merge_base: &str) -> Result<Vec<String>> {
    let bytes = git_bytes(
        root,
        &["diff", "--name-only", "-z", merge_base, "HEAD", "--"],
    )?;
    let mut paths = Vec::new();
    for path in bytes.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8(path.to_vec()).context("non-UTF-8 changed path")?;
        if path.contains('\n') {
            bail!("newline in changed path: {path:?}");
        }
        paths.push(path);
    }
    Ok(paths)
}

fn effective_policy(root: &Path, base_sha: &str) -> Result<(Policy, String, String)> {
    let base_spec = format!("{base_sha}:{POLICY_PATH}");
    let (bytes, source) = if git_ok(root, &["cat-file", "-e", &base_spec]) {
        (git_bytes(root, &["show", &base_spec])?, "base".to_string())
    } else {
        let head_spec = format!("HEAD:{POLICY_PATH}");
        (git_bytes(root, &["show", &head_spec])?, "HEAD".to_string())
    };
    let digest = evidence::sha256(&bytes);
    let policy = Policy::parse(std::str::from_utf8(&bytes)?)?;
    Ok((policy, source, digest))
}

fn source(root: &Path, base_ref: &str) -> Result<(Source, Policy)> {
    let head = git(root, &["rev-parse", "HEAD^{commit}"])?;
    let tree = git(root, &["rev-parse", "HEAD^{tree}"])?;
    let base_sha = git(root, &["rev-parse", &format!("{base_ref}^{{commit}}")])?;
    let merge_base = git(root, &["merge-base", &base_sha, &head])?;
    let (policy, policy_source, policy_sha256) = effective_policy(root, &base_sha)?;
    let changed_paths = changed_paths(root, &merge_base)?;
    Ok((
        Source {
            head,
            tree,
            base_sha,
            merge_base,
            policy_source,
            policy_sha256,
            changed_paths,
        },
        policy,
    ))
}

fn clean_checkout(root: &Path, policy: &Policy) -> Result<()> {
    let tracked = git_bytes(root, &["status", "--porcelain=v1", "--untracked-files=no"])?;
    if !tracked.is_empty() {
        bail!("tracked checkout is dirty; commit changes before forge check/verify");
    }
    let untracked = git_bytes(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    for entry in untracked.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8(entry.to_vec()).context("non-UTF-8 untracked path")?;
        if !policy
            .allowed_untracked_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix))
        {
            bail!("untracked input {path}; commit or remove it before checking");
        }
    }
    Ok(())
}

fn environment(
    root: &Path,
    policy: &Policy,
    source: &Source,
    changed: &Path,
    changed_py: &Path,
    all: bool,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = policy.env.clone().into_iter().collect();
    let mut path: Vec<String> = policy
        .path_prepend
        .iter()
        .map(|dir| root.join(dir).to_string_lossy().into_owned())
        .collect();
    path.push(std::env::var("PATH").unwrap_or_default());
    let tools = root.join(".git/forge-tools");
    path.insert(0, tools.join("node_modules/.bin").display().to_string());
    path.insert(0, tools.join("pmd-bin-7.27.0/bin").display().to_string());
    env.push(("PATH".into(), path.join(":")));
    env.push(("FORGE_CHECK_TOOLS".into(), tools.display().to_string()));
    env.push((
        "FORGE_BIN".into(),
        root.join(".venv/bin/forge").display().to_string(),
    ));
    env.push((
        "UV_PROJECT_ENVIRONMENT".into(),
        root.join(".venv").display().to_string(),
    ));
    env.push(("PYTHONPATH".into(), root.join("src").display().to_string()));
    env.push(("FORGE_CHECK_BASE".into(), source.merge_base.clone()));
    env.push(("FORGE_CHECK_HEAD".into(), source.head.clone()));
    env.push(("FORGE_CHECK_CHANGED".into(), changed.display().to_string()));
    env.push((
        "FORGE_CHECK_CHANGED_PY".into(),
        changed_py.display().to_string(),
    ));
    env.push(("FORGE_CHECK_ALL".into(), if all { "1" } else { "0" }.into()));
    let run_dir = changed.parent().expect("changed file has parent");
    env.push((
        "PYTHONPYCACHEPREFIX".into(),
        run_dir.join("pycache").display().to_string(),
    ));
    env
}

fn clean_command(command: &str) -> String {
    let mut names = vec![
        "PYO3_NO_PYTHON".to_string(),
        "PYO3_CONFIG_FILE".to_string(),
        "PYO3_PYTHON".to_string(),
    ];
    for (key, _) in std::env::vars_os() {
        let key = key.to_string_lossy();
        if key.starts_with("GIT_") && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            names.push(key.into_owned());
        }
    }
    format!("unset {}; {command}", names.join(" "))
}

fn changed_python(root: &Path, merge_base: &str) -> Result<Vec<u8>> {
    git_bytes(
        root,
        &[
            "diff",
            "--name-only",
            "--diff-filter=ACMR",
            merge_base,
            "HEAD",
            "--",
            "*.py",
        ],
    )
}

fn run_dir(common: &Path) -> Result<PathBuf> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let dir = evidence::receipt_dir(common).join(format!("run-{now}-{}", std::process::id()));
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

fn acquire_lock(common: &Path, exclusive: bool) -> Result<File> {
    let dir = evidence::receipt_dir(common);
    fs::create_dir_all(&dir)?;
    let path = dir.join("lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    let kind = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    // SAFETY: flock is applied to this owned descriptor and released on drop.
    if unsafe { libc::flock(file.as_raw_fd(), kind | libc::LOCK_NB) } != 0 {
        bail!(
            "another forge check or verify is active ({})",
            path.display()
        );
    }
    Ok(file)
}

fn run_step(
    root: &Path,
    step: &Step,
    stage: &str,
    index: usize,
    env: &[(String, String)],
    dir: &Path,
) -> Result<StepRecord> {
    let log_file = format!("{index:02}-{stage}.log");
    let log = dir.join(&log_file);
    let start = Instant::now();
    let command = clean_command(&step.run);
    let outcome = run_shell(&ShellRun {
        command: &command,
        cwd: root,
        env,
        log: &log,
        timeout: Duration::from_secs(step.timeout_s),
    })?;
    let verdict = match outcome {
        Outcome::Passed => Verdict::Passed,
        Outcome::Failed(_) => Verdict::Failed,
        Outcome::TimedOut => Verdict::TimedOut,
    };
    println!(
        "  {:<7} {:<28} {:.1}s",
        format!("{verdict:?}"),
        step.name,
        start.elapsed().as_secs_f64()
    );
    if verdict != Verdict::Passed {
        eprintln!("    log {}\n{}", log.display(), tail(&log, 20));
    }
    Ok(StepRecord {
        stage: stage.into(),
        name: step.name.clone(),
        command: step.run.clone(),
        timeout_s: step.timeout_s,
        blocking: step.blocking,
        verdict,
        log_file: Some(log_file),
        log_sha256: Some(evidence::sha256(&fs::read(&log)?)),
    })
}

fn skipped(step: &Step, stage: &str) -> StepRecord {
    StepRecord {
        stage: stage.into(),
        name: step.name.clone(),
        command: step.run.clone(),
        timeout_s: step.timeout_s,
        blocking: step.blocking,
        verdict: Verdict::Skipped,
        log_file: None,
        log_sha256: None,
    }
}

fn execute(
    root: &Path,
    policy: &Policy,
    source: &Source,
    all: bool,
    dir: &Path,
) -> Result<(Vec<StepRecord>, String, String)> {
    let changed = dir.join("changed.txt");
    let changed_bytes = (source.changed_paths.join("\n") + "\n").into_bytes();
    fs::write(&changed, &changed_bytes)?;
    let changed_py = dir.join("changed-python.txt");
    let python_bytes = changed_python(root, &source.merge_base)?;
    fs::write(&changed_py, &python_bytes)?;
    let env = environment(root, policy, source, &changed, &changed_py, all);
    let mut records = Vec::new();
    let mut setup_ok = true;
    for step in &policy.setup {
        let select = all || selected(step, &source.changed_paths);
        let record = if setup_ok && select {
            run_step(root, step, "setup", records.len(), &env, dir)?
        } else {
            skipped(step, "setup")
        };
        if select {
            setup_ok &= record.verdict == Verdict::Passed;
        }
        records.push(record);
    }
    for step in &policy.checks {
        let select = all || selected(step, &source.changed_paths);
        let record = if select && setup_ok {
            run_step(root, step, "check", records.len(), &env, dir)?
        } else {
            skipped(step, "check")
        };
        records.push(record);
    }
    if fs::read(&changed)? != changed_bytes || fs::read(&changed_py)? != python_bytes {
        bail!("changed-path input files changed during local checks");
    }
    Ok((
        records,
        evidence::sha256(&changed_bytes),
        evidence::sha256(&python_bytes),
    ))
}

fn records_pass(policy: &Policy, source: &Source, all: bool, records: &[StepRecord]) -> bool {
    if records.len() != policy.setup.len() + policy.checks.len() {
        return false;
    }
    for (step, record) in policy.setup.iter().zip(records) {
        let selected = all || selected(step, &source.changed_paths);
        if selected && record.verdict != Verdict::Passed {
            return false;
        }
        if !selected && record.verdict != Verdict::Skipped {
            return false;
        }
    }
    let mut selected_count = 0;
    for (step, record) in policy.checks.iter().zip(&records[policy.setup.len()..]) {
        if all || selected(step, &source.changed_paths) {
            selected_count += 1;
            if record.verdict != Verdict::Passed && step.blocking {
                return false;
            }
        } else if record.verdict != Verdict::Skipped {
            return false;
        }
    }
    selected_count > 0
}

fn effective_all(requested: bool, changed: &[String]) -> bool {
    requested
        || changed.iter().any(|path| {
            path == "Makefile"
                || path == "conductor.mk"
                || path == "AGENTS.md"
                || path.starts_with(".forge/")
                || path.starts_with(".github/workflows/")
                || path.starts_with("native/forge/")
        })
}

pub fn run(args: CheckArgs) -> Result<u8> {
    let root = root()?;
    let common = common_dir(&root)?;
    let _lock = acquire_lock(&common, true)?;
    let base_ref = args
        .base_ref
        .unwrap_or_else(|| DEFAULT_BASE_REF.to_string());
    let (before, policy) = source(&root, &base_ref)?;
    if policy.base_ref != base_ref {
        bail!("policy base_ref differs from selected target");
    }
    clean_checkout(&root, &policy)?;
    let all = effective_all(args.all, &before.changed_paths);
    let dir = run_dir(&common)?;
    evidence::write_atomic(
        &evidence::receipt_dir(&common).join("latest"),
        dir.file_name()
            .context("run dir has no name")?
            .as_encoded_bytes(),
    )?;
    println!(
        "forge check: {} at {} against {} ({})",
        before.head, before.tree, base_ref, before.base_sha
    );
    let (steps, changed_file_sha256, changed_python_sha256) =
        execute(&root, &policy, &before, all, &dir)?;
    let (after, post_policy) = source(&root, &base_ref)?;
    clean_checkout(&root, &post_policy)?;
    if before != after {
        bail!("source, target ref, or policy changed during local check");
    }
    let passed = records_pass(&policy, &before, all, &steps);
    let receipt = Receipt {
        schema: evidence::SCHEMA,
        head: before.head,
        tree: before.tree,
        base_ref,
        base_sha: before.base_sha,
        merge_base: before.merge_base,
        policy_path: POLICY_PATH.into(),
        policy_source: before.policy_source,
        policy_sha256: before.policy_sha256,
        changed_paths: before.changed_paths,
        changed_file_sha256,
        changed_python_sha256,
        all,
        passed,
        steps,
    };
    let receipt_path = dir.join("receipt.json");
    let digest = evidence::save(&receipt_path, &receipt)?;
    println!(
        "forge check: {} receipt={} sha256={digest}",
        if passed { "PASS" } else { "FAIL" },
        receipt_path.display()
    );
    Ok(if passed { 0 } else { 1 })
}

fn default_receipt(root: &Path) -> Result<PathBuf> {
    let dir = evidence::receipt_dir(&common_dir(root)?);
    let latest = fs::read_to_string(dir.join("latest"))?;
    let name = latest.trim();
    if !name.starts_with("run-") || name.contains('/') || name.contains('\\') {
        bail!("invalid local check latest pointer");
    }
    Ok(dir.join(name).join("receipt.json"))
}

fn check_records(policy: &Policy, source: &Source, receipt: &Receipt) -> Result<()> {
    let expected: Vec<_> = policy
        .setup
        .iter()
        .map(|step| ("setup", step))
        .chain(policy.checks.iter().map(|step| ("check", step)))
        .collect();
    if receipt.steps.len() != expected.len() {
        bail!("receipt step count differs from policy");
    }
    for ((stage, step), record) in expected.into_iter().zip(&receipt.steps) {
        if record.stage != stage
            || record.name != step.name
            || record.command != step.run
            || record.timeout_s != step.timeout_s
            || record.blocking != step.blocking
        {
            bail!("receipt step differs from policy: {}", step.name);
        }
    }
    if !receipt.passed || !records_pass(policy, source, receipt.all, &receipt.steps) {
        bail!("receipt does not prove a passing local check");
    }
    Ok(())
}

pub fn verify(args: VerifyArgs) -> Result<u8> {
    let root = root()?;
    let _lock = acquire_lock(&common_dir(&root)?, false)?;
    let latest = default_receipt(&root)?;
    let path = match args.receipt {
        Some(path) if path.canonicalize()? == latest.canonicalize()? => path,
        Some(_) => bail!("only the latest local check attempt may be verified"),
        None => latest,
    };
    let (receipt, digest) = evidence::load(&path)?;
    if receipt.schema != evidence::SCHEMA || receipt.policy_path != POLICY_PATH {
        bail!("unsupported local check receipt schema or policy path");
    }
    if args.require_all && !receipt.all {
        bail!("receipt did not run every declared check");
    }
    if receipt.base_ref != args.base_ref {
        bail!("receipt targets a different integration ref");
    }
    let (current, policy) = source(&root, &receipt.base_ref)?;
    clean_checkout(&root, &policy)?;
    if receipt.head != current.head
        || receipt.tree != current.tree
        || receipt.base_sha != current.base_sha
        || receipt.merge_base != current.merge_base
        || receipt.policy_source != current.policy_source
        || receipt.policy_sha256 != current.policy_sha256
        || receipt.changed_paths != current.changed_paths
    {
        bail!("receipt is stale for current HEAD, target ref, changed paths, or policy");
    }
    if receipt.all != effective_all(receipt.all, &current.changed_paths) {
        bail!("receipt skipped mandatory full selection for infrastructure changes");
    }
    let dir = path.parent().context("receipt has no parent")?;
    let changed_bytes = (current.changed_paths.join("\n") + "\n").into_bytes();
    let python_bytes = changed_python(&root, &current.merge_base)?;
    if receipt.changed_file_sha256 != evidence::sha256(&changed_bytes)
        || receipt.changed_python_sha256 != evidence::sha256(&python_bytes)
        || fs::read(dir.join("changed.txt"))? != changed_bytes
        || fs::read(dir.join("changed-python.txt"))? != python_bytes
    {
        bail!("changed-path input files do not match the committed source");
    }
    check_records(&policy, &current, &receipt)?;
    evidence::validate_logs(&path, &receipt)?;
    println!(
        "forge verify: PASS head={} receipt={} sha256={digest}",
        current.head,
        path.display()
    );
    Ok(0)
}

#[cfg(test)]
#[path = "local_check_tests.rs"]
mod tests;
