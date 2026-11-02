//! Native Cargo package runner. A parent Stage owns sandbox/cgroup/deadlines.
//! This helper does not claim test discovery, changed-line coverage or approval.
use crate::{
    candidate_cargo_lint_cli::runtime::Runtime,
    candidate_inputs::{self, BoundFile},
    candidate_source::{Source, ToolBinding, TreeEntry},
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::ExitStatus,
};

#[derive(Args)]
pub struct CargoTestArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    base: String,
    #[arg(long)]
    tree: String,
    /// Captured, candidate-relative Cargo.toml.
    #[arg(long)]
    manifest: String,
    /// Repeat exact source/test inputs whose post-run bytes must remain bound.
    #[arg(long = "bind")]
    bind: Vec<String>,
    /// Instrument and export changed Rust source line coverage inside this Stage.
    #[arg(long)]
    coverage: bool,
    #[arg(long)]
    llvm_cov: Option<PathBuf>,
    #[arg(long)]
    llvm_profdata: Option<PathBuf>,
    /// Explicitly launch Cargo; absence is a read-only candidate preflight.
    #[arg(long)]
    execute: bool,
    /// Observe the granted Cargo version without running candidate code.
    #[arg(long, conflicts_with = "execute")]
    tool_version: bool,
}
pub(crate) struct Captured {
    source: Source,
    binding: ToolBinding,
    held: Vec<BoundFile>,
    manifest: PathBuf,
    paths: Vec<String>,
}
impl Captured {
    pub(crate) fn verify(&self) -> Result<()> {
        self.binding.verify(&self.source)?;
        for file in &self.held {
            file.verify()?;
        }
        Ok(())
    }
}
pub(crate) fn capture_shared(
    repo: &Path,
    base: &str,
    tree: &str,
    manifest: &str,
    bind: &[String],
) -> Result<Captured> {
    admit(&CargoTestArgs {
        repo: repo.into(),
        base: base.into(),
        tree: tree.into(),
        manifest: manifest.into(),
        bind: bind.to_vec(),
        coverage: false,
        llvm_cov: None,
        llvm_profdata: None,
        execute: false,
        tool_version: false,
    })
}
fn admit(args: &CargoTestArgs) -> Result<Captured> {
    if args.coverage {
        for path in [&args.llvm_cov, &args.llvm_profdata] {
            let path = path
                .as_ref()
                .context("coverage requires granted LLVM tools")?;
            ensure!(
                path.is_absolute()
                    && path.as_os_str().as_encoded_bytes().len() <= 4096
                    && std::fs::metadata(path)?.is_file(),
                "invalid granted LLVM tool"
            );
        }
    }
    ensure!(
        args.bind.len() <= 4095,
        "Cargo test binding count exceeds 4095"
    );
    let mut paths = BTreeSet::new();
    let mut bytes = 0usize;
    for path in args.bind.iter().chain(std::iter::once(&args.manifest)) {
        crate::candidate_source::safe_path(path.as_bytes())?;
        ensure!(
            !path.chars().any(char::is_control),
            "Cargo test binding path contains controls"
        );
        bytes = bytes
            .checked_add(path.len())
            .context("Cargo test path bytes overflow")?;
        ensure!(
            bytes <= 8 * 1024 * 1024,
            "Cargo test paths exceed byte limit"
        );
        ensure!(paths.insert(path.clone()), "duplicate Cargo test binding");
    }
    ensure!(
        Path::new(&args.manifest)
            .file_name()
            .is_some_and(|name| name == "Cargo.toml"),
        "Cargo test requires a Cargo.toml"
    );
    let root = args
        .repo
        .canonicalize()
        .context("resolve candidate workspace")?;
    let (source, _, binding) = Source::tool_context(&root, &args.base, &args.tree)?;
    let entries: BTreeMap<_, _> = source
        .list_tree(&args.tree)?
        .into_iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect();
    let held = capture(&root, &entries, &paths)?;
    let manifest = root.join(&args.manifest);
    let captured = Captured {
        source,
        binding,
        held,
        manifest,
        paths: paths.into_iter().collect(),
    };
    captured.verify()?;
    Ok(captured)
}
fn capture(
    root: &Path,
    entries: &BTreeMap<String, TreeEntry>,
    paths: &BTreeSet<String>,
) -> Result<Vec<BoundFile>> {
    let mut held = Vec::with_capacity(paths.len());
    let mut total = 0usize;
    for path in paths {
        let entry = entries
            .get(path)
            .with_context(|| format!("Cargo test binding absent from candidate: {path}"))?;
        ensure!(
            entry.object_type == "blob"
                && matches!(entry.mode.as_str(), "100644" | "100755")
                && entry.size.is_some_and(|size| size <= 1024 * 1024),
            "Cargo test binding must be a regular blob at most 1 MiB: {path}"
        );
        total = total
            .checked_add(entry.size.context("Cargo test size missing")? as usize)
            .context("Cargo test binding size overflow")?;
        ensure!(
            total <= 32 * 1024 * 1024,
            "Cargo test binding bytes exceed 32 MiB"
        );
        let (file, _) = candidate_inputs::snapshot(root, path, &entry.oid, 1024 * 1024)?;
        held.push(file);
    }
    Ok(held)
}
fn exit_code(status: ExitStatus) -> Result<u8> {
    let code = status.code().context("Cargo test terminated by signal")?;
    u8::try_from(code).context("Cargo test exit code is outside u8")
}
fn verify_and_finish(
    captured: &Captured,
    runtime: Runtime,
    status: Result<ExitStatus>,
) -> Result<u8> {
    let verified = captured.verify();
    let cleanup = runtime.finish();
    let mut failures = Vec::new();
    if let Err(error) = &status {
        failures.push(format!("Cargo test launch: {error:#}"));
    }
    if let Err(error) = &verified {
        failures.push(format!("captured input verification: {error:#}"));
    }
    if let Err(error) = &cleanup {
        failures.push(format!("private Cargo cleanup: {error:#}"));
    }
    ensure!(failures.is_empty(), "{}", failures.join("; "));
    exit_code(status?)
}
pub fn run(args: CargoTestArgs) -> Result<u8> {
    let captured = admit(&args)?;
    if !args.execute && !args.tool_version {
        captured.verify()?;
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"executed":false,"preimage_tree":args.tree,"manifest":args.manifest,"bound_files":captured.paths,"test_discovery_verified":false,"coverage_verified":false})
        );
        return Ok(0);
    }
    if args.coverage {
        let tmp = std::env::var_os("TMPDIR").context("private TMPDIR missing")?;
        crate::candidate_rust_coverage_run::private_runtime(Path::new(&tmp), &args.repo)?;
    }
    let runtime = Runtime::new(&args.repo, "test", !args.tool_version)?;
    if args.coverage {
        return coverage_run(&args, &captured, runtime);
    }
    let status = (|| -> Result<ExitStatus> {
        let mut command = runtime.command(if args.tool_version {
            None
        } else {
            Some(&captured.manifest)
        })?;
        if args.tool_version {
            command.arg("--version");
        } else {
            command
                .args(["test", "--offline", "--locked", "--manifest-path"])
                .arg(&captured.manifest)
                .arg("--all-targets");
        }
        command.status().context("run native Cargo test")
    })();
    verify_and_finish(&captured, runtime, status)
}
fn coverage_run(args: &CargoTestArgs, captured: &Captured, runtime: Runtime) -> Result<u8> {
    let result = (|| -> Result<()> {
        let tmp = std::env::var_os("TMPDIR").context("private TMPDIR missing")?;
        let parent =
            crate::candidate_rust_coverage_run::private_runtime(Path::new(&tmp), &args.repo)?;
        let rustc =
            PathBuf::from(std::env::var_os("FORGE_CARGO_BIN").context("Cargo bin missing")?)
                .join("rustc");
        let profdata = args
            .llvm_profdata
            .as_deref()
            .context("llvm-profdata missing")?;
        let cov = args.llvm_cov.as_deref().context("llvm-cov missing")?;
        let version = crate::candidate_rust_coverage_run::tool_versions(&rustc, profdata, cov)?;
        if args.tool_version {
            let mut command = runtime.command(None)?;
            command.arg("--version");
            ensure!(command.status()?.success(), "Cargo version failed");
            println!("{version}");
        } else {
            crate::candidate_rust_coverage_run::run(
                &runtime,
                &captured.manifest,
                &parent,
                profdata,
                cov,
            )?;
        }
        Ok(())
    })();
    let verified = captured.verify();
    let cleanup = runtime.finish();
    ensure!(
        result.is_ok() && verified.is_ok() && cleanup.is_ok(),
        "coverage run: {}; input verification: {}; cleanup: {}",
        result
            .err()
            .map(|e| format!("{e:#}"))
            .unwrap_or_else(|| "ok".into()),
        verified
            .err()
            .map(|e| format!("{e:#}"))
            .unwrap_or_else(|| "ok".into()),
        cleanup
            .err()
            .map(|e| format!("{e:#}"))
            .unwrap_or_else(|| "ok".into())
    );
    Ok(0)
}
