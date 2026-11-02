//! Supervised Rust source coverage helper. All subprocesses remain in the
//! enclosing candidate Stage cgroup; no independent process supervisor exists.
use crate::candidate_cargo_lint_cli::runtime::Runtime;
use anyhow::{bail, ensure, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
const MAX_STDOUT: u64 = 16 * 1024 * 1024;
const MAX_OBJECTS: usize = 512;
const MAX_PROFILES: usize = 4096;
pub(crate) fn private_runtime(tmp: &Path, snapshot: &Path) -> Result<PathBuf> {
    ensure!(
        tmp.is_absolute()
            && tmp.file_name().is_some_and(|v| v == "tmp")
            && tmp.canonicalize()? == tmp,
        "private TMPDIR must be canonical"
    );
    let parent = tmp.parent().context("private runtime missing")?;
    for path in [tmp, parent] {
        let meta = fs::symlink_metadata(path)?;
        ensure!(
            meta.is_dir()
                && meta.uid() == unsafe { libc::geteuid() }
                && meta.mode() & 0o777 == 0o700,
            "private runtime must be owned mode 0700"
        );
    }
    let snapshot = snapshot.canonicalize()?;
    ensure!(
        !parent.starts_with(&snapshot) && !snapshot.starts_with(parent),
        "private runtime overlaps candidate"
    );
    Ok(parent.to_path_buf())
}

/// Output is bounded and processes are owned by the outer cgroup/deadline.
pub(crate) fn output(mut command: Command, label: &str, maximum: u64) -> Result<Vec<u8>> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("launch {label}"))?;
    let stdout = child.stdout.take().context("child stdout missing")?;
    let mut bytes = Vec::new();
    let read = stdout.take(maximum + 1).read_to_end(&mut bytes);
    if read.is_err() || bytes.len() as u64 > maximum {
        let killed = child.kill();
        let waited = child.wait();
        bail!("{label} output exceeds bound or cannot be read; kill: {killed:?}; wait: {waited:?}");
    }
    let status = child.wait().with_context(|| format!("wait {label}"))?;
    crate::child_exit_status::require_success(status, label).with_context(|| {
        format!(
            "{label} failed: {status}; captured stdout tail: {}",
            String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(8192)..])
        )
    })?;
    Ok(bytes)
}
fn isolated(path: &Path) -> Command {
    let mut command = Command::new(path);
    command.env_clear().env("PATH", "");
    command
}
fn llvm_version(bytes: &[u8]) -> Result<(u32, u32, u32)> {
    let text = std::str::from_utf8(bytes).context("tool version is not UTF-8")?;
    let raw = text
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("LLVM version:")
                .or_else(|| line.trim().strip_prefix("LLVM version "))
        })
        .context("LLVM version missing")?
        .trim();
    let mut parts = raw.split('.');
    let major = parts.next().context("LLVM major missing")?.parse()?;
    let minor = parts.next().context("LLVM minor missing")?.parse()?;
    let patch = parts
        .next()
        .context("LLVM patch missing")?
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .context("LLVM patch missing")?
        .parse()?;
    Ok((major, minor, patch))
}
pub(crate) fn tool_versions(rustc: &Path, profdata: &Path, cov: &Path) -> Result<String> {
    let mut rust = isolated(rustc);
    rust.arg("-vV");
    let rust = output(rust, "rustc version", 65536)?;
    let mut profile = isolated(profdata);
    profile.arg("--version");
    let profile = output(profile, "llvm-profdata version", 65536)?;
    let mut coverage = isolated(cov);
    coverage.arg("--version");
    let coverage = output(coverage, "llvm-cov version", 65536)?;
    let expected = llvm_version(&rust)?;
    ensure!(
        llvm_version(&profile)? == expected && llvm_version(&coverage)? == expected,
        "Rust and LLVM tool versions differ"
    );
    Ok(format!(
        "rustc LLVM {}.{}.{}; llvm-profdata and llvm-cov matched",
        expected.0, expected.1, expected.2
    ))
}
pub(crate) fn target(command: &Command) -> Result<PathBuf> {
    let value = command
        .get_envs()
        .find_map(|(key, value)| {
            (key == OsStr::new("CARGO_TARGET_DIR"))
                .then_some(value)
                .flatten()
        })
        .context("private Cargo target missing")?;
    let path = PathBuf::from(value);
    ensure!(path.is_absolute(), "private Cargo target must be absolute");
    Ok(path)
}
fn instrument(command: &mut Command, profiles: &Path) {
    command.env("CARGO_ENCODED_RUSTFLAGS", "-Cinstrument-coverage");
    command.env("LLVM_PROFILE_FILE", profiles.join("%m-%p.profraw"));
}
pub(crate) fn executable(path: &str, target: &Path) -> Result<PathBuf> {
    let path = PathBuf::from(path);
    ensure!(path.is_absolute(), "Cargo executable must be absolute");
    let actual = path
        .canonicalize()
        .context("Cargo test executable missing")?;
    let target = target
        .canonicalize()
        .context("private Cargo target missing")?;
    ensure!(
        actual.starts_with(target) && fs::metadata(&actual)?.is_file(),
        "Cargo executable escaped private target"
    );
    Ok(actual)
}
#[derive(PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    links: u64,
}
fn stamp(meta: &fs::Metadata) -> Stamp {
    Stamp {
        dev: meta.dev(),
        ino: meta.ino(),
        len: meta.len(),
        modified: (meta.mtime(), meta.mtime_nsec()),
        changed: (meta.ctime(), meta.ctime_nsec()),
        links: meta.nlink(),
    }
}
fn hash_binary(path: &Path) -> Result<(Stamp, [u8; 32])> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= 256 * 1024 * 1024,
        "unsafe Cargo test binary"
    );
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .context("binary size overflow")?;
        ensure!(bytes <= before.len(), "Cargo test binary grew");
        hash.update(&buffer[..count]);
    }
    ensure!(
        bytes == before.len()
            && stamp(&before) == stamp(&file.metadata()?)
            && stamp(&before) == stamp(&fs::symlink_metadata(path)?),
        "Cargo test binary changed"
    );
    Ok((stamp(&before), hash.finalize().into()))
}
fn bind_binaries(paths: &[PathBuf]) -> Result<Vec<(Stamp, [u8; 32])>> {
    let mut bytes = 0u64;
    let mut rows = Vec::new();
    for path in paths {
        let row = hash_binary(path)?;
        bytes = bytes
            .checked_add(row.0.len)
            .context("binary total overflow")?;
        ensure!(
            bytes <= 512 * 1024 * 1024,
            "Cargo test binary bytes exceed limit"
        );
        rows.push(row);
    }
    Ok(rows)
}
fn verify_binaries(paths: &[PathBuf], bound: &[(Stamp, [u8; 32])]) -> Result<()> {
    ensure!(paths.len() == bound.len(), "Cargo binary inventory changed");
    for (path, expected) in paths.iter().zip(bound) {
        ensure!(
            &hash_binary(path)? == expected,
            "Cargo test binary binding changed"
        );
    }
    Ok(())
}
fn objects(bytes: &[u8], target: &Path) -> Result<Vec<PathBuf>> {
    let text = std::str::from_utf8(bytes).context("Cargo JSON is not UTF-8")?;
    let mut objects = BTreeSet::new();
    for line in text.lines() {
        ensure!(line.len() <= 1024 * 1024, "Cargo JSON line exceeds bound");
        let row = crate::tool_schema::parse_document(line)?;
        if row["reason"] != "compiler-artifact" || row["profile"]["test"] != true {
            continue;
        }
        if let Some(path) = row["executable"].as_str() {
            objects.insert(executable(path, target)?);
        }
        ensure!(
            objects.len() <= MAX_OBJECTS,
            "too many Cargo test executables"
        );
    }
    ensure!(!objects.is_empty(), "Cargo produced no test executables");
    Ok(objects.into_iter().collect())
}
fn profiles(dir: &Path, required: bool) -> Result<Vec<PathBuf>> {
    let directory = fs::symlink_metadata(dir)?;
    ensure!(
        directory.is_dir() && directory.uid() == unsafe { libc::geteuid() },
        "unsafe LLVM profile directory"
    );
    let mut rows = Vec::new();
    let mut bytes = 0u64;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let metadata = entry.file_type()?;
        ensure!(metadata.is_file(), "unsafe LLVM profile entry");
        let meta = fs::symlink_metadata(entry.path())?;
        ensure!(
            meta.is_file() && meta.nlink() == 1 && meta.len() <= 256 * 1024 * 1024,
            "unsafe LLVM profile"
        );
        bytes = bytes
            .checked_add(meta.len())
            .context("LLVM profile size overflow")?;
        ensure!(
            bytes <= 256 * 1024 * 1024,
            "LLVM profiles exceed aggregate limit"
        );
        ensure!(
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(".profraw")),
            "unexpected LLVM profile name"
        );
        rows.push(entry.path());
        ensure!(rows.len() <= MAX_PROFILES, "too many LLVM profiles");
    }
    ensure!(
        !required || !rows.is_empty(),
        "instrumented Cargo tests produced no LLVM profiles"
    );
    rows.sort();
    let current = fs::symlink_metadata(dir)?;
    ensure!(
        current.is_dir() && current.dev() == directory.dev() && current.ino() == directory.ino(),
        "LLVM profile directory changed"
    );
    Ok(rows)
}
fn signature(path: &Path) -> Result<&str> {
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .context("LLVM profile name not UTF-8")?;
    let stem = name
        .strip_suffix(".profraw")
        .context("invalid LLVM profile suffix")?;
    let (signature, pid) = stem
        .rsplit_once('-')
        .context("LLVM profile signature missing")?;
    ensure!(
        !signature.is_empty()
            && signature.len() <= 128
            && signature.bytes().all(|b| b.is_ascii_digit() || b == b'_')
            && !pid.is_empty()
            && pid.bytes().all(|b| b.is_ascii_digit()),
        "invalid LLVM profile signature or PID"
    );
    Ok(signature)
}
fn reject_build_profiles(inventory: &[PathBuf], measured: &[PathBuf]) -> Result<()> {
    let mut signatures = BTreeSet::new();
    for path in inventory {
        signatures.insert(signature(path)?);
    }
    for path in measured {
        ensure!(
            !signatures.contains(signature(path)?),
            "instrumented build-time code ran during measured tests"
        );
    }
    Ok(())
}
fn save(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
fn reject_rebuilds(bytes: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(bytes).context("Cargo test output is not UTF-8")?;
    for line in text.lines() {
        if !line.starts_with('{') {
            continue;
        }
        ensure!(
            line.len() <= 1024 * 1024,
            "Cargo test JSON line exceeds bound"
        );
        let row = match crate::tool_schema::parse_document(line) {
            Ok(row) => row,
            Err(error) if line.starts_with("{\"reason\"") => {
                return Err(error).context("malformed Cargo event");
            }
            Err(_) => continue,
        };
        match row["reason"].as_str() {
            // Cargo replays this event when a cached build script did not run.
            // Actual instrumented build execution is checked from profile signatures.
            Some("build-script-executed") => (),
            Some("compiler-artifact") => ensure!(
                row["fresh"] == true,
                "Cargo rebuilt an artifact during measured tests"
            ),
            _ => (),
        }
    }
    Ok(())
}
/// Invoked only by the native Cargo helper inside an owned Stage.
pub(crate) fn run(
    runtime: &Runtime,
    manifest: &Path,
    runtime_dir: &Path,
    profdata: &Path,
    cov: &Path,
) -> Result<()> {
    let dir = runtime_dir.join("rust-profiles");
    fs::create_dir(&dir)?;
    let inventory_dir = runtime_dir.join("rust-inventory-profiles");
    fs::create_dir(&inventory_dir)?;
    let mut inventory = runtime.command(Some(manifest))?;
    let target = target(&inventory)?;
    instrument(&mut inventory, &inventory_dir);
    inventory
        .args(["test", "--offline", "--locked", "--manifest-path"])
        .arg(manifest)
        .args(["--all-targets", "--no-run", "--message-format=json"]);
    let binaries = objects(
        &output(inventory, "Cargo test inventory", 8 * 1024 * 1024)?,
        &target,
    )?;
    let inventory_profiles = profiles(&inventory_dir, false)?;
    // Validate names before starting tests; do not rely on the directory later.
    reject_build_profiles(&inventory_profiles, &[])?;
    let bound = bind_binaries(&binaries)?;
    let mut test = runtime.command(Some(manifest))?;
    instrument(&mut test, &dir);
    test.args(["test", "--offline", "--locked", "--manifest-path"])
        .arg(manifest)
        .args(["--all-targets", "--message-format=json"]);
    let test_output = output(test, "instrumented Cargo test", MAX_STDOUT)?;
    std::io::stdout().write_all(&test_output)?;
    reject_rebuilds(&test_output)?;
    verify_binaries(&binaries, &bound)?;
    let profiles = profiles(&dir, true)?;
    reject_build_profiles(&inventory_profiles, &profiles)?;
    let bound_profiles = bind_binaries(&profiles)?;
    let merged = runtime_dir.join("rust.profdata");
    ensure!(
        matches!(fs::symlink_metadata(&merged),Err(ref error) if error.kind()==std::io::ErrorKind::NotFound),
        "LLVM merged profile path already exists or cannot be inspected"
    );
    let mut merge = isolated(profdata);
    merge
        .args(["merge", "-sparse", "-o"])
        .arg(&merged)
        .args(&profiles);
    ensure!(
        merge.status().context("launch llvm-profdata")?.success(),
        "llvm-profdata failed"
    );
    verify_binaries(&profiles, &bound_profiles)?;
    let merged_metadata = fs::symlink_metadata(&merged)?;
    ensure!(
        merged_metadata.is_file()
            && merged_metadata.nlink() == 1
            && merged_metadata.len() <= 256 * 1024 * 1024,
        "unsafe merged LLVM profile"
    );
    let mut export = isolated(cov);
    let mut profile_arg = std::ffi::OsString::from("--instr-profile=");
    profile_arg.push(&merged);
    export
        .args(["export", "--format=lcov", "--check-binary-ids"])
        .arg(profile_arg)
        .arg(&binaries[0]);
    for binary in &binaries[1..] {
        let mut object = std::ffi::OsString::from("--object=");
        object.push(binary);
        export.arg(object);
    }
    let bytes = output(export, "llvm-cov export", 4 * 1024 * 1024)?;
    verify_binaries(&binaries, &bound)?;
    save(&runtime_dir.join("rust-coverage.lcov"), &bytes)
}
#[cfg(test)]
#[path = "candidate_rust_coverage_run_tests.rs"]
mod tests;
