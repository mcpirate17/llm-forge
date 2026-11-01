//! Operator-pinned offline audit evidence. A digest binds supplied bytes, not
//! upstream authenticity/completeness; the operator owns that trust decision.
use crate::candidate_cargo_engine::Auditor;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, Metadata, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: usize = 32 * 1024 * 1024;
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceSpec {
    pub path: PathBuf,
    pub sha256: String,
    pub max_age_seconds: u64,
}
pub struct VerifiedEvidence {
    auditor: Auditor,
    digest: String,
    checked_at: u64,
}
impl VerifiedEvidence {
    pub fn auditor(&self) -> &Auditor {
        &self.auditor
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn checked_at(&self) -> u64 {
        self.checked_at
    }
}
pub fn now() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|_| "audit clock precedes UNIX epoch".into())
}
fn admitted(meta: &Metadata, maximum: usize) -> bool {
    meta.is_file()
        && meta.uid() == unsafe { libc::geteuid() }
        && meta.nlink() == 1
        && meta.mode() & 0o022 == 0
        && meta.len() <= maximum as u64
}
fn stable(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.nlink() == b.nlink()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn io_error(action: &str, error: &std::io::Error) -> String {
    format!(
        "audit evidence {action}: {:?} errno={:?}",
        error.kind(),
        error.raw_os_error()
    )
}
fn location(path: &Path, forbidden: &[&Path]) -> Result<(), String> {
    if !path.is_absolute()
        || path.as_os_str().len() > 4096
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err("audit evidence path must be bounded and absolute".into());
    }
    let actual = fs::canonicalize(path).map_err(|error| io_error("resolve path", &error))?;
    if actual != path {
        return Err("audit evidence paths must not traverse symlinks".into());
    }
    for root in forbidden {
        let root =
            fs::canonicalize(root).map_err(|error| io_error("resolve excluded root", &error))?;
        if actual.starts_with(root) {
            return Err("audit evidence must be outside the source and candidate snapshot".into());
        }
    }
    Ok(())
}
/// The operator owns evidence ancestors and keeps them immutable during loading.
/// Selected candidate files cannot redirect evidence or supply its authority.
fn read_owned(path: &Path, forbidden: &[&Path], maximum: usize) -> Result<Vec<u8>, String> {
    location(path, forbidden)?;
    let before = fs::symlink_metadata(path).map_err(|error| io_error("inspect path", &error))?;
    if !admitted(&before, maximum) {
        return Err(
            "audit evidence must be an owned private regular single-link bounded file".into(),
        );
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| io_error("open", &error))?;
    let opened = file
        .metadata()
        .map_err(|error| io_error("inspect file", &error))?;
    if !stable(&before, &opened) || !admitted(&opened, maximum) {
        return Err("audit evidence identity changed before reading".into());
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    (&file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read", &error))?;
    let after = file
        .metadata()
        .map_err(|error| io_error("inspect completed read", &error))?;
    let current =
        fs::symlink_metadata(path).map_err(|error| io_error("inspect current path", &error))?;
    if bytes.len() > maximum
        || bytes.len() as u64 != opened.len()
        || !stable(&opened, &after)
        || !stable(&opened, &current)
    {
        return Err("audit evidence changed while reading".into());
    }
    location(path, forbidden)?;
    Ok(bytes)
}
pub fn read_spec(path: &Path, forbidden: &[&Path]) -> Result<EvidenceSpec, String> {
    serde_json::from_slice(&read_owned(path, forbidden, 64 * 1024)?)
        .map_err(|_| "invalid audit evidence specification".into())
}
pub fn load(
    spec: &EvidenceSpec,
    forbidden: &[&Path],
    checked_at: u64,
) -> Result<VerifiedEvidence, String> {
    if !(1..=31 * 86400).contains(&spec.max_age_seconds)
        || spec.sha256.len() != 64
        || !spec
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(
            "audit evidence requires lowercase SHA-256 and age limit 1..2678400 seconds".into(),
        );
    }
    let bytes = read_owned(&spec.path, forbidden, MAX_BYTES)?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if digest != spec.sha256 {
        return Err("audit evidence SHA-256 mismatch".into());
    }
    let auditor = Auditor::parse(&bytes)?;
    match checked_at.checked_sub(auditor.captured_at()) {
        Some(age) if age <= spec.max_age_seconds => {}
        _ => return Err("audit evidence is stale or future-dated".into()),
    }
    Ok(VerifiedEvidence {
        auditor,
        digest,
        checked_at,
    })
}

#[cfg(test)]
#[path = "candidate_cargo_evidence_tests.rs"]
mod tests;
