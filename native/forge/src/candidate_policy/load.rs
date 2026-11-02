//! Snapshot-confined input and configured path resolution from captured bytes.
use super::{normalize, validate_today, Policy, MAX_BYTES, MAX_STRING};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, MetadataExt, OpenOptions, OpenOptionsExt};
use conductor_native::{project_config, project_paths};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

fn directory_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_DIRECTORY);
    options
}

// Landlock allows traversal through ancestors without granting READ_DIR on
// them. O_PATH preserves that distinction while retaining directory and
// no-symlink checks; the final directory is reopened for actual reads.
#[cfg(target_os = "linux")]
fn traversal_options() -> OpenOptions {
    let mut options = directory_options();
    options.custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_DIRECTORY);
    options
}

#[cfg(not(target_os = "linux"))]
fn traversal_options() -> OpenOptions {
    directory_options()
}

fn snapshot_root(path: &Path) -> Result<Dir> {
    ensure!(
        !path.as_os_str().is_empty(),
        "candidate snapshot path is empty"
    );
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    ensure!(
        absolute.as_os_str().len() <= 4096 && absolute.components().count() <= 256,
        "candidate snapshot path exceeds limits"
    );
    #[cfg(target_os = "linux")]
    let root = {
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/")?;
        Dir::from_std_file(file)
    };
    #[cfg(not(target_os = "linux"))]
    let root = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    let mut ancestors = vec![root];
    for component in absolute.components() {
        match component {
            Component::Normal(name) => {
                let file = ancestors
                    .last()
                    .context("snapshot ancestor missing")?
                    .open_with(name, &traversal_options())
                    .context("snapshot ancestors must be real directories without symlinks")?;
                ancestors.push(Dir::from_std_file(file.into_std()));
            }
            Component::ParentDir => {
                ensure!(ancestors.len() > 1, "snapshot path escapes filesystem root");
                ancestors.pop();
            }
            Component::RootDir | Component::CurDir => {}
            Component::Prefix(_) => anyhow::bail!("unsupported candidate snapshot path"),
        }
    }
    let selected = ancestors
        .pop()
        .context("candidate snapshot directory missing")?;
    let readable = selected
        .open_with(".", &directory_options())
        .context("open candidate snapshot directory for reading")?;
    Ok(Dir::from_std_file(readable.into_std()))
}

fn relative(raw: &str) -> Result<String> {
    ensure!(
        !raw.is_empty() && raw.len() <= MAX_STRING && !raw.contains('\0'),
        "policy path must be nonempty and at most 4096 bytes without NUL"
    );
    let path = Path::new(raw);
    ensure!(
        !path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_) | Component::CurDir)),
        "policy path must be candidate-relative without traversal"
    );
    let path: PathBuf = path
        .components()
        .filter_map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    ensure!(
        !path.as_os_str().is_empty(),
        "policy path must name a candidate file"
    );
    Ok(path
        .to_str()
        .context("policy path must be UTF-8")?
        .to_owned())
}

// cap-std follows relative symlinks only within this directory capability. It
// refuses absolute targets and escapes; nonblocking mode avoids FIFO hangs.
fn read(root: &Dir, path: &str, maximum: usize) -> Result<Option<Vec<u8>>> {
    match root.symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspect candidate policy input"),
    }
    let mut options = OpenOptions::new();
    options.read(true).custom_flags(libc::O_NONBLOCK);
    let mut file = root
        .open_with(path, &options)
        .with_context(|| format!("open confined candidate input {path}"))?;
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= maximum as u64,
        "candidate policy input must be a regular file within its byte limit"
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= maximum,
        "candidate policy input exceeds byte limit"
    );
    let after = file.metadata()?;
    ensure!(
        before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "candidate policy input changed while reading"
    );
    Ok(Some(bytes))
}

fn configured_path(root: &Dir, snapshot: &Path) -> Result<String> {
    // The project-path environment override precedes manifest loading in core.
    let override_set = match std::env::var("CONDUCTOR_CANDIDATE_POLICY") {
        Ok(value) => !value.trim().is_empty(),
        Err(std::env::VarError::NotPresent) => false,
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("CONDUCTOR_CANDIDATE_POLICY must be UTF-8")
        }
    };
    let config = if override_set {
        project_config::parse_legacy(&snapshot.join("pyproject.toml"), "")
    } else if let Some(bytes) = read(root, "forge.toml", project_config::CONFIG_LIMIT)? {
        project_config::parse_native(
            &snapshot.join("forge.toml"),
            std::str::from_utf8(&bytes).context("candidate forge.toml must be UTF-8")?,
        )
    } else {
        let bytes = read(root, "pyproject.toml", project_config::CONFIG_LIMIT)?.unwrap_or_default();
        project_config::parse_legacy(
            &snapshot.join("pyproject.toml"),
            std::str::from_utf8(&bytes).context("candidate pyproject.toml must be UTF-8")?,
        )
    }
    .map_err(anyhow::Error::msg)?;
    let (path, _) =
        project_paths::candidate_policy_relative(&config).map_err(anyhow::Error::msg)?;
    relative(&path)
}

pub(super) fn load(
    snapshot: &Path,
    explicit: Option<&str>,
    policy_env: Option<&str>,
    today: &str,
) -> Result<Policy> {
    validate_today(today)?;
    let root = snapshot_root(snapshot)?;
    let selected = explicit
        .filter(|s| !s.is_empty())
        .or_else(|| policy_env.map(str::trim).filter(|s| !s.is_empty()));
    let path = match selected {
        Some(path) => relative(path)?,
        None => configured_path(&root, snapshot)?,
    };
    let bytes = read(&root, &path, MAX_BYTES)?
        .with_context(|| format!("required candidate policy is absent: {path}"))?;
    let raw = normalize::parse(&bytes)?;
    let document = conductor_native::candidate_policy::parse_policy(&raw, today)
        .map_err(anyhow::Error::msg)?;
    normalize::validate_json(&document)?;
    Ok(Policy {
        path,
        digest: format!("{:x}", Sha256::digest(&bytes)),
        document,
    })
}
