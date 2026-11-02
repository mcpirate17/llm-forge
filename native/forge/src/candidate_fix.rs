//! Explicit current-source fixes through a pinned native Ruff stdin filter.
#[path = "candidate_fix_config.rs"]
mod config;
#[path = "candidate_fix_execution.rs"]
mod execution;
use crate::{
    candidate_execution_workspace::Grants,
    candidate_inputs::{self, BoundFile},
    candidate_source::Source,
    patch,
    process_signals::Interrupts,
    source_file::{self, Snapshot, SourceRoot},
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde_json::json;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
const MAX_FILE: usize = 1024 * 1024;
const MAX_FILES: usize = 256;
const MAX_TOTAL: usize = 32 * MAX_FILE;
const RUFF_VERSION: &str = "ruff 0.16.5";

#[derive(Args)]
pub struct FixArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Owned external operator capabilities; programs.ruff selects the native ELF.
    #[arg(long)]
    grants: PathBuf,
    /// Exact SHA-256 of the operator-authorized executable (not a publisher signature).
    #[arg(long)]
    ruff_sha256: String,
    /// External JSON array of literal relative file paths, mutually exclusive with FILE.
    #[arg(long, conflicts_with = "paths")]
    paths_from: Option<PathBuf>,
    /// Pin any ancestor or extended config outside the source root: absolute PATH=SHA256.
    #[arg(long)]
    config_pin: Vec<String>,
    /// Execute and publish the complete successful batch; otherwise only admit inputs.
    #[arg(long)]
    execute: bool,
    /// Keep owned scratch after a failed filter for diagnosis.
    #[arg(long)]
    keep_failed: bool,
    /// Literal .py/.pyi files; pass after --. Directories/globs are refused.
    #[arg(value_name = "FILE", num_args = 0..)]
    paths: Vec<PathBuf>,
}
struct File {
    snapshot: Snapshot,
    binding: BoundFile,
}
struct Prepared {
    source: Source,
    root: SourceRoot,
    grants: Grants,
    tool: BoundFile,
    tool_path: PathBuf,
    files: Vec<File>,
    configs: config::Configs,
    paths_binding: Option<BoundFile>,
}
pub fn run(args: FixArgs) -> Result<u8> {
    let interrupts = args.execute.then(Interrupts::install).transpose()?;
    let prepared = prepare(&args)?;
    let mut output = json!({"schema_version":1,"operation":"candidate_fix", "executed":false,
        "supported_ruff":RUFF_VERSION,"configuration":"hierarchical_project_or_pinned_version_defaults",
        "ambient_user_configuration":false,"ruff_sha256":args.ruff_sha256,
        "grants_sha256":prepared.grants.digest(),"config_files":prepared.configs.paths(),
        "config_bindings":prepared.configs.evidence(),
        "files":prepared.files.iter().map(|file| json!({"path":file.snapshot.path(),
            "source_sha256":file.snapshot.sha256(),"bytes":file.snapshot.content().len()})).collect::<Vec<_>>()});
    let code = if let Some(interrupts) = interrupts {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(2)
            .build()?;
        let (code, stages, candidates) =
            runtime.block_on(execution::execute(&prepared, &interrupts, args.keep_failed))?;
        output["executed"] = json!(true);
        output["stages"] = json!(stages);
        if code == 0 {
            let (publish_code, reports) = publish(&prepared, candidates, &interrupts)?;
            output["edits"] = json!(reports);
            publish_code
        } else {
            code
        }
    } else {
        0
    };
    output["exit_code"] = json!(code);
    println!("{}", serde_json::to_string(&output)?);
    Ok(code)
}
fn publish(
    prepared: &Prepared,
    candidates: Vec<String>,
    interrupts: &Interrupts,
) -> Result<(u8, Vec<patch::Report>)> {
    if let Some(signal) = interrupts.signal()? {
        return Ok((execution::signal_code(signal), Vec::new()));
    }
    prepared.verify()?;
    ensure!(
        candidates.len() == prepared.files.len(),
        "filter batch result count mismatch"
    );
    let mut plans = Vec::new();
    for (file, candidate) in prepared.files.iter().zip(candidates) {
        if candidate != file.snapshot.content() {
            plans.push(patch::prepare_content(
                &prepared.root,
                file.snapshot.path(),
                file.snapshot.sha256(),
                &candidate,
                Some("python"),
            )?);
        }
    }
    if plans.is_empty() {
        return Ok((
            interrupts
                .signal()?
                .map(execution::signal_code)
                .unwrap_or(0),
            Vec::new(),
        ));
    }
    let interrupted = std::cell::Cell::new(None);
    let reports = patch::apply_many_monitored(
        &prepared.root,
        plans,
        || {
            if let Some(signal) = interrupts.signal()? {
                interrupted.set(Some(signal));
                anyhow::bail!("interrupted before source publication");
            }
            prepared.verify()
        },
        || {
            if let Some(signal) = interrupts.signal()? {
                if interrupted.get().is_none() {
                    interrupted.set(Some(signal));
                    anyhow::bail!("interrupted during source publication");
                }
            }
            prepared.tool.verify()?;
            prepared.configs.verify()
        },
    );
    if let Some(signal) = interrupted.get().or(interrupts.signal()?) {
        // Cleanup/rollback has already completed. Retain conflict/recovery detail.
        if let Err(error) = &reports {
            eprintln!("forge: interrupted batch: {error:#}");
        }
        return Ok((execution::signal_code(signal), reports.unwrap_or_default()));
    }
    Ok((0, reports?))
}
fn prepare(args: &FixArgs) -> Result<Prepared> {
    let source = Source::open(&args.repo)?;
    let root = SourceRoot::open(source.root())?;
    let grants = Grants::load(&args.grants, &source)?;
    let tool_path = grants
        .programs
        .get("ruff")
        .context("operator grants require programs.ruff")?
        .clone();
    let (tool, bytes) = candidate_inputs::external_with_tmp(
        &tool_path,
        &args.ruff_sha256,
        MAX_TOTAL,
        source.root(),
        Some(&grants.scratch_parent),
    )?;
    ensure!(
        bytes.starts_with(b"\x7fELF") && fs::metadata(&tool_path)?.mode() & 0o111 != 0,
        "Ruff must be an executable native ELF; Python launchers are unsupported"
    );
    let (paths, paths_binding) = paths(args)?;
    let files = admit_files(&root, source.root(), &paths)?;
    let configs = config::Configs::capture(source.root(), &paths, &grants, &args.config_pin)?;
    let prepared = Prepared {
        source,
        root,
        grants,
        tool,
        tool_path,
        files,
        configs,
        paths_binding,
    };
    prepared.verify()?;
    Ok(prepared)
}
fn paths(args: &FixArgs) -> Result<(Vec<PathBuf>, Option<BoundFile>)> {
    let (paths, held) = if let Some(path) = &args.paths_from {
        let absolute = path.canonicalize()?;
        ensure!(
            absolute == *path,
            "--paths-from must be an absolute canonical owned file"
        );
        let (held, bytes) = candidate_inputs::repository_live(path, 65536)?;
        (
            serde_json::from_slice::<Vec<PathBuf>>(&bytes)
                .context("paths file must be a JSON string array")?,
            Some(held),
        )
    } else {
        (args.paths.clone(), None)
    };
    ensure!(
        !paths.is_empty() && paths.len() <= MAX_FILES,
        "fix requires 1..256 explicit files"
    );
    Ok((paths, held))
}
fn admit_files(root: &SourceRoot, absolute: &Path, paths: &[PathBuf]) -> Result<Vec<File>> {
    let mut seen = BTreeSet::new();
    let mut files = Vec::with_capacity(paths.len());
    let mut total = 0usize;
    for path in paths {
        source_file::validate_relative(path)?;
        let text = path.to_str().context("source path must be UTF-8")?;
        ensure!(
            !text.starts_with('-')
                && !text.contains(['*', '?', '[', ']'])
                && !text.chars().any(char::is_control),
            "fix requires literal bounded file paths"
        );
        ensure!(
            matches!(
                path.extension().and_then(|v| v.to_str()),
                Some("py" | "pyi")
            ),
            "fix supports only .py/.pyi sources"
        );
        ensure!(seen.insert(path.clone()), "duplicate selected source");
        let (binding, bytes) = candidate_inputs::repository_live(&absolute.join(path), MAX_FILE)?;
        let metadata = fs::metadata(absolute.join(path))?;
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() } && metadata.mode() & 0o200 != 0,
            "selected source must be writable and owned by this user"
        );
        validate_encoding(&bytes)?;
        let snapshot = root.read(path)?;
        ensure!(
            snapshot.content().as_bytes() == bytes,
            "source changed during admission"
        );
        total = total
            .checked_add(bytes.len())
            .context("source byte overflow")?;
        ensure!(total <= MAX_TOTAL / 2, "selected source exceeds 16 MiB");
        files.push(File { snapshot, binding });
    }
    Ok(files)
}
fn validate_encoding(bytes: &[u8]) -> Result<()> {
    let source = std::str::from_utf8(bytes).context("fix requires exact UTF-8 sources")?;
    ensure!(
        !source.starts_with('\u{feff}') && !source.contains('\0'),
        "source BOM/NUL is unsupported"
    );
    static COOKIE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let cookie = COOKIE.get_or_init(|| {
        regex::Regex::new(r"^[ \t\x0c]*#.*?coding[:=][ \t]*([-_.a-zA-Z0-9]+)")
            .expect("literal PEP263 pattern")
    });
    for line in source.lines().take(2) {
        if let Some(found) = cookie.captures(line) {
            ensure!(
                matches!(
                    found[1].to_ascii_lowercase().as_str(),
                    "utf-8" | "utf8" | "utf_8"
                ),
                "non-UTF-8 source coding cookie is unsupported"
            );
        }
    }
    Ok(())
}
impl Prepared {
    fn verify(&self) -> Result<()> {
        ensure!(
            self.root.identity()? == {
                use std::os::unix::fs::MetadataExt;
                let m = fs::metadata(self.source.root())?;
                (m.dev(), m.ino())
            },
            "source root identity changed"
        );
        self.tool.verify()?;
        self.configs.verify()?;
        if let Some(binding) = &self.paths_binding {
            binding.verify()?;
        }
        for file in &self.files {
            file.binding.verify()?;
        }
        Ok(())
    }
}
#[cfg(test)]
#[path = "candidate_fix_tests.rs"]
mod tests;
