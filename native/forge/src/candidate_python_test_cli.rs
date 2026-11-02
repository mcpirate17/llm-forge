//! Native helper for one pytest shard. The parent Stage owns sandbox, cgroup,
//! deadline, and output capture. The interpreter must come from operator grants.
use crate::{
    candidate_inputs::{self, BoundFile},
    candidate_source::{Source, ToolBinding},
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Args)]
pub struct PythonTestArgs {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    base: String,
    #[arg(long)]
    tree: String,
    #[arg(long)]
    interpreter: PathBuf,
    #[arg(long)]
    pytest: PathBuf,
    #[arg(long)]
    coverage_tool: Option<PathBuf>,
    #[arg(long = "test")]
    tests: Vec<String>,
    #[arg(long = "bind")]
    bindings: Vec<String>,
    #[arg(long)]
    coverage: bool,
    #[arg(long)]
    execute: bool,
    #[arg(long, conflicts_with = "execute")]
    tool_version: bool,
}
struct Inputs {
    source: Source,
    binding: ToolBinding,
    files: Vec<BoundFile>,
}
fn tool(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && path.as_os_str().as_encoded_bytes().len() <= 4096
            && !path
                .as_os_str()
                .as_encoded_bytes()
                .iter()
                .any(u8::is_ascii_control),
        "invalid granted test tool path"
    );
    ensure!(
        std::fs::metadata(path)?.is_file(),
        "granted test tool is not a regular file"
    );
    Ok(())
}
impl Inputs {
    fn verify(&self) -> Result<()> {
        self.binding.verify(&self.source)?;
        for file in &self.files {
            file.verify()?;
        }
        Ok(())
    }
}
fn admit(args: &PythonTestArgs) -> Result<Inputs> {
    ensure!(
        args.interpreter.is_absolute() && args.pytest.is_absolute(),
        "interpreter and pytest must be operator-granted absolute paths"
    );
    ensure!(
        !args.coverage || args.coverage_tool.as_ref().is_some_and(|p| p.is_absolute()),
        "coverage requires an operator-granted absolute entrypoint"
    );
    tool(&args.interpreter)?;
    tool(&args.pytest)?;
    if args.coverage {
        tool(
            args.coverage_tool
                .as_deref()
                .context("coverage tool missing")?,
        )?;
    }
    ensure!(
        !args.tests.is_empty() && args.tests.len() <= 4096 && args.bindings.len() <= 4096,
        "invalid Python test selection count"
    );
    let root = args.repo.canonicalize()?;
    let (source, _, binding) = Source::tool_context(&root, &args.base, &args.tree)?;
    let entries: BTreeMap<_, _> = source
        .list_tree(&args.tree)?
        .into_iter()
        .map(|row| (row.path.clone(), row))
        .collect();
    let mut paths = BTreeSet::new();
    let mut bytes = 0usize;
    for path in args.tests.iter().chain(&args.bindings) {
        crate::candidate_source::safe_path(path.as_bytes())?;
        ensure!(
            !path.chars().any(char::is_control),
            "test binding has controls"
        );
        bytes = bytes
            .checked_add(path.len())
            .context("test path overflow")?;
        ensure!(bytes <= 8 * 1024 * 1024, "test path budget exceeded");
        paths.insert(path.clone());
    }
    let mut held = Vec::new();
    let mut total = 0usize;
    for path in paths {
        let entry = entries
            .get(&path)
            .context("Python test input is absent from captured tree")?;
        ensure!(
            entry.object_type == "blob" && matches!(entry.mode.as_str(), "100644" | "100755"),
            "Python test input is not a regular blob"
        );
        let len = entry.size.context("Python test input size missing")?;
        ensure!(len <= 1024 * 1024, "Python test input too large");
        total = total
            .checked_add(len as usize)
            .context("test byte overflow")?;
        ensure!(total <= 32 * 1024 * 1024, "test input bytes exceed limit");
        held.push(candidate_inputs::snapshot(&root, &path, &entry.oid, 1024 * 1024)?.0);
    }
    let input = Inputs {
        source,
        binding,
        files: held,
    };
    input.verify()?;
    Ok(input)
}
fn command(args: &PythonTestArgs) -> Command {
    let mut cmd = Command::new(&args.interpreter);
    cmd.current_dir(&args.repo)
        // Preserve the legacy runner's explicit captured-root import path.
        .env("PYTHONPATH", &args.repo)
        .env_remove("PYTEST_ADDOPTS")
        .env_remove("COVERAGE_RCFILE");
    cmd
}
fn success(mut cmd: Command, action: &str) -> Result<()> {
    let status = cmd.status().with_context(|| format!("launch {action}"))?;
    ensure!(status.success(), "{action} failed: {status}");
    Ok(())
}
fn runtime(args: &PythonTestArgs) -> Result<PathBuf> {
    let tmp = std::env::var_os("TMPDIR").context("private TMPDIR required")?;
    let tmp = Path::new(&tmp);
    ensure!(
        tmp.is_absolute() && tmp.file_name().is_some_and(|n| n == "tmp"),
        "invalid private TMPDIR"
    );
    let runtime = tmp.parent().context("runtime parent missing")?;
    ensure!(
        !runtime.starts_with(&args.repo) && !args.repo.starts_with(runtime),
        "runtime overlaps candidate"
    );
    Ok(runtime.to_path_buf())
}
fn run_shard(args: &PythonTestArgs) -> Result<()> {
    let runtime = runtime(args)?;
    let config_path = runtime.join("coverage.ini");
    const CONFIG: &[u8] = b"[run]\nbranch = True\n";
    if args.coverage {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&config_path)?;
        file.write_all(CONFIG)?;
        file.sync_all()?;
    }
    let mut cmd = command(args);
    cmd.arg("-P");
    if args.coverage {
        cmd.arg(
            args.coverage_tool
                .as_ref()
                .context("coverage tool missing")?,
        )
        .args(["run", "--branch"])
        .arg(format!("--rcfile={}", config_path.display()))
        .arg(format!(
            "--data-file={}",
            runtime.join("coverage.data").display()
        ));
    }
    cmd.arg(&args.pytest).args([
        "-q",
        "-o",
        "addopts=",
        "-p",
        "no:cacheprovider",
        "-rfE",
        "--",
    ]);
    cmd.args(&args.tests);
    success(cmd, "pytest")?;
    if args.coverage {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&config_path)?;
        use std::io::Read;
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.nlink() == 1 && metadata.len() == CONFIG.len() as u64,
            "coverage configuration changed"
        );
        let mut data = Vec::new();
        file.take(CONFIG.len() as u64 + 1).read_to_end(&mut data)?;
        ensure!(data == CONFIG, "coverage configuration changed");
        let mut json = command(args);
        json.arg("-P")
            .arg(
                args.coverage_tool
                    .as_ref()
                    .context("coverage tool missing")?,
            )
            .arg("json")
            .arg(format!("--rcfile={}", config_path.display()))
            .arg(format!(
                "--data-file={}",
                runtime.join("coverage.data").display()
            ))
            .arg("-o")
            .arg(runtime.join("coverage.json"))
            .arg("--quiet");
        success(json, "coverage JSON")?;
    }
    Ok(())
}
pub fn run(args: PythonTestArgs) -> Result<u8> {
    let inputs = admit(&args)?;
    if !args.execute && !args.tool_version {
        inputs.verify()?;
        return Ok(0);
    }
    let run = (|| -> Result<()> {
        if args.tool_version {
            let mut interpreter = command(&args);
            interpreter.arg("--version");
            success(interpreter, "Python version")?;
            let mut version = command(&args);
            version.arg("-P").arg(&args.pytest).arg("--version");
            success(version, "pytest version")?;
            if args.coverage {
                let mut version = command(&args);
                version
                    .arg("-P")
                    .arg(
                        args.coverage_tool
                            .as_ref()
                            .context("coverage tool missing")?,
                    )
                    .arg("--version");
                success(version, "coverage version")?;
            }
            Ok(())
        } else {
            run_shard(&args)
        }
    })();
    let checked = inputs.verify();
    if let Err(error) = checked {
        anyhow::bail!("captured test input changed: {error:#}; run: {run:?}");
    }
    run?;
    Ok(0)
}
