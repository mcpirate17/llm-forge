//! Four-case syscall investigation. Traced timings never replace paired results.
#[path = "../fixture.rs"]
mod fixture;
#[path = "../measured.rs"]
mod measured;
#[path = "../profile_contract.rs"]
mod profile_contract;
use anyhow::{ensure, Context, Result};
use clap::Parser;
use profile_contract::{BLOB, SOURCE_SHA};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
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
    strace: PathBuf,
    #[arg(long)]
    forge_sha256: String,
    #[arg(long)]
    python_sha256: String,
    #[arg(long)]
    paired_results: PathBuf,
    #[arg(long)]
    paired_sha256: String,
    #[arg(long, requires = "alternate_native_build_sha256")]
    alternate_native_build_receipt: Option<PathBuf>,
    #[arg(long, requires = "alternate_native_build_receipt")]
    alternate_native_build_sha256: Option<String>,
    #[arg(long)]
    forge: PathBuf,
    #[arg(long)]
    python: PathBuf,
    #[arg(long)]
    parent: PathBuf,
    #[arg(long)]
    output: PathBuf,
}
fn command(program: &Path, root: &Path) -> Command {
    let mut c = Command::new(program);
    c.env_clear()
        .env("PATH", "")
        .env("HOME", root)
        .env("TMPDIR", root)
        .env("LANG", "C.UTF-8")
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_CACHE_HOME", root.join("cache"))
        .env("FORGE_LEDGER_DISABLE", "1")
        .current_dir(root);
    c
}
fn hash_file(path: &Path) -> Result<String> {
    Ok(fixture::hash(&read_artifact(path)?))
}
fn read_artifact(path: &Path) -> Result<Vec<u8>> {
    ensure!(
        fs::metadata(path)?.len() <= 64 * 1024 * 1024,
        "profile artifact exceeds64MiB"
    );
    let mut bytes = Vec::new();
    File::open(path)?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 64 * 1024 * 1024,
        "profile artifact exceeds64MiB"
    );
    Ok(bytes)
}
fn category(name: &str) -> &'static str {
    match name {
        "fsync" | "fdatasync" | "syncfs" => "sync",
        "renameat2" | "renameat" | "rename" => "rename_exchange",
        "flock" | "fcntl" => "locks_and_descriptor_control",
        "read" | "pread64" | "readv" | "write" | "pwrite64" | "writev" => "read_write",
        "openat" | "openat2" | "open" | "close" | "stat" | "fstat" | "lstat" | "newfstatat"
        | "statx" | "readlink" | "readlinkat" | "access" | "faccessat" | "getdents64"
        | "unlink" | "unlinkat" => "path_metadata_open_close",
        _ => "other",
    }
}
fn trace(path: &Path) -> Result<Value> {
    ensure!(
        fs::metadata(path)?.len() <= 64 * 1024 * 1024,
        "trace exceeds64MiB"
    );
    let mut calls = BTreeMap::<String, (u64, f64)>::new();
    let mut groups = BTreeMap::<String, (u64, f64)>::new();
    let (mut first, mut last, mut first_fixture) = (None::<f64>, None::<f64>, None::<f64>);
    let mut pids = BTreeSet::new();
    let mut unparsed = 0;
    for line in BufReader::new(File::open(path)?).lines() {
        let line = line?;
        let Some((pid, rest)) = line.trim_start().split_once(char::is_whitespace) else {
            continue;
        };
        let Some((stamp, body)) = rest.trim_start().split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(time) = stamp.parse::<f64>() else {
            continue;
        };
        pids.insert(pid.to_owned());
        first = Some(first.unwrap_or(time));
        last = Some(time);
        let body = body.trim_start();
        let Some((name, _)) = body.split_once('(') else {
            continue;
        };
        if name != "execve"
            && [
                "openat",
                "openat2",
                "newfstatat",
                "statx",
                "readlink",
                "readlinkat",
                "stat",
                "lstat",
            ]
            .contains(&name)
            && (body.contains("source-env") || body.contains("guest-env"))
        {
            first_fixture = Some(first_fixture.unwrap_or(time));
        }
        let Some((_, tail)) = line.rsplit_once(" <") else {
            continue;
        };
        let Some(text) = tail.strip_suffix('>') else {
            continue;
        };
        let Ok(seconds) = text.parse::<f64>() else {
            unparsed += 1;
            continue;
        };
        if name.contains("resumed") {
            unparsed += 1;
            continue;
        }
        let entry = calls.entry(name.to_owned()).or_default();
        entry.0 += 1;
        entry.1 += seconds * 1000.0;
        let entry = groups.entry(category(name).into()).or_default();
        entry.0 += 1;
        entry.1 += seconds * 1000.0;
    }
    ensure!(
        !calls.is_empty() && first_fixture.is_some(),
        "trace parser found no syscalls/fixture boundary"
    );
    let rows = |map: BTreeMap<String, (u64, f64)>| {
        map.into_iter()
            .map(|(name, (count, millis))| {
                (name, json!({"calls":count,"traced_syscall_wall_ms":millis}))
            })
            .collect::<serde_json::Map<_, _>>()
    };
    let first = first.context("trace start missing")?;
    let last = last.context("trace end missing")?;
    let fixture = first_fixture.unwrap();
    Ok(
        json!({"syscalls":rows(calls),"categories":rows(groups),"trace_process_ids":pids,"unparsed_duration_lines":unparsed,
        "trace_span_ms":(last-first)*1000.0,"exec_to_first_fixture_path_operation_ms":(fixture-first)*1000.0,
        "first_fixture_path_to_last_event_ms":(last-fixture)*1000.0,
        "boundary_scope":"operational first fixture path access, not pure startup/algorithm timing; traced syscall sums can overlap across processes"}),
    )
}
fn baseline(args: &Args, root: &Path) -> Result<()> {
    let mut git = command(&args.git, root);
    git.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["--no-pager", "-C"])
        .arg(&args.repo)
        .args(["cat-file", "blob", BLOB]);
    let source = measured::run(&mut git, root, "git-blob", Duration::from_secs(15))?
        .require_success()?
        .stdout;
    ensure!(
        fixture::hash(&source) == SOURCE_SHA,
        "reference source changed"
    );
    fs::write(root.join("reference-only.py"), source)?;
    Ok(())
}
fn task(args: &Args, root: &Path, f: &fixture::Fixture, native: bool) -> Command {
    let mut c = command(if native { &args.forge } else { &args.python }, root);
    if native {
        c.arg("worktree-runtime");
    } else {
        c.args(["-I", "-B"]).arg(root.join("reference-only.py"));
    }
    c.arg(&f.source).arg(&f.destination);
    c
}
fn paired_evidence(args: &Args) -> Result<profile_contract::Paired> {
    let bytes = read_artifact(&args.paired_results)?;
    ensure!(
        fixture::hash(&bytes) == args.paired_sha256,
        "paired data pin mismatch"
    );
    let build = if let Some(path) = &args.alternate_native_build_receipt {
        ensure!(
            path.is_absolute(),
            "alternate build receipt must be absolute"
        );
        let bytes = read_artifact(path)?;
        ensure!(
            Some(fixture::hash(&bytes)).as_ref() == args.alternate_native_build_sha256.as_ref(),
            "alternate native build receipt pin mismatch"
        );
        Some(serde_json::from_slice::<Value>(&bytes)?)
    } else {
        None
    };
    profile_contract::validate(
        &serde_json::from_slice::<Value>(&bytes)?,
        &args.forge_sha256,
        &args.python_sha256,
        build.as_ref(),
    )
}
fn traced_command(
    args: &Args,
    root: &Path,
    f: &fixture::Fixture,
    native: bool,
    trace_path: &Path,
) -> Command {
    let mut c = command(&args.strace, root);
    c.args([
        "-f",
        "-ttt",
        "-T",
        "-s",
        "4096",
        "-e",
        "raw=read,write,pread64,pwrite64,readv,writev",
        "-o",
    ])
    .arg(trace_path)
    .arg("--");
    let plain = task(args, root, f, native);
    c.arg(plain.get_program()).args(plain.get_args());
    unsafe {
        c.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: 64 * 1024 * 1024,
                rlim_max: 64 * 1024 * 1024,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    c
}
fn profile(
    args: &Args,
    root: &Path,
    count: usize,
    body: usize,
    native: bool,
    original: &Path,
    row: &mut Value,
) -> Result<()> {
    measured::compiler_guard()?;
    ensure!(!measured::expired(), "profile budget expired");
    let label = if native { "native" } else { "python" };
    let dir = root.join(format!("fixture-{count}"));
    fs::create_dir(&dir)?;
    let f = fixture::Fixture::new(&dir, count, body)?;
    row["fixture"] = f.provenance();
    ensure!(
        f.source.join("bin").components().count() == original.components().count()
            && f.source.join("bin").as_os_str().len() == original.as_os_str().len(),
        "profile path depth/length differs from original fixture"
    );

    row["fixture_bin_path_components"] = json!(f.source.join("bin").components().count());
    row["fixture_bin_path_bytes"] = json!(f.source.join("bin").as_os_str().len());
    let trace_path = args.parent.join(format!("profile-{label}-{count}.strace"));
    ensure!(
        !trace_path.exists(),
        "trace exists; use a fresh output directory"
    );
    let mut c = traced_command(args, root, &f, native, &trace_path);
    row["command"] = json!(std::iter::once(c.get_program())
        .chain(c.get_args())
        .map(|s| s.to_string_lossy().into_owned())
        .collect::<Vec<_>>());
    row["status"] = json!("started");
    let observed = measured::run(
        &mut c,
        root,
        &format!("profile-{label}-{count}"),
        Duration::from_secs(60),
    )?;
    row["tracer_plus_tracee_observation"] = observed.record();
    row["status"] = json!(profile_contract::failure_status(
        observed.timed_out,
        observed.cancelled
    ));
    ensure!(
        observed.success(),
        "traced workload failed: {}",
        observed.record()
    );
    ensure!(
        observed.stderr.is_empty(),
        "unexpected traced workload stderr"
    );
    f.verify(&observed.stdout, count)?;
    let before = f.destination_inodes()?;
    let mut replay = task(args, root, &f, native);
    let replay = measured::run(
        &mut replay,
        root,
        &format!("replay-{label}-{count}"),
        Duration::from_secs(30),
    )?;
    row["idempotence_observation"] = replay.record();
    row["status"] = json!(profile_contract::failure_status(
        replay.timed_out,
        replay.cancelled
    ));
    let replay = replay.require_success()?;
    ensure!(replay.stderr.is_empty(), "unexpected profile replay stderr");
    f.verify(&replay.stdout, 0)?;
    ensure!(
        f.destination_inodes()? == before,
        "profile replay inode change"
    );
    row["trace"] = trace(&trace_path)?;
    row["trace_path"] = json!(trace_path);
    row["trace_sha256"] = json!(hash_file(&trace_path)?);
    row["correctness_checked"] = json!(true);
    row["status"] = json!("completed");
    fs::remove_dir_all(dir)?;
    Ok(())
}
fn unchanged_inputs(args: &Args, strace_sha: &str, harness_sha: &str) -> Result<()> {
    ensure!(
        hash_file(&std::env::current_exe()?)? == harness_sha,
        "profile harness executable changed"
    );
    ensure!(
        hash_file(&args.forge)? == args.forge_sha256
            && hash_file(&args.python)? == args.python_sha256
            && hash_file(&args.strace)? == strace_sha,
        "profile runtime/tracer changed"
    );
    ensure!(
        hash_file(&args.paired_results)? == args.paired_sha256,
        "paired results changed"
    );
    if let Some(path) = &args.alternate_native_build_receipt {
        ensure!(
            Some(hash_file(path)?).as_ref() == args.alternate_native_build_sha256.as_ref(),
            "alternate build observation changed"
        );
    }
    measured::compiler_guard()
}
fn validate_paths(args: &Args) -> Result<()> {
    ensure!(
        [
            &args.repo,
            &args.git,
            &args.strace,
            &args.forge,
            &args.python,
            &args.parent,
            &args.output,
            &args.paired_results
        ]
        .iter()
        .all(|p| p.is_absolute()),
        "absolute paths required"
    );
    ensure!(
        !args
            .parent
            .canonicalize()?
            .starts_with(args.repo.canonicalize()?),
        "scratch must be external"
    );
    let output_parent = args
        .output
        .parent()
        .context("output parent required")?
        .canonicalize()?;
    ensure!(
        !output_parent.starts_with(args.repo.canonicalize()?),
        "output must be external"
    );
    Ok(())
}
fn run() -> Result<()> {
    let args = Args::parse();
    validate_paths(&args)?;
    measured::install()?;
    measured::budget(Duration::from_secs(300))?;
    measured::compiler_guard()?;
    ensure!(
        hash_file(&args.forge)? == args.forge_sha256
            && hash_file(&args.python)? == args.python_sha256,
        "runtime ELF pin mismatch"
    );
    let paired = paired_evidence(&args)?;
    let strace_sha = hash_file(&args.strace)?;
    let harness_sha = hash_file(&std::env::current_exe()?)?;
    let scratch = tempfile::Builder::new()
        .prefix("forge-value-")
        .tempdir_in(&args.parent)?;
    baseline(&args, scratch.path())?;
    let mut report = json!({"schema":"forge-relocation-syscall-profile-v1","complete":false,"paired_results_sha256":args.paired_sha256,
        "strace_sha256":strace_sha,"native_elf_sha256":args.forge_sha256,"python_elf_sha256":args.python_sha256,"baseline_blob":BLOB,"baseline_source_sha256":SOURCE_SHA,"scope":"four traced cases; perturbing diagnostic only, never replacement timing or exact attribution of untraced differences",
        "harness_sha256":harness_sha,"runtime_binding":paired.binding,
        "fixture_reconstruction":"reference/body/script/skipped bytes and declared script modes bound to paired evidence; original path depth/length matched, device/inode values re-created and verified within each run, not matched across runs; skipped-entry permission bits depend on creation umask",
        "compiler_admission":"sampled /proc compiler-name checks before admission, before each case and after cases; no continuous exclusion guarantee","alternate_native_build_sha256":args.alternate_native_build_sha256,
        "model_weights":"not loaded; paired tokenizer model association0707... is synthetic fixture sentinel, not a weight hash","cases":[
            {"variant":"native","scripts":16,"body":128,"status":"unstarted"},{"variant":"python","scripts":16,"body":128,"status":"unstarted"},
            {"variant":"python","scripts":256,"body":4096,"status":"unstarted"},{"variant":"native","scripts":256,"body":4096,"status":"unstarted"}]});
    let result = (|| -> Result<()> {
        for row in report["cases"].as_array_mut().unwrap() {
            let n = row["scripts"].as_u64().unwrap() as usize;
            let b = row["body"].as_u64().unwrap() as usize;
            let native = row["variant"] == "native";
            let original = &paired.source_bins[if n == 16 { 0 } else { 1 }];
            if let Err(error) = profile(&args, scratch.path(), n, b, native, original, row) {
                if row["status"] != "censored" {
                    row["status"] = json!(profile_contract::failure_status(
                        measured::expired(),
                        measured::signal().is_some()
                    ));
                }
                row["error"] = json!(format!("{error:#}"));
                return Err(error);
            }
        }
        unchanged_inputs(&args, &strace_sha, &harness_sha)
    })();
    report["complete"] = json!(result.is_ok());
    if let Err(error) = &result {
        report["error"] = json!(format!("{error:#}"));
    }
    scratch.close()?;
    report["temporary_reference_and_fixtures_removed"] = json!(true);
    let mut out = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&args.output)?;
    out.write_all(&serde_json::to_vec_pretty(&report)?)?;
    out.sync_all()?;
    println!("{}", args.output.display());
    result
}
fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(2);
    }
}
