//! Candidate-built Python extensions and Forge for selected legacy contracts.
//! Invoke only inside an existing supervised candidate Stage; tools and Python
//! sites are explicit operator grants. No global installation is performed.
use crate::{
    candidate_cargo_lint_cli::runtime::Runtime,
    candidate_rust_coverage_run::{executable, output, target},
};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};
const MAX_TARGETS: usize = 1024;
const MAX_ARTIFACT: u64 = 256 * 1024 * 1024;
const NATIVE: &str = "native/conductor-native/Cargo.toml";
const SLOP: &str = "native/slop-core/Cargo.toml";
const FORGE: &str = "native/forge/Cargo.toml";
#[derive(Serialize)]
pub(crate) struct Evidence {
    targets: Vec<String>,
    python: String,
    artifacts: Vec<Artifact>,
    scope: &'static str,
    slop_candidate_built: bool,
    slop_scope: &'static str,
}
#[derive(Serialize)]
struct Artifact {
    name: String,
    sha256: String,
    bytes: u64,
}
pub(crate) fn targets(values: &[String]) -> Result<Vec<String>> {
    ensure!(
        !values.is_empty() && values.len() <= MAX_TARGETS,
        "invalid selected contract target count"
    );
    let mut seen = BTreeSet::new();
    for value in values {
        ensure!(
            value.len() <= 256
                && value.starts_with("python_contracts_")
                && value
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "invalid contract target"
        );
        ensure!(seen.insert(value.clone()), "duplicate contract target");
    }
    Ok(seen.into_iter().collect())
}
pub(crate) fn slop_needed(
    snapshot: &Path,
    runtime: &Path,
    python: &Path,
    targets: &[String],
) -> Result<bool> {
    let root = cap_std::fs::Dir::open_ambient_dir(snapshot, cap_std::ambient_authority())?;
    let slop = conductor_native::candidate_cargo_lint::read_optional(&root, SLOP, 1024 * 1024)
        .map_err(anyhow::Error::msg)?;
    let request = json!({"snapshot":snapshot,"runtime_dir":runtime,"python_executable":python,"targets":targets});
    let plan = conductor_native::candidate_verification::decide("contract_runtime_plan", &request)
        .map_err(anyhow::Error::msg)?;
    let commands = plan["build_commands"]
        .as_array()
        .context("native contract build plan missing")?;
    ensure!(
        (2..=3).contains(&commands.len()),
        "invalid native contract build plan"
    );
    let declared = commands.iter().any(|row| {
        row["argv"]
            .as_array()
            .is_some_and(|args| args.iter().any(|arg| arg == "extension-module"))
    });
    match slop {
        Some(_) => Ok(true),
        None => {
            ensure!(
                !declared,
                "selected contract requires candidate slop source"
            );
            Ok(false)
        }
    }
}
fn artifact_from_json(
    bytes: &[u8],
    target_dir: &Path,
    name: &str,
    binary: bool,
) -> Result<PathBuf> {
    let mut hits = BTreeSet::new();
    let text = std::str::from_utf8(bytes).context("Cargo artifact stream not UTF-8")?;
    for line in text.lines() {
        ensure!(
            line.len() <= 1024 * 1024,
            "Cargo artifact JSON line too large"
        );
        let row = crate::tool_schema::parse_document(line)?;
        if row["reason"] != "compiler-artifact" || row["target"]["name"] != name {
            continue;
        }
        let choices: Vec<&str> = if binary {
            row["executable"].as_str().into_iter().collect()
        } else {
            row["filenames"]
                .as_array()
                .context("Cargo artifact filenames missing")?
                .iter()
                .filter_map(|v| v.as_str())
                .filter(|v| v.ends_with(".so"))
                .collect()
        };
        for path in choices {
            hits.insert(executable(path, target_dir)?);
        }
    }
    ensure!(
        hits.len() == 1,
        "candidate Cargo artifact missing or ambiguous"
    );
    Ok(hits.into_iter().next().expect("one artifact"))
}
fn build(
    runtime: &Runtime,
    snapshot: &Path,
    manifest: &str,
    arguments: &[&str],
    name: &str,
    binary: bool,
    python: &Path,
) -> Result<PathBuf> {
    let manifest = snapshot.join(manifest);
    let mut command = runtime.command(Some(&manifest))?;
    let directory = target(&command)?;
    pin_python(&mut command, python)?;
    command
        .args(["build", "--offline", "--locked", "--manifest-path"])
        .arg(&manifest)
        .args(arguments)
        .arg("--message-format=json");
    let bytes = output(command, "candidate contract build", 8 * 1024 * 1024)?;
    artifact_from_json(&bytes, &directory, name, binary)
}
fn copy_artifact(source: &Path, dest: &Path, name: &str) -> Result<Artifact> {
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(source)?;
    let before = input.metadata()?;
    ensure!(
        before.is_file() && before.len() <= MAX_ARTIFACT,
        "unsafe contract build artifact"
    );
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(dest)?;
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .context("contract artifact overflow")?;
        ensure!(bytes <= before.len(), "contract artifact grew");
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    output.sync_all()?;
    let after = input.metadata()?;
    let current = fs::symlink_metadata(source)?;
    ensure!(
        bytes == before.len()
            && before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == after.len()
            && before.mode() == after.mode()
            && before.nlink() == after.nlink()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec()
            && current.is_file()
            && after.dev() == current.dev()
            && after.ino() == current.ino()
            && after.len() == current.len()
            && after.mtime() == current.mtime()
            && after.mtime_nsec() == current.mtime_nsec()
            && after.ctime() == current.ctime()
            && after.ctime_nsec() == current.ctime_nsec(),
        "contract build artifact changed"
    );
    Ok(Artifact {
        name: name.into(),
        sha256: format!("{:x}", hash.finalize()),
        bytes,
    })
}
fn environment(
    command: &mut Command,
    python: &Path,
    snapshot: &Path,
    extensions: &Path,
    forge: &Path,
) -> Result<()> {
    let paths = std::env::join_paths([
        extensions.to_path_buf(),
        snapshot.join("src"),
        snapshot.to_path_buf(),
    ])?;
    pin_python(command, python)?;
    command
        .env("PYTHONPATH", paths)
        .env("PYTHONNOUSERSITE", "1")
        .env("FORGE_BIN", forge)
        .env("FORGE_TEST_BIN", forge)
        .env("CUDA_VISIBLE_DEVICES", "")
        .env("ROCR_VISIBLE_DEVICES", "")
        .env("OMP_NUM_THREADS", "1")
        .env("OPENBLAS_NUM_THREADS", "1")
        .env("RAYON_NUM_THREADS", "1");
    Ok(())
}
pub(crate) fn pin_python(command: &mut Command, python: &Path) -> Result<()> {
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("PYO3_") {
            command.env_remove(key);
        }
    }
    command.env("PYO3_PYTHON", python);
    if let Some(raw) = std::env::var_os("FORGE_CONTRACT_PYTHON_LIB_DIR") {
        let path = Path::new(&raw);
        ensure!(
            path.is_absolute()
                && path.as_os_str().as_encoded_bytes().len() <= 4096
                && path.canonicalize()? == path
                && path.is_dir(),
            "invalid explicitly granted Python library directory"
        );
        command.env("LD_LIBRARY_PATH", path);
    }
    Ok(())
}
/// Builds and runs exact selected targets; the Stage owns cancellation/cleanup.
pub(crate) fn run(
    runtime: &Runtime,
    snapshot: &Path,
    runtime_dir: &Path,
    python: &Path,
    selected: &[String],
) -> Result<Evidence> {
    ensure!(
        snapshot.is_absolute() && runtime_dir.is_absolute() && python.is_absolute(),
        "contract roots and Python must be absolute"
    );
    let targets = targets(selected)?;
    let slop = slop_needed(snapshot, runtime_dir, python, &targets)?;
    let extensions = runtime_dir.join("contract-extension");
    fs::create_dir(&extensions)?;
    fs::set_permissions(&extensions, fs::Permissions::from_mode(0o700))?;
    let native = build(
        runtime,
        snapshot,
        NATIVE,
        &["--features", "python", "--lib"],
        "conductor_native",
        false,
        python,
    )?;
    let mut artifacts = vec![copy_artifact(
        &native,
        &extensions.join("conductor_native.so"),
        "conductor_native",
    )?];
    if slop {
        let artifact = build(
            runtime,
            snapshot,
            SLOP,
            &["--features", "extension-module", "--lib"],
            "slop_core",
            false,
            python,
        )?;
        artifacts.push(copy_artifact(
            &artifact,
            &extensions.join("slop_core.so"),
            "slop_core",
        )?);
    }
    let forge = build(
        runtime,
        snapshot,
        FORGE,
        &["--bin", "forge"],
        "forge",
        true,
        python,
    )?;
    let candidate_forge = runtime_dir.join("contract-forge");
    artifacts.push(copy_artifact(&forge, &candidate_forge, "forge")?);
    fs::set_permissions(&candidate_forge, fs::Permissions::from_mode(0o700))?;
    let manifest = snapshot.join(NATIVE);
    let mut command = runtime.command(Some(&manifest))?;
    environment(
        &mut command,
        python,
        snapshot,
        &extensions,
        &candidate_forge,
    )?;
    command
        .args(["test", "--offline", "--locked", "--manifest-path"])
        .arg(manifest)
        .args(["--features", "python-compat-tests"]);
    for name in &targets {
        command.arg("--test").arg(name);
    }
    command.args(["--", "--test-threads=1"]);
    ensure!(
        command
            .status()
            .context("launch candidate contracts")?
            .success(),
        "candidate contract tests failed"
    );
    Ok(Evidence {
        targets,
        python: python.to_string_lossy().into(),
        artifacts,
        scope: "candidate_built_contract_runtime",
        slop_candidate_built: slop,
        slop_scope: "legacy_target_mapping_not_runtime_import_attestation",
    })
}
#[cfg(test)]
#[path = "candidate_contract_run_tests.rs"]
mod tests;
