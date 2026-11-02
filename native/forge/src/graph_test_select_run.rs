//! Explicit guest execution under one bounded native process-group supervisor.
use super::{SelectArgs, Selection};
use crate::{candidate_cargo_lint_cli::runtime::Runtime, candidate_contract_run};
use anyhow::{ensure, Context, Result};
use clap::Args;
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Args)]
pub struct WorkerArgs {
    #[arg(long)]
    pub repo: PathBuf,
    #[arg(long)]
    pub runtime: PathBuf,
    #[arg(long)]
    pub python: Option<PathBuf>,
    #[arg(long = "target")]
    pub targets: Vec<String>,
    #[arg(long = "manifest")]
    pub manifests: Vec<PathBuf>,
}

/// Executed only as a child of the advisory supervisor; existing Runtime owns
/// scratch/cache isolation and the contract runner builds selected source and Forge.
pub fn worker(args: WorkerArgs) -> Result<u8> {
    let runtime = Runtime::new(&args.repo, "test", true)?;
    let result = (|| {
        if !args.targets.is_empty() {
            let python = args
                .python
                .as_deref()
                .context("contracts require explicit --python")?;
            let evidence = candidate_contract_run::run(
                &runtime,
                &args.repo,
                &args.runtime,
                python,
                &args.targets,
            )?;
            println!(
                "{}",
                serde_json::json!({"graph_test_contract_evidence": evidence})
            );
        }
        for manifest in &args.manifests {
            ensure!(
                manifest.starts_with(&args.repo) && manifest.canonicalize()? == *manifest,
                "native manifest must be canonical and inside repository"
            );
            let mut command = runtime.command(Some(manifest))?;
            command
                .args([
                    "test",
                    "--offline",
                    "--locked",
                    "--all-targets",
                    "--manifest-path",
                ])
                .arg(manifest);
            if manifest == &args.repo.join("native/conductor-native/Cargo.toml") {
                let python = args
                    .python
                    .as_deref()
                    .context("selected conductor unit tests require explicit Python guest")?;
                let extensions = args.runtime.join("contract-extension");
                let forge = args.runtime.join("contract-forge");
                ensure!(extensions.join("conductor_native.so").is_file() && forge.is_file(),
                    "selected conductor unit tests require freshly provisioned compatibility artifacts");
                candidate_contract_run::environment(
                    &mut command,
                    python,
                    &args.repo,
                    &extensions,
                    &forge,
                )?;
                command.args(["--features", "python-compat-tests"]);
            }
            crate::child_exit_status::require_success(
                command.status()?,
                &format!("native tests {}", manifest.display()),
            )?;
        }
        Ok(0)
    })();
    let cleanup = runtime.finish();
    match (result, cleanup) {
        (Ok(code), Ok(())) => Ok(code),
        (Err(error), Ok(())) => match error.downcast_ref::<crate::child_exit_status::ChildExit>() {
            Some(child) => {
                eprintln!("{error:#}");
                Ok(crate::child_exit_status::code(child.status))
            }
            None => Err(error),
        },
        (Ok(_), Err(error)) => Err(error).context("clean native test runtime"),
        (Err(error), Err(cleanup)) => {
            Err(error).context(format!("runtime cleanup also failed: {cleanup:#}"))
        }
    }
}

pub(super) fn preflight(
    root: &Path,
    plan: &Selection,
    args: &SelectArgs,
) -> Result<Option<PathBuf>> {
    ensure!(
        plan.unsupported_tests.is_empty(),
        "selected tests lack a runner: {}",
        plan.unsupported_tests.join(", ")
    );
    let needs_python = !plan.selected_tests.is_empty()
        || !plan.contract_targets.is_empty()
        || plan
            .native_manifests
            .contains(&root.join("native/conductor-native/Cargo.toml"));
    let python = if needs_python {
        let path = args
            .python
            .as_ref()
            .context("selected guest tests require explicit --python /path/to/python")?;
        // Preserve venv executable spelling: canonicalizing its symlink loses the venv.
        ensure!(
            path.is_absolute() && path.is_file(),
            "--python must be an existing absolute interpreter path"
        );
        Some(path.clone())
    } else {
        None
    };
    if !plan.contract_targets.is_empty() || !plan.native_manifests.is_empty() {
        let path = args.runtime_dir.as_ref().context(
            "native tests require --runtime-dir outside repository, plus FORGE_CARGO_BIN",
        )?;
        let path = path
            .canonicalize()
            .context("resolve existing runtime parent")?;
        let metadata = fs::metadata(&path)?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o777 == 0o700
                && !path.starts_with(root)
                && !root.starts_with(&path),
            "runtime parent must be operator-owned mode 0700 outside the repository"
        );
        ensure!(
            std::env::var_os("FORGE_CARGO_BIN").is_some(),
            "FORGE_CARGO_BIN must name provisioned Cargo/rustc/rustdoc directory"
        );
    }
    Ok(python)
}

