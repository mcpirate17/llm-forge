//! Guest-only invariant measurement. All subprocesses remain in the parent
//! supervised Stage; admission is performed by the sealed native owner.
use crate::{
    candidate_inputs::{self, BoundFile},
    candidate_source::{Source, ToolBinding},
};
use anyhow::{ensure, Context, Result};
use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
};
const LIMIT: usize = 4 * 1024 * 1024;
const CONFIG: &[u8] = b"[run]\nbranch = True\n[report]\nexclude_lines =\nexclude_also =\n";
#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    source_root: PathBuf,
    #[arg(long)]
    base: String,
    #[arg(long)]
    tree: String,
    #[arg(long)]
    python: PathBuf,
    #[arg(long)]
    pytest: PathBuf,
    #[arg(long)]
    coverage: PathBuf,
    #[arg(long)]
    nodeid: String,
    #[arg(long)]
    torch_pin: String,
    #[arg(long)]
    phase: String,
    #[arg(long)]
    probe_sha256: String,
    #[arg(long)]
    probe_path: PathBuf,
    #[arg(long)]
    execute: bool,
}
struct Inputs {
    source: Source,
    binding: ToolBinding,
    files: Vec<BoundFile>,
}
impl Inputs {
    fn verify(&self) -> Result<()> {
        self.binding.verify(&self.source)?;
        for f in &self.files {
            f.verify()?;
        }
        Ok(())
    }
}
fn tools(args: &Args) -> Result<()> {
    for path in [&args.python, &args.pytest, &args.coverage] {
        ensure!(
            path.is_absolute()
                && path.as_os_str().as_encoded_bytes().len() <= 4096
                && !path
                    .as_os_str()
                    .as_encoded_bytes()
                    .iter()
                    .any(u8::is_ascii_control)
                && std::fs::metadata(path)?.is_file(),
            "invalid granted invariant tool"
        );
    }
    Ok(())
}
pub(crate) fn node_module(nodeid: &str) -> Result<&str> {
    ensure!(
        nodeid.len() <= 4096 && !nodeid.chars().any(char::is_control),
        "invalid invariant nodeid"
    );
    let (module, node) = nodeid
        .split_once("::")
        .context("invariant nodeid must name a test")?;
    crate::candidate_source::safe_path(module.as_bytes())?;
    ensure!(
        module.ends_with(".py") && !node.is_empty() && node.split("::").all(|v| !v.is_empty()),
        "invalid invariant nodeid"
    );
    Ok(module)
}
fn capture(args: &Args) -> Result<Inputs> {
    tools(args)?;
    node_module(&args.nodeid)?;
    ensure!(
        matches!(args.phase.as_str(), "baseline" | "node")
            && !args.torch_pin.is_empty()
            && args.torch_pin.len() <= 256
            && !args.torch_pin.chars().any(char::is_control),
        "invalid invariant phase or torch pin"
    );
    ensure!(
        args.probe_sha256.len() == 64
            && args
                .probe_sha256
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v)),
        "invalid invariant probe hash"
    );
    let root = args.repo.canonicalize()?;
    ensure!(
        args.source_root.is_absolute()
            && args.source_root.canonicalize()? == args.source_root
            && args.source_root.is_dir()
            && args.source_root != root,
        "invalid original source root"
    );
    let (source, _, binding) = Source::tool_context(&root, &args.base, &args.tree)?;
    let entries = source.list_tree(&args.tree)?;
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut module = false;
    for row in entries {
        if row.path.ends_with(".pyc") || row.path.ends_with(".pyo") {
            anyhow::bail!("tracked Python bytecode is unsupported for invariant measurement");
        }
        if !row.path.ends_with(".py") {
            continue;
        }
        ensure!(
            row.object_type == "blob" && matches!(row.mode.as_str(), "100644" | "100755"),
            "invariant Python inputs must be regular blobs"
        );
        let size = row.size.context("invariant source size missing")?;
        ensure!(size <= 1024 * 1024, "invariant source exceeds 1 MiB");
        total = total
            .checked_add(size)
            .context("invariant source byte overflow")?;
        ensure!(
            total <= 32 * 1024 * 1024 && files.len() < 4096,
            "invariant source inventory exceeds limit"
        );
        module |= row.path == node_module(&args.nodeid)?;
        files.push(candidate_inputs::snapshot(&root, &row.path, &row.oid, 1024 * 1024)?.0);
    }
    ensure!(module, "invariant test module missing from candidate tree");
    let result = Inputs {
        source,
        binding,
        files,
    };
    result.verify()?;
    Ok(result)
}
fn root(args: &Args) -> Result<(PathBuf, cap_std::fs::Dir)> {
    let raw = std::env::var_os("TMPDIR").context("private TMPDIR missing")?;
    let parent = crate::candidate_rust_coverage_run::private_runtime(Path::new(&raw), &args.repo)?;
    let directory = cap_std::fs::Dir::open_ambient_dir(&parent, cap_std::ambient_authority())?;
    Ok((parent, directory))
}
fn probe_hash(args: &Args) -> Result<()> {
    ensure!(
        args.probe_path
            .file_name()
            .is_some_and(|name| name == "conductor_native.so"),
        "invariant probe must be named conductor_native.so"
    );
    let (held, _) = candidate_inputs::external(
        &args.probe_path,
        &args.probe_sha256,
        32 * 1024 * 1024,
        &args.repo,
    )?;
    held.verify()
}
fn stamp(meta: &std::fs::Metadata) -> (u64, u64, u64, u32, u64, i64, i64, i64, i64) {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mode(),
        meta.nlink(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}
fn create(path: &Path, bytes: &[u8]) -> Result<std::fs::Metadata> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file()
            && metadata.nlink() == 1
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o777 == 0o600,
        "unsafe invariant artifact"
    );
    Ok(metadata)
}
fn verify_config(runtime: &Path, dir: &cap_std::fs::Dir, pinned: &std::fs::Metadata) -> Result<()> {
    ensure!(
        read(dir, "invariant.ini")? == CONFIG
            && stamp(&std::fs::symlink_metadata(runtime.join("invariant.ini"))?) == stamp(pinned),
        "invariant coverage configuration changed"
    );
    Ok(())
}
fn torch(args: &Args) -> Result<(String, String)> {
    let mut cmd = Command::new(&args.python);
    cmd.args(["-I", "-c", "import torch,json,os; print(json.dumps([torch.__version__,os.path.realpath(torch.__file__)]))"]);
    let bytes = crate::candidate_rust_coverage_run::output(cmd, "torch version", 4096)?;
    let values: Vec<String> = serde_json::from_slice(&bytes)?;
    ensure!(values.len() == 2, "invalid isolated torch identity");
    let text = &values[0];
    let origin = Path::new(&values[1]);
    ensure!(
        !text.is_empty()
            && text.len() <= 256
            && !text.chars().any(char::is_control)
            && text == &args.torch_pin
            && origin.is_absolute()
            && origin.canonicalize()? == origin
            && !origin.starts_with(&args.repo)
            && !origin.starts_with(&args.source_root)
            && values[1].len() <= 4096
            && !values[1].chars().any(char::is_control),
        "installed torch does not match exact pin"
    );
    Ok((text.clone(), values[1].clone()))
}
fn pytest(args: &Args, runtime: &Path, origin: &str) -> Result<()> {
    let mut cmd = Command::new(&args.python);
    let pythonpath = std::env::join_paths([
        args.probe_path.parent().context("probe parent missing")?,
        &args.repo,
    ])?;
    cmd.current_dir(&args.repo)
        .env("PYTHONPATH", pythonpath)
        .env_remove("PYTEST_ADDOPTS")
        .env("FORGE_INVARIANT_TORCH_VERSION", &args.torch_pin)
        .env("FORGE_INVARIANT_TORCH_ORIGIN", origin)
        .env("FORGE_INVARIANT_ROOT", &args.repo)
        .env("FORGE_INVARIANT_SOURCE_ROOT", &args.source_root)
        .env("FORGE_INVARIANT_PHASE", &args.phase)
        .env("FORGE_INVARIANT_NODEID", &args.nodeid)
        .env(
            "FORGE_INVARIANT_NATIVE_REPORT",
            runtime.join("invariant-native.json"),
        )
        .arg("-P")
        .arg(&args.coverage)
        .args(["run", "--branch"])
        .arg(format!(
            "--rcfile={}",
            runtime.join("invariant.ini").display()
        ))
        .arg(format!(
            "--data-file={}",
            runtime.join("invariant.coverage").display()
        ))
        .arg(&args.pytest)
        .args([
            "-q",
            "-o",
            "addopts=",
            "--rootdir=.",
            "-p",
            "no:cacheprovider",
            "-p",
            "conductor_native",
        ]);
    if args.phase == "baseline" {
        cmd.arg("--collect-only");
    }
    cmd.arg("--").arg(if args.phase == "baseline" {
        node_module(&args.nodeid)?
    } else {
        &args.nodeid
    });
    ensure!(
        cmd.status().context("run invariant pytest")?.success(),
        "invariant pytest failed"
    );
    Ok(())
}
fn export(args: &Args, runtime: &Path) -> Result<()> {
    let mut cmd = Command::new(&args.python);
    cmd.current_dir(&args.repo)
        .arg("-P")
        .arg(&args.coverage)
        .arg("json")
        .arg(format!(
            "--rcfile={}",
            runtime.join("invariant.ini").display()
        ))
        .arg(format!(
            "--data-file={}",
            runtime.join("invariant.coverage").display()
        ))
        .arg("-o")
        .arg(runtime.join("invariant-coverage.json"))
        .arg("--quiet");
    ensure!(
        cmd.status().context("export invariant coverage")?.success(),
        "invariant coverage export failed"
    );
    Ok(())
}
fn read(dir: &cap_std::fs::Dir, name: &str) -> Result<Vec<u8>> {
    conductor_native::candidate_cargo_lint::read_optional(dir, name, LIMIT)
        .map_err(anyhow::Error::msg)?
        .with_context(|| format!("required invariant artifact missing: {name}"))
}
fn measure(args: &Args, runtime: &Path, dir: &cap_std::fs::Dir) -> Result<()> {
    probe_hash(args)?;
    let (version, origin) = torch(args)?;
    let config = create(&runtime.join("invariant.ini"), CONFIG)?;
    pytest(args, runtime, &origin)?;
    verify_config(runtime, dir, &config)?;
    export(args, runtime)?;
    verify_config(runtime, dir, &config)?;
    probe_hash(args)?;
    let coverage = read(dir, "invariant-coverage.json")?;
    let native = read(dir, "invariant-native.json")?;
    ensure!(
        coverage
            .len()
            .checked_add(native.len())
            .is_some_and(|n| n <= LIMIT),
        "invariant artifacts exceed limit"
    );
    let coverage =
        crate::tool_schema::parse_document_with_limit(std::str::from_utf8(&coverage)?, LIMIT)?;
    let native =
        crate::tool_schema::parse_document_with_limit(std::str::from_utf8(&native)?, LIMIT)?;
    let value = serde_json::json!({"schema_version":1,"nodeid":args.nodeid,"phase":args.phase,"torch_version":version,"probe_sha256":args.probe_sha256,"coverage":coverage,"native":native});
    let bytes = serde_json::to_vec(&value)?;
    ensure!(
        bytes.len() <= LIMIT,
        "combined invariant artifact exceeds limit"
    );
    create(&runtime.join("invariant.json"), &bytes).map(|_| ())
}
pub fn run(args: Args) -> Result<u8> {
    let inputs = capture(&args)?;
    let (runtime, dir) = root(&args)?;
    probe_hash(&args)?;
    if !args.execute {
        inputs.verify()?;
        return Ok(0);
    }
    let outcome = measure(&args, &runtime, &dir);
    let verified = inputs.verify();
    ensure!(
        outcome.is_ok() && verified.is_ok(),
        "invariant measurement: {}; input verification: {}",
        outcome
            .err()
            .map(|v| format!("{v:#}"))
            .unwrap_or_else(|| "ok".into()),
        verified
            .err()
            .map(|v| format!("{v:#}"))
            .unwrap_or_else(|| "ok".into())
    );
    Ok(0)
}
