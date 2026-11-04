//! Bounded matched-task CLI comparison; no shipping Python dependency.
mod context;
mod fixture;
mod measured;
use anyhow::{ensure, Context, Result};
use clap::Parser;
use conductor_native::performance_receipt::{summarize, Sample};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
const BASELINE_OBJECT: &str = "4cd99821a4c6d9f9ddc4e9c3746f3dd0c4189edb";
const BASELINE_SHA: &str = "9015359e74c9fcd17a7bd898665ec6acbb610e088aa56b9e72defefebdf888cb";
#[derive(Parser)]
struct Args {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    git: PathBuf,
    #[arg(long)]
    forge: PathBuf,
    #[arg(long)]
    forge_sha256: String,
    #[arg(long)]
    python: PathBuf,
    #[arg(long)]
    python_sha256: String,
    #[arg(long)]
    scratch_parent: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long, default_value_t = 21)]
    samples: usize,
    #[arg(long, default_value_t = 3)]
    warmups: usize,
    #[arg(long, default_value = "native/forge/src/worktree_runtime.rs")]
    source: PathBuf,
    #[arg(long, default_value = "relocate_console_scripts")]
    symbol: String,
    #[arg(long)]
    tokenizer_config: Option<PathBuf>,
    #[arg(long)]
    tokenizer_name: Option<String>,
}
fn command(program: &Path, home: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", "")
        .env("HOME", home)
        .env("TMPDIR", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("LANG", "C.UTF-8")
        .env("FORGE_LEDGER_DISABLE", "1")
        .current_dir(home);
    command
}
fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= 1024 * 1024 * 1024,
        "tool must be bounded regular file"
    );
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    let mut total = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        ensure!(total <= 1024 * 1024 * 1024, "tool grew beyond limit");
        hash.update(&buffer[..count]);
    }
    let after = fs::metadata(path)?;
    ensure!(
        (
            before.dev(),
            before.ino(),
            before.len(),
            before.mtime(),
            before.ctime()
        ) == (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.ctime()
        ),
        "tool changed while hashing"
    );
    Ok(format!("{:x}", hash.finalize()))
}
fn git(args: &Args, scratch: &Path, argv: &[&str], tag: &str) -> Result<Vec<u8>> {
    let mut command = command(&args.git, scratch);
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["--no-pager", "-C"])
        .arg(&args.repo)
        .args(argv);
    Ok(
        measured::run(&mut command, scratch, tag, Duration::from_secs(15))?
            .require_success()?
            .stdout,
    )
}
fn host() -> Value {
    let read = |p: &str| fs::read_to_string(p).ok();
    let status = read("/proc/self/status").unwrap_or_default();
    let cgroup = read("/proc/self/cgroup").unwrap_or_default();
    let relative = cgroup
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .unwrap_or("/");
    let path = Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
    json!({"isolation":"shared unisolated host; no affinity/governor/service changes","loadavg":read("/proc/loadavg"),
        "cpu_pressure":read("/proc/pressure/cpu"),"memory_pressure":read("/proc/pressure/memory"),
        "affinity":status.lines().find(|l|l.starts_with("Cpus_allowed_list:")),"cgroup":cgroup,
        "cpu_max":fs::read_to_string(path.join("cpu.max")).ok(),"cpuset_effective":fs::read_to_string(path.join("cpuset.cpus.effective")).ok(),
        "kernel":read("/proc/sys/kernel/osrelease")})
}
fn identities(args: &Args, scratch: &Path) -> Result<Value> {
    ensure!(
        hash_file(&args.forge)? == args.forge_sha256,
        "Forge ELF pin mismatch"
    );
    ensure!(
        hash_file(&args.python)? == args.python_sha256,
        "Python ELF pin mismatch"
    );
    let head = String::from_utf8(git(args, scratch, &["rev-parse", "HEAD"], "head")?)?
        .trim()
        .to_owned();
    let mut native = command(&args.forge, scratch);
    native.arg("--version");
    let version = String::from_utf8(
        measured::run(
            &mut native,
            scratch,
            "forge-version",
            Duration::from_secs(15),
        )?
        .require_success()?
        .stdout,
    )?;
    ensure!(
        version.contains(&head[..7]),
        "Forge version does not identify measurement source HEAD: {version} vs {head}"
    );
    let mut python = command(&args.python, scratch);
    python.args(["-I","-B","-c","import sys,json,platform;print(json.dumps({'version':sys.version,'executable':sys.executable,'prefix':sys.prefix,'platform':platform.platform()}))"]);
    let runtime: Value = serde_json::from_slice(
        &measured::run(
            &mut python,
            scratch,
            "python-version",
            Duration::from_secs(15),
        )?
        .require_success()?
        .stdout,
    )?;
    let source = git(
        args,
        scratch,
        &["cat-file", "blob", BASELINE_OBJECT],
        "baseline",
    )?;
    ensure!(
        source.len() <= 65536 && fixture::hash(&source) == BASELINE_SHA,
        "retired baseline Git blob mismatch"
    );
    fs::write(scratch.join("reference-only.py"), &source)?;
    Ok(
        json!({"repository_head":head,"forge_path":args.forge,"forge_sha256":args.forge_sha256,"forge_version":version.trim(),"native_source_binding":"pinned ELF plus reported Git version; this alone is not proof of build provenance; retain root shipping build/check receipt",
        "python_path_as_selected":args.python,"python_elf_sha256":args.python_sha256,"python_runtime":runtime,
        "reference":{"git_expression":"ef50dcb^:src/conductor/worktree_runtime.py","git_blob":BASELINE_OBJECT,"source_sha256":BASELINE_SHA,
            "lifetime":"owned temporary file only; removed after run"},"harness_binary_sha256":hash_file(&std::env::current_exe()?)?}),
    )
}
fn candidate(args: &Args, scratch: &Path, fixture: &fixture::Fixture, native: bool) -> Command {
    let mut command = command(if native { &args.forge } else { &args.python }, scratch);
    if native {
        command.arg("worktree-runtime");
    } else {
        command
            .args(["-I", "-B"])
            .arg(scratch.join("reference-only.py"));
    }
    command.arg(&fixture.source).arg(&fixture.destination);
    command
}
fn p95(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() as f64 * 0.95).ceil() as usize).saturating_sub(1)]
}
fn summary(samples: &[Sample]) -> Result<Value> {
    let mut result = serde_json::to_value(summarize(samples).map_err(anyhow::Error::msg)?)?;
    result["cpu_p95_ms"] = json!(p95(samples.iter().map(|s| s.cpu_ms).collect()));
    result["rss_p95_bytes"] = json!(p95(samples
        .iter()
        .map(|s| s.max_rss_bytes as f64)
        .collect()));
    Ok(result)
}
fn plan(args: &Args, count: usize, body: usize) -> Value {
    let mut raw = Vec::new();
    for iteration in 0..args.samples + args.warmups {
        for (position, native) in if iteration % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        }
        .into_iter()
        .enumerate()
        {
            raw.push(json!({"variant":if native {"native"}else{"python"},"iteration":iteration,"pair_position":position,
                "warmup":iteration<args.warmups,"status":"unstarted","correctness":"unstarted","idempotence":{"status":"unstarted"}}));
        }
    }
    json!({"scripts":count,"body_bytes_per_script":body,"raw":raw,"complete":false,
        "timing_scope":"CLI spawn/startup plus relocation; reset, validation and replay excluded",
        "filesystem":"warm filesystem; fresh hardlinked destination; alternating pair order"})
}
fn attempt(
    args: &Args,
    scratch: &Path,
    fixture: &fixture::Fixture,
    count: usize,
    row: &mut Value,
) -> Result<Sample> {
    row["status"] = json!("started");
    row["phase"] = json!("fixture_reset");
    fixture.reset()?;
    let native = row["variant"] == "native";
    let tag = format!(
        "{count}-{}-{}",
        row["iteration"],
        if native { "native" } else { "python" }
    );
    let mut cmd = candidate(args, scratch, fixture, native);
    row["phase"] = json!("measurement");
    let observed = measured::run(&mut cmd, scratch, &tag, Duration::from_secs(30))?;
    row["observation"] = observed.record();
    if observed.timed_out || observed.cancelled {
        row["status"] = json!("censored");
    }
    ensure!(
        observed.success(),
        "measured child failed/censored: {}",
        row["observation"]
    );
    row["phase"] = json!("correctness");
    row["correctness"] = json!("started");
    ensure!(observed.stderr.is_empty(), "unexpected workload stderr");
    fixture.verify(&observed.stdout, count)?;
    row["correctness"] = json!("completed");
    let inodes = fixture.destination_inodes()?;
    row["phase"] = json!("idempotence");
    row["idempotence"]["status"] = json!("started");
    let mut cmd = candidate(args, scratch, fixture, native);
    let replay = measured::run(
        &mut cmd,
        scratch,
        &format!("{tag}-idempotent"),
        Duration::from_secs(30),
    )?;
    row["idempotence"]["observation"] = replay.record();
    if replay.timed_out || replay.cancelled {
        row["idempotence"]["status"] = json!("censored");
    }
    ensure!(replay.success(), "idempotence child failed/censored");
    ensure!(replay.stderr.is_empty(), "unexpected replay stderr");
    fixture.verify(&replay.stdout, 0)?;
    ensure!(
        fixture.destination_inodes()? == inodes,
        "idempotence changed destination inodes"
    );
    row["idempotence"]["status"] = json!("completed");
    row["status"] = json!("completed");
    Ok(observed.sample)
}
fn relocation(
    args: &Args,
    scratch: &Path,
    count: usize,
    body: usize,
    case: &mut Value,
) -> Result<()> {
    let root = scratch.join(format!("fixture-{count}"));
    fs::create_dir(&root)?;
    let fixture = fixture::Fixture::new(&root, count, body)?;
    case["fixture"] = fixture.provenance();
    let mut native_samples = Vec::new();
    let mut python_samples = Vec::new();
    for row in case["raw"].as_array_mut().unwrap() {
        measured::compiler_guard()?;
        ensure!(
            !measured::expired() && measured::signal().is_none(),
            "global deadline/cancellation before attempt"
        );
        let result = attempt(args, scratch, &fixture, count, row);
        match result {
            Ok(sample) => {
                if row["warmup"] == false {
                    if row["variant"] == "native" {
                        native_samples.push(sample);
                    } else {
                        python_samples.push(sample);
                    }
                }
            }
            Err(error) => {
                if row["status"] != "censored" {
                    row["status"] = json!(if measured::expired() || measured::signal().is_some() {
                        "censored"
                    } else {
                        "failed"
                    });
                }
                if row["correctness"] == "started" {
                    row["correctness"] = json!("failed");
                }
                if row["idempotence"]["status"] == "started" {
                    row["idempotence"]["status"] = row["status"].clone();
                }
                row["error"] = json!(format!("{error:#}"));
                return Err(error);
            }
        }
    }
    let native = summary(&native_samples)?;
    let python = summary(&python_samples)?;
    case["native"] = native.clone();
    case["python"] = python.clone();
    case["complete"] = json!(true);
    case["python_over_native_median_wall_ratio"] =
        json!(python["p50_ms"].as_f64().unwrap() / native["p50_ms"].as_f64().unwrap());
    Ok(())
}
fn execute(args: &Args, scratch: &Path, report: &mut Value) -> Result<()> {
    measured::compiler_guard()?;
    report["identity"] = identities(args, scratch)?;
    for (index, (count, body)) in [(16, 128), (256, 4096)].into_iter().enumerate() {
        relocation(args, scratch, count, body, &mut report["relocation"][index])?;
    }
    measured::compiler_guard()?;
    context::workflow(args, scratch, &mut report["context"])?;
    ensure!(
        hash_file(&args.forge)? == args.forge_sha256
            && hash_file(&args.python)? == args.python_sha256,
        "runtime ELF changed"
    );
    measured::compiler_guard()?;
    Ok(())
}
fn accounting(report: &Value) -> Value {
    let rows: Vec<_> = report["relocation"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|c| c["raw"].as_array().unwrap())
        .collect();
    let counts = |replay: bool| {
        let mut result = json!({"planned":rows.len(),"started":0,"completed":0,"failed":0,"censored":0,"unstarted":0});
        for row in &rows {
            let status = if replay {
                &row["idempotence"]["status"]
            } else {
                &row["status"]
            };
            let label = status.as_str().unwrap();
            result[label] = json!(result[label].as_u64().unwrap() + 1);
            if label != "unstarted" {
                result["started"] = json!(result["started"].as_u64().unwrap() + 1);
            }
        }
        result
    };
    json!({"measurement_attempts_including_warmups":counts(false),"separate_untimed_idempotence_attempts":counts(true)})
}
fn run() -> Result<()> {
    let mut args = Args::parse();
    measured::install()?;
    measured::budget(Duration::from_secs(300))?;
    args.repo = args.repo.canonicalize()?;
    ensure!(
        (5..=101).contains(&args.samples) && args.warmups <= 10,
        "samples5..101/warmups<=10 required"
    );
    ensure!(
        args.forge.is_absolute()
            && args.python.is_absolute()
            && args.git.is_absolute()
            && args.output.is_absolute(),
        "absolute tool/output paths required"
    );
    let parent = args.scratch_parent.canonicalize()?;
    let output_parent = args
        .output
        .parent()
        .context("output parent required")?
        .canonicalize()?;
    ensure!(
        !parent.starts_with(&args.repo) && !output_parent.starts_with(&args.repo),
        "scratch/output must remain outside checkout"
    );
    let scratch = tempfile::Builder::new()
        .prefix("forge-value-")
        .tempdir_in(parent)?;
    let mut report = json!({"schema":"forge-product-value-v1","complete":false,"host_before":host(),"relocation":[plan(&args,16,128),plan(&args,256,4096)],"context":{"status":"unstarted","calls":{"projection":{"status":"unstarted"},"raw_tokens":{"status":if args.tokenizer_config.is_some(){"unstarted"}else{"not_requested"}},"projection_tokens":{"status":if args.tokenizer_config.is_some(){"unstarted"}else{"not_requested"}}}},
        "methodology":{"warmups":args.warmups,"samples_per_variant":args.samples,"clock":"monotonic Instant","cpu_rss":"Linux wait4 per child; RSS KiB converted to bytes",
            "semantic_differences":"native additionally pins/locks dirs, bounds reads, exchanges and validates publication; matched accepted task, not identical safety semantics or isolated language speed",
            "python_scope":"direct isolated -I -B exact retired script; UV/package wrapper overhead excluded",
            "startup":"included in CLI measurements; not subtracted or separately identified","in_process":"reuse existing native/forge/benches/context.rs, not measured by this runner",
            "model_time":"no model inference; token reduction does not establish answer quality or completed-task time"}});
    let result = execute(&args, scratch.path(), &mut report);
    report["host_after"] = host();
    report["accounting"] = accounting(&report);
    if let Err(error) = &result {
        report["error"] = json!(format!("{error:#}"));
    } else {
        report["complete"] = json!(true);
    }
    let bytes = serde_json::to_vec_pretty(&report)?;
    ensure!(bytes.len() <= 2 * 1024 * 1024, "result exceeded2MiB");
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&args.output)?;
    output.write_all(&bytes)?;
    output.sync_all()?;
    scratch.close()?;
    println!("{}", args.output.display());
    result
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(measured::signal().map_or(2, |s| 128 + s));
    }
}