fn supervise(
    command: &mut Command,
    args: &SelectArgs,
    interrupts: &crate::process_signals::Interrupts,
) -> Result<u8> {
    let output = match crate::process_probe::run_interruptible(
        command,
        Duration::from_secs(args.timeout_seconds),
        1024 * 1024,
        interrupts,
    ) {
        Ok(output) => output,
        Err(error)
            if error
                .downcast_ref::<crate::process_probe::Interrupted>()
                .is_some() =>
        {
            return Ok(128u8.saturating_add(interrupts.signal()?.unwrap_or(libc::SIGINT) as u8));
        }
        Err(error) => return Err(error),
    };
    io::stdout().write_all(&output.stdout)?;
    io::stderr().write_all(&output.stderr)?;
    Ok(output
        .status
        .code()
        .map(|code| u8::try_from(code).unwrap_or(1))
        .unwrap_or_else(|| {
            use std::os::unix::process::ExitStatusExt;
            output
                .status
                .signal()
                .map(|signal| 128u8.saturating_add(signal as u8))
                .unwrap_or(1)
        }))
}

struct NativeRun {
    exit: u8,
    scratch: Option<PathBuf>,
}
impl NativeRun {
    fn finish(self, result: Result<u8>) -> Result<u8> {
        if let Some(scratch) = &self.scratch {
            if matches!(&result, Ok(0)) {
                fs::remove_dir_all(scratch).context("remove graph test scratch")?;
            } else {
                eprintln!("graph test scratch retained: {}", scratch.display());
            }
        }
        result
    }
}

fn native_tests(
    root: &Path,
    plan: &Selection,
    args: &SelectArgs,
    python: Option<&Path>,
    interrupts: &crate::process_signals::Interrupts,
) -> Result<NativeRun> {
    if plan.native_manifests.is_empty() && plan.contract_targets.is_empty() {
        return Ok(NativeRun {
            exit: 0,
            scratch: None,
        });
    }
    let parent = args
        .runtime_dir
        .as_ref()
        .context("runtime parent missing")?
        .canonicalize()?;
    let scratch = parent.join(crate::resource_id("graph-tests")?);
    fs::create_dir(&scratch)?;
    fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("graph-test-worker")
        .arg("--repo")
        .arg(root)
        .arg("--runtime")
        .arg(&scratch)
        .env("TMPDIR", &scratch);
    if let Some(path) = python {
        command.arg("--python").arg(path);
    }
    for target in &plan.contract_targets {
        command.arg("--target").arg(target);
    }
    for manifest in &plan.native_manifests {
        command.arg("--manifest").arg(manifest);
    }
    match supervise(&mut command, args, interrupts) {
        Ok(exit) => Ok(NativeRun {
            exit,
            scratch: Some(scratch),
        }),
        Err(error) => Err(error).context(format!("scratch retained: {}", scratch.display())),
    }
}

fn pytest_tests(
    root: &Path,
    plan: &Selection,
    args: &SelectArgs,
    python: Option<&Path>,
    native: &NativeRun,
    interrupts: &crate::process_signals::Interrupts,
) -> Result<u8> {
    if let Some(signal) = interrupts.signal()? {
        return Ok(128u8.saturating_add(signal as u8));
    }
    if plan.selected_tests.is_empty() {
        return Ok(native.exit);
    }
    let python = python.context("pytest Python missing")?;
    let mut command = Command::new(python);
    command
        .current_dir(root)
        .env_remove("PYTHONHOME")
        .env_remove("PYO3_CONFIG_FILE");
    if !plan.contract_targets.is_empty() {
        let scratch = native
            .scratch
            .as_ref()
            .context("compatibility runtime missing")?;
        let extensions = scratch.join("contract-extension");
        let forge = scratch.join("contract-forge");
        if !extensions.join("conductor_native.so").is_file() || !forge.is_file() {
            ensure!(
                native.exit != 0,
                "successful native worker omitted fresh guest artifacts"
            );
            return Ok(native.exit);
        }
        // Keep exactly the same built artifacts and source import root alive for
        // pytest. Never fall back to an installed extension or ambient Forge.
        candidate_contract_run::environment(&mut command, python, root, &extensions, &forge)?;
        command.env("TMPDIR", scratch);
    }
    command.args(["-m", "pytest", "-o", "addopts="]);
    if args.pytest_args.is_empty() {
        command.args(["-q", "--tb=short"]);
    } else {
        command.args(&args.pytest_args);
    }
    command.arg("--").args(&plan.selected_tests);
    let code = supervise(&mut command, args, interrupts)?;
    Ok(if interrupts.interrupted()? || native.exit == 0 {
        code
    } else {
        native.exit
    })
}

pub(super) fn execute(root: &Path, plan: &Selection, args: &SelectArgs) -> Result<u8> {
    let python = preflight(root, plan, args)?;
    let interrupts = crate::process_signals::Interrupts::install()?;
    if let Some(python) = &python {
        let mut probe = Command::new(python);
        probe
            .current_dir(root)
            .env_remove("PYTHONHOME")
            .env_remove("PYO3_CONFIG_FILE")
            .args([
                "-c",
                if plan.selected_tests.is_empty() {
                    "import sys; print(sys.version)"
                } else {
                    "import pytest; print(pytest.__version__)"
                },
            ]);
        let code = supervise(&mut probe, args, &interrupts)?;
        if code != 0 {
            eprintln!("Python guest prerequisite failed with status {code}");
            return Ok(code);
        }
    }
    let native = native_tests(root, plan, args, python.as_deref(), &interrupts)?;
    let result = pytest_tests(root, plan, args, python.as_deref(), &native, &interrupts);
    if plan.count == 0 {
        println!("No targeted tests selected.");
    }
    native.finish(result)
}
