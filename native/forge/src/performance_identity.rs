use crate::local_check_receipt::sha256_file;
use anyhow::{ensure, Context, Result};
use conductor_native::performance_receipt::{digest, Identity};
use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::process::Command;

pub fn files(root: &Path, paths: &[String]) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for path in paths {
        let relative = Path::new(path);
        ensure!(
            !relative.is_absolute()
                && relative
                    .components()
                    .all(|p| matches!(p, Component::Normal(_))),
            "source/input must be an exact checkout-relative file: {path}"
        );
        let actual = root
            .join(relative)
            .canonicalize()
            .with_context(|| format!("resolving input {path}"))?;
        ensure!(
            actual.starts_with(root) && actual.is_file(),
            "input escapes checkout or is not a file: {path}"
        );
        result.insert(path.clone(), sha256_file(&actual)?);
    }
    Ok(result)
}

pub fn environment(overrides: &[(String, String)]) -> Result<String> {
    let mut env = BTreeMap::new();
    for (key, value) in std::env::vars_os() {
        env.insert(
            key.into_string()
                .map_err(|_| anyhow::anyhow!("non-UTF-8 environment key"))?,
            value
                .into_string()
                .map_err(|_| anyhow::anyhow!("non-UTF-8 environment value"))?,
        );
    }
    for (key, value) in overrides {
        env.insert(key.clone(), value.clone());
    }
    // Values can include credentials; only their digest is persisted.
    Ok(digest(&serde_json::to_vec(&env)?))
}

pub fn toolchain() -> Result<String> {
    let output = Command::new("rustc")
        .arg("--version")
        .output()
        .context("rustc is required to identify the benchmark toolchain")?;
    ensure!(
        output.status.success(),
        "rustc --version failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

pub fn platform() -> Result<String> {
    let cpu = std::fs::read_to_string("/proc/cpuinfo")
        .context("benchmark requires Linux /proc/cpuinfo")?;
    let model = cpu
        .lines()
        .find(|line| line.starts_with("model name"))
        .unwrap_or("unknown CPU");
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")?;
    let mut identity = format!(
        "{model}; kernel={}; arch={}; cpus={}",
        kernel.trim(),
        std::env::consts::ARCH,
        std::thread::available_parallelism()?.get()
    );
    for path in ["/sys/fs/cgroup/cpu.max", "/sys/fs/cgroup/memory.max"] {
        match std::fs::read_to_string(path) {
            Ok(text) => identity.push_str(&format!("; {path}={}", text.trim())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                identity.push_str(&format!("; {path}=absent"))
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(identity)
}

pub fn identity(
    root: &Path,
    sources: &[String],
    command: &str,
    inputs: &[String],
    overrides: &[(String, String)],
    warmups: usize,
    timeout: u64,
) -> Result<Identity> {
    let workload = serde_json::to_vec(&(command, files(root, inputs)?))?;
    let head = crate::local_check::git_bytes(root, &["rev-parse", "HEAD"])?;
    Ok(Identity {
        host: root.display().to_string(),
        head: String::from_utf8(head)?.trim().into(),
        source_files: files(root, sources)?,
        command: command.into(),
        workload_sha256: digest(&workload),
        environment_sha256: environment(overrides)?,
        platform: platform()?,
        toolchain: toolchain()?,
        warmups,
        timeout_seconds: timeout,
    })
}

pub fn parse_env(values: &[String]) -> Result<Vec<(String, String)>> {
    values
        .iter()
        .map(|text| {
            let (key, value) = text.split_once('=').context("--env needs NAME=value")?;
            ensure!(
                !key.is_empty()
                    && !key.starts_with(|c: char| c.is_ascii_digit())
                    && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                "invalid environment name {key}"
            );
            Ok((key.into(), value.into()))
        })
        .collect()
}

pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
