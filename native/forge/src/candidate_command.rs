//! Pure preparation of untrusted candidate-policy argv. This grants no executable,
//! filesystem, environment, network or process authority and performs no I/O.
use crate::{candidate_policy::Policy, candidate_source::Candidate};
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

const MAX_CHECKS: usize = 256;
const MAX_ARG_COUNT: usize = 4096;
const MAX_TEMPLATE_ARG: usize = 4096;
const MAX_ARG_BYTES: usize = 131_071;
const MAX_VECTOR_BYTES: usize = 1024 * 1024;
const MAX_FILES: usize = 100_000;
const MAX_FILE_LIST_BYTES: usize = 8 * 1024 * 1024;
const MAX_WORK: usize = 64 * 1024 * 1024;
pub const MAX_OUTPUT_CHARS: usize = 262_144;

/// Extracted from Policy::active_checks, not a second policy parser. Templates
/// remain untrusted; even argv[0] must be matched to a separate operator grant.
#[derive(Clone, Debug)]
pub struct ExternalCheck {
    pub check_id: String,
    command: Vec<String>,
    version_command: Vec<String>,
    pub max_output_chars: usize,
}
/// Trusted caller supplies these literal substitutions; no PATH or interpreter
/// discovery is performed. Paths are not normalized or opened by this module.
pub struct OperatorInputs<'a> {
    pub repo: &'a Path,
    pub snapshot: &'a Path,
    pub runtime_dir: &'a Path,
    pub python: Option<&'a str>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFiles {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedArgv {
    pub argv: Vec<String>,
    /// Caller writes these bytes before this stage, using confined exclusive I/O.
    pub changed_files: Option<ChangedFiles>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedCheck {
    pub check_id: String,
    pub command: PreparedArgv,
    pub version: PreparedArgv,
    pub max_output_chars: usize,
}
struct Work {
    used: usize,
    maximum: usize,
}
impl Work {
    fn add(&mut self, amount: usize) -> Result<()> {
        self.used = self
            .used
            .checked_add(amount)
            .context("command expansion work overflow")?;
        ensure!(
            self.used <= self.maximum,
            "command expansion work limit exceeded"
        );
        Ok(())
    }
}
fn text(value: &str, maximum: usize, label: &str) -> Result<()> {
    ensure!(
        value.len() <= maximum && !value.contains('\0'),
        "{label} exceeds byte limit or contains NUL"
    );
    Ok(())
}
fn identity(id: &str, maximum: usize) -> Result<()> {
    ensure!(
        !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control),
        "invalid external check ID"
    );
    ensure!(
        (1..=MAX_OUTPUT_CHARS).contains(&maximum),
        "external max_output_chars must be 1..262144"
    );
    Ok(())
}
fn vector(value: &Value, label: &str) -> Result<Vec<String>> {
    let values = value
        .as_array()
        .with_context(|| format!("normalized {label} must be an argv array"))?;
    ensure!(
        !values.is_empty() && values.len() <= MAX_ARG_COUNT,
        "normalized {label} argv count must be 1..4096"
    );
    let mut total = 0_usize;
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        let value = value
            .as_str()
            .with_context(|| format!("normalized {label} argv entries must be strings"))?;
        text(value, MAX_TEMPLATE_ARG, label)?;
        total = total
            .checked_add(value.len())
            .and_then(|n| n.checked_add(1))
            .context("template argv byte overflow")?;
        ensure!(
            total <= MAX_VECTOR_BYTES,
            "normalized {label} argv byte limit exceeded"
        );
        result.push(value.to_owned());
    }
    Ok(result)
}
impl ExternalCheck {
    pub fn from_normalized(value: &Value) -> Result<Option<Self>> {
        match value.get("kind").and_then(Value::as_str) {
            Some("builtin") => return Ok(None),
            Some("command") => {}
            _ => anyhow::bail!("normalized check kind must be builtin or command"),
        }
        let id = value
            .get("check_id")
            .and_then(Value::as_str)
            .context("normalized check ID missing")?;
        let max_output = value
            .get("max_output_chars")
            .and_then(Value::as_u64)
            .context("normalized max_output_chars must be an integer")?;
        ensure!(
            (1..=MAX_OUTPUT_CHARS as u64).contains(&max_output),
            "external max_output_chars must be 1..262144"
        );
        identity(id, usize::try_from(max_output)?)?;
        Ok(Some(Self {
            check_id: id.into(),
            command: vector(&value["command"], "command")?,
            version_command: vector(&value["version_command"], "version_command")?,
            max_output_chars: usize::try_from(max_output)?,
        }))
    }
}
pub fn selected_external(policy: &Policy, profile: &str) -> Result<Vec<ExternalCheck>> {
    let active = policy.active_checks(profile)?;
    let mut selected = Vec::new();
    for check in &active {
        if check["kind"] == "command" {
            ensure!(
                selected.len() < MAX_CHECKS,
                "selected external check count exceeds 256"
            );
        }
        if let Some(check) = ExternalCheck::from_normalized(check)? {
            selected.push(check);
        }
    }
    Ok(selected)
}
fn path_text<'a>(path: &'a Path, label: &str) -> Result<&'a str> {
    let value = path
        .to_str()
        .with_context(|| format!("operator {label} path must be UTF-8"))?;
    text(value, 4096, label)?;
    Ok(value)
}
fn file_bytes(files: &[String], work: &mut Work) -> Result<usize> {
    ensure!(
        files.len() <= MAX_FILES,
        "changed file count exceeds 100000"
    );
    let mut bytes = 0_usize;
    for path in files {
        ensure!(
            !path.contains(['\n', '\r']),
            "changed-file list paths must not contain newlines"
        );
        crate::candidate_source::safe_path(path.as_bytes())?;
        work.add(path.len())?;
        bytes = bytes
            .checked_add(path.len())
            .and_then(|n| n.checked_add(1))
            .context("changed-file list byte overflow")?;
        ensure!(
            bytes <= MAX_FILE_LIST_BYTES,
            "changed-file list exceeds 8 MiB"
        );
    }
    Ok(bytes)
}
fn artifact(
    runtime: &Path,
    id: &str,
    files: &[String],
    bytes: usize,
    work: &mut Work,
) -> Result<ChangedFiles> {
    ensure!(
        !id.contains(['/', '\\']),
        "changed-file list check ID must be a basename component"
    );
    let name = format!("changed-files-{id}.txt");
    ensure!(
        name.len() <= 255,
        "changed-file list filename exceeds component limit"
    );
    let path = runtime.join(name);
    path_text(&path, "changed-file list")?;
    work.add(bytes)?;
    let mut output = Vec::with_capacity(bytes);
    for path in files {
        output.extend_from_slice(path.as_bytes());
        output.push(b'\n');
    }
    Ok(ChangedFiles {
        path,
        bytes: output,
    })
}
fn replace(current: String, token: &str, value: &str, work: &mut Work) -> Result<String> {
    work.add(current.len())?;
    let count = current.match_indices(token).count();
    if count == 0 {
        return Ok(current);
    }
    let removed = count
        .checked_mul(token.len())
        .context("command replacement byte overflow")?;
    let added = count
        .checked_mul(value.len())
        .context("command replacement byte overflow")?;
    let length = current
        .len()
        .checked_sub(removed)
        .and_then(|n| n.checked_add(added))
        .context("command replacement byte overflow")?;
    ensure!(
        length <= MAX_ARG_BYTES,
        "expanded argument byte limit exceeded"
    );
    work.add(
        current
            .len()
            .checked_add(length)
            .context("command replacement work overflow")?,
    )?;
    let mut result = String::with_capacity(length);
    let mut previous = 0;
    for (index, _) in current.match_indices(token) {
        result.push_str(&current[previous..index]);
        result.push_str(value);
        previous = index + token.len();
    }
    result.push_str(&current[previous..]);
    Ok(result)
}
fn push(argv: &mut Vec<String>, value: String, total: &mut usize, work: &mut Work) -> Result<()> {
    ensure!(
        argv.len() < MAX_ARG_COUNT,
        "expanded argv count exceeds 4096"
    );
    text(&value, MAX_ARG_BYTES, "expanded argument")?;
    *total = total
        .checked_add(value.len())
        .and_then(|n| n.checked_add(1))
        .context("expanded argv byte overflow")?;
    ensure!(*total <= MAX_VECTOR_BYTES, "expanded argv exceeds 1 MiB");
    work.add(value.len() + 1)?;
    argv.push(value);
    Ok(())
}
struct Substitutions<'a> {
    repo: &'a str,
    snapshot: &'a str,
    base: &'a str,
    tree: &'a str,
    python: Option<&'a str>,
}
fn expand(
    template: &[String],
    sub: &Substitutions<'_>,
    files: &[String],
    changed: Option<&ChangedFiles>,
    work: &mut Work,
) -> Result<Vec<String>> {
    let mut argv = Vec::new();
    let mut total = 0;
    for item in template {
        if item == "{files}" {
            ensure!(
                files.len() <= MAX_ARG_COUNT.saturating_sub(argv.len()),
                "expanded argv count exceeds 4096"
            );
            for path in files {
                push(&mut argv, path.clone(), &mut total, work)?;
            }
            continue;
        }
        work.add(item.len())?;
        let mut current = item.clone();
        for (token, value) in [
            ("{repo}", sub.repo),
            ("{snapshot}", sub.snapshot),
            ("{base}", sub.base),
            ("{tree}", sub.tree),
        ] {
            current = replace(current, token, value, work)?;
        }
        if current.contains("{python}") {
            let python = sub
                .python
                .context("{python} requires an explicit operator-granted executable")?;
            current = replace(current, "{python}", python, work)?;
        }
        if let Some(changed) = changed {
            current = replace(
                current,
                "{changed_files_file}",
                path_text(&changed.path, "changed-file list")?,
                work,
            )?;
        }
        push(&mut argv, current, &mut total, work)?;
    }
    ensure!(
        argv.first().is_some_and(|program| !program.is_empty()),
        "expanded argv must have a nonempty executable argument"
    );
    Ok(argv)
}
fn stage(
    template: &[String],
    check: &ExternalCheck,
    inputs: &OperatorInputs<'_>,
    sub: &Substitutions<'_>,
    files: &[String],
    bytes: usize,
    work: &mut Work,
) -> Result<PreparedArgv> {
    let changed = if template
        .iter()
        .any(|item| item.contains("{changed_files_file}"))
    {
        Some(artifact(
            inputs.runtime_dir,
            &check.check_id,
            files,
            bytes,
            work,
        )?)
    } else {
        None
    };
    let argv = expand(template, sub, files, changed.as_ref(), work)?;
    Ok(PreparedArgv {
        argv,
        changed_files: changed,
    })
}
pub fn prepare(
    check: &ExternalCheck,
    candidate: &Candidate,
    inputs: &OperatorInputs<'_>,
    files: &[String],
) -> Result<PreparedCheck> {
    prepare_with_work(check, candidate, inputs, files, MAX_WORK)
}
fn prepare_with_work(
    check: &ExternalCheck,
    candidate: &Candidate,
    inputs: &OperatorInputs<'_>,
    files: &[String],
    maximum: usize,
) -> Result<PreparedCheck> {
    let mut work = Work { used: 0, maximum };
    identity(&check.check_id, check.max_output_chars)?;
    let repo = path_text(inputs.repo, "repository")?;
    let snapshot = path_text(inputs.snapshot, "snapshot")?;
    path_text(inputs.runtime_dir, "runtime")?;
    let base = candidate
        .base_commit_oid
        .as_deref()
        .filter(|v| !v.is_empty())
        .unwrap_or(&candidate.base_tree_oid);
    text(base, 4096, "candidate base")?;
    text(&candidate.tree_oid, 4096, "candidate tree")?;
    if let Some(python) = inputs.python {
        text(python, 4096, "operator Python executable")?;
    }
    let bytes = file_bytes(files, &mut work)?;
    let sub = Substitutions {
        repo,
        snapshot,
        base,
        tree: &candidate.tree_oid,
        python: inputs.python,
    };
    let command = stage(&check.command, check, inputs, &sub, files, bytes, &mut work)?;
    // Legacy probes receive files=(): a whole {files} argument disappears,
    // embedded {files} stays literal, and any requested list file is empty.
    let version = stage(
        &check.version_command,
        check,
        inputs,
        &sub,
        &[],
        0,
        &mut work,
    )?;
    Ok(PreparedCheck {
        check_id: check.check_id.clone(),
        command,
        version,
        max_output_chars: check.max_output_chars,
    })
}

#[cfg(test)]
mod tests;
