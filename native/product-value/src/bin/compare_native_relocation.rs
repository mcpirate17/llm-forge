//! Alternating pinned native baseline/candidate comparison; separate raw schema.
#[path = "../fixture.rs"]
mod fixture;
#[path = "../measured.rs"]
mod measured;
#[path = "../native_compare_sources.rs"]
mod source_identity;
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
#[derive(Parser)]
struct Args {
    #[arg(long)]
    repo: PathBuf,
    #[arg(long)]
    git: PathBuf,
    #[arg(long)]
    baseline: PathBuf,
    #[arg(long)]
    baseline_sha256: String,
    #[arg(long)]
    current: PathBuf,
    #[arg(long)]
    current_sha256: String,
    #[arg(long)]
    scratch_parent: PathBuf,
    #[arg(long)]
    output: PathBuf,
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
        hash_file(&args.baseline)? == args.baseline_sha256,
        "baseline ELF pin mismatch"
    );
    ensure!(
        hash_file(&args.current)? == args.current_sha256,
        "candidate ELF pin mismatch"
    );
    ensure!(
        args.current_sha256 != args.baseline_sha256,
        "candidate must differ from baseline ELF"
    );
    let mut runtimes = serde_json::Map::new();
    for (label, path) in [("baseline", &args.baseline), ("current", &args.current)] {
        let mut magic = [0u8; 4];
        File::open(path)?.read_exact(&mut magic)?;
        ensure!(&magic == b"\x7fELF", "native ELF required");
        let mut c = command(path, scratch);
        c.arg("--version");
        let observed = measured::run(
            &mut c,
            scratch,
            &format!("version-{label}"),
            Duration::from_secs(15),
        )?
        .require_success()?;
        runtimes.insert(label.into(),json!({"path":path,"sha256":hash_file(path)?,"version":String::from_utf8(observed.stdout)?.trim()}));
    }
    Ok(
        json!({"runtimes":runtimes,"selected_sources":source_identity::snapshot(&args.repo,&args.git,scratch)?,
 "harness_sha256":hash_file(&std::env::current_exe()?)?,
 "source_binding":"ELF pins and full selected source hashes are observations, not proof binary was built from them; retain root build receipts for both ELFs"}),
    )
}
fn candidate(args: &Args, scratch: &Path, fixture: &fixture::Fixture, baseline: bool) -> Command {
    let mut c = command(
        if baseline {
            &args.baseline
        } else {
            &args.current
        },
        scratch,
    );
    c.arg("worktree-runtime")
        .arg(&fixture.source)
        .arg(&fixture.destination);
    c
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

fn plan(count: usize, body: usize) -> Value {
    let mut raw = Vec::new();
    for iteration in 0..21 + 3 {
        for (position, baseline) in if iteration % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        }
        .into_iter()
        .enumerate()
        {
            raw.push(json!({"variant":if baseline {"baseline"}else{"current"},"iteration":iteration,"pair_position":position,
                "warmup":iteration<3,"status":"unstarted","correctness":"unstarted","idempotence":{"status":"unstarted"}}));
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
    let baseline = row["variant"] == "baseline";
    let tag = format!(
        "{count}-{}-{}",
        row["iteration"],
        if baseline { "baseline" } else { "current" }
    );
    let mut cmd = candidate(args, scratch, fixture, baseline);
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
    let mut cmd = candidate(args, scratch, fixture, baseline);
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
    let mut baseline_samples = Vec::new();
    let mut current_samples = Vec::new();
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
                    if row["variant"] == "baseline" {
                        baseline_samples.push(sample);
                    } else {
                        current_samples.push(sample);
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
    let baseline = summary(&baseline_samples)?;
    let current = summary(&current_samples)?;
    case["baseline"] = baseline.clone();
    case["current"] = current.clone();
    case["complete"] = json!(true);
    case["current_over_baseline_median_wall_ratio"] =
        json!(current["p50_ms"].as_f64().unwrap() / baseline["p50_ms"].as_f64().unwrap());
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

fn execute(args: &Args, scratch: &Path, report: &mut Value) -> Result<()> {
    measured::compiler_guard()?;
    report["identity"] = identities(args, scratch)?;
    for (index, (count, body)) in [(16, 128), (256, 4096)].into_iter().enumerate() {
        relocation(args, scratch, count, body, &mut report["relocation"][index])?;
    }
    ensure!(
        hash_file(&args.baseline)? == args.baseline_sha256
            && hash_file(&args.current)? == args.current_sha256,
        "runtime ELF changed"
    );
    ensure!(
        source_identity::snapshot(&args.repo, &args.git, scratch)?
            == report["identity"]["selected_sources"],
        "selected source changed during comparison"
    );
    measured::compiler_guard()?;
    Ok(())
}
fn run() -> Result<()> {
    let mut args = Args::parse();
    measured::install()?;
    measured::budget(Duration::from_secs(300))?;
    args.repo = args.repo.canonicalize()?;
    ensure!(
        args.baseline.is_absolute()
            && args.current.is_absolute()
            && args.git.is_absolute()
            && args.output.is_absolute(),
        "absolute paths required"
    );
    let parent = args.scratch_parent.canonicalize()?;
    let output_parent = args
        .output
        .parent()
        .context("output parent required")?
        .canonicalize()?;
    ensure!(
        !parent.starts_with(&args.repo) && !output_parent.starts_with(&args.repo),
        "scratch/output must be external"
    );
    ensure!(
        !args.output.exists(),
        "output exists; never overwrite original results"
    );
    let scratch = tempfile::Builder::new()
        .prefix("forge-value-")
        .tempdir_in(parent)?;
    let mut report = json!({"schema":"forge-native-relocation-paired-v1","complete":false,"host_before":host(),
 "relocation":[plan(16,128),plan(256,4096)],"methodology":{"warmups_per_variant":3,"samples_per_variant":21,"alternation":"baseline/current on even iterations; current/baseline on odd iterations",
 "fixtures":"console-script-hardlink-fixture-v1; same frozen generation/reset/verification code as results-01.json",
 "timing":"CLI spawn/startup/task; fixture/reset/correctness/replay excluded; no inference",
 "cpu_rss":"Linux wait4 child CPU and peak RSS; KiB converted to bytes",
 "contract":"both native variants retain original fsync/exchange/publication safeguards; source review and separate correctness controls required, benchmark does not prove safeguards",
 "deadline":"300s child/admission deadline, not hard whole-process cap","isolation":"shared unisolated host; compiler guard before attempts"}});
    let result = execute(&args, scratch.path(), &mut report);
    report["host_after"] = host();
    report["accounting"] = accounting(&report);
    if let Err(error) = &result {
        report["error"] = json!(format!("{error:#}"));
    } else {
        report["complete"] = json!(true);
    }
    let bytes = serde_json::to_vec_pretty(&report)?;
    ensure!(bytes.len() <= 2 * 1024 * 1024, "report exceeds2MiB");
    let mut out = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&args.output)?;
    out.write_all(&bytes)?;
    out.sync_all()?;
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
