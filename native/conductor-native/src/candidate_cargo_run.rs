//! Shared audit composition: confined lock reads, one work budget, explicit evidence.
use crate::{
    candidate_cargo_audit,
    candidate_cargo_engine::Observation,
    candidate_cargo_evidence::{self, VerifiedEvidence},
    candidate_checks::Finding,
};
use cap_std::fs::{Dir, Metadata, MetadataExt, OpenOptions, OpenOptionsExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

const MAX_BYTES: usize = 32 * 1024 * 1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    source_root: PathBuf,
    snapshot: PathBuf,
    evidence_spec: PathBuf,
    files: Vec<String>,
}
/// Compatibility binding only: callers supply paths and selected file metadata.
/// The same native loader and evaluator serve the standalone Forge binary.
pub fn evaluate_payload(payload: &Value) -> Result<Value, String> {
    let request: Request =
        serde_json::from_value(payload.clone()).map_err(|_| "invalid cargo-audit request")?;
    let excluded = [request.source_root.as_path(), request.snapshot.as_path()];
    let spec = candidate_cargo_evidence::read_spec(&request.evidence_spec, &excluded)?;
    let evidence =
        candidate_cargo_evidence::load(&spec, &excluded, candidate_cargo_evidence::now()?)?;
    let root = Dir::open_ambient_dir(&request.snapshot, cap_std::ambient_authority())
        .map_err(|_| "cannot open candidate audit snapshot")?;
    let files: Vec<&str> = request.files.iter().map(String::as_str).collect();
    evaluate(&root, &files, &evidence, || Ok(()))
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    a.is_file()
        && b.is_file()
        && a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn read_lock(root: &Dir, path: &str, remaining: &mut usize) -> Result<String, String> {
    let mut directory = root
        .try_clone()
        .map_err(|_| "cannot clone audit snapshot")?;
    let path = Path::new(path);
    for part in path.parent().ok_or("lockfile parent missing")?.components() {
        let std::path::Component::Normal(name) = part else {
            return Err("invalid lockfile parent".into());
        };
        let child = directory
            .open_with(
                name,
                OpenOptions::new().read(true).custom_flags(
                    libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                ),
            )
            .map_err(|_| "cannot open lockfile parent")?;
        directory = Dir::from_std_file(child.into_std());
    }
    let name = path.file_name().ok_or("lockfile name missing")?;
    let before = directory
        .symlink_metadata(name)
        .map_err(|_| "cannot inspect lockfile")?;
    if !before.is_file() || before.len() > 8 * 1024 * 1024 {
        return Err("lockfile must be regular and at most 8 MiB".into());
    }
    let size = usize::try_from(before.len()).map_err(|_| "lockfile length overflow")?;
    *remaining = remaining
        .checked_sub(size + 1)
        .ok_or("aggregate lockfile reads exceed 32 MiB")?;
    let mut file = directory
        .open_with(
            name,
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
        )
        .map_err(|_| "cannot open lockfile")?;
    let opened = file
        .metadata()
        .map_err(|_| "cannot inspect open lockfile")?;
    if !same(&before, &opened) {
        return Err("lockfile changed before reading".into());
    }
    let mut bytes = Vec::with_capacity(size);
    (&mut file)
        .take(size as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read lockfile")?;
    let after = file
        .metadata()
        .map_err(|_| "cannot inspect completed lockfile read")?;
    let current = directory
        .symlink_metadata(name)
        .map_err(|_| "cannot inspect current lockfile")?;
    if bytes.len() != size || !same(&opened, &after) || !same(&opened, &current) {
        return Err("lockfile changed while reading".into());
    }
    String::from_utf8(bytes).map_err(|_| "lockfile is not UTF-8".into())
}
fn push(findings: &mut Vec<Value>, finding: Finding, remaining: &mut usize) -> Result<(), String> {
    if findings.len() >= 100_000 {
        return Err("audit findings exceed 100000".into());
    }
    let value = serde_json::to_value(finding).map_err(|_| "cannot encode audit finding")?;
    let size = candidate_cargo_audit::serialized_size(&value, *remaining)?;
    *remaining = remaining
        .checked_sub(size + 1)
        .ok_or("audit findings exceed 32 MiB")?;
    findings.push(value);
    Ok(())
}
fn findings_for(
    path: &str,
    observation: &Observation,
    findings: &mut Vec<Value>,
    remaining: &mut usize,
) -> Result<(), String> {
    for item in &observation.matches {
        let informational = item.informational.is_some();
        push(
            findings,
            Finding::new(
                "cargo-audit",
                if informational {
                    "informational-advisory"
                } else {
                    "vulnerable-dependency"
                },
                if informational { "medium" } else { "critical" },
                format!(
                    "{} {}: {} — {}",
                    item.package, item.version, item.advisory_id, item.title
                ),
            )
            .path(path)
            .evidence(serde_json::to_value(item).map_err(|_| "cannot encode advisory evidence")?),
            remaining,
        )?;
    }
    for item in &observation.yanked {
        push(
            findings,
            Finding::new(
                "cargo-audit",
                "yanked-dependency",
                "medium",
                format!(
                    "{} {} is yanked in the pinned registry evidence",
                    item.name, item.version
                ),
            )
            .path(path)
            .evidence(serde_json::to_value(item).map_err(|_| "cannot encode registry evidence")?),
            remaining,
        )?;
    }
    Ok(())
}
pub fn evaluate(
    root: &Dir,
    files: &[&str],
    evidence: &VerifiedEvidence,
    mut live: impl FnMut() -> Result<(), String>,
) -> Result<Value, String> {
    live()?;
    let plan = candidate_cargo_audit::plan(root, files)?;
    let mut work = 1_000_000;
    let mut read_bytes = MAX_BYTES;
    let mut observation_bytes = MAX_BYTES;
    let mut finding_bytes = MAX_BYTES;
    let mut findings = Vec::new();
    let mut audits = Vec::new();
    for lockfile in &plan.lockfiles {
        live()?;
        let text = read_lock(root, lockfile, &mut read_bytes)?;
        let observation = evidence.auditor().audit_with_budget(&text, &mut work)?;
        findings_for(lockfile, &observation, &mut findings, &mut finding_bytes)?;
        let record = json!({"lockfile":lockfile,"observation":observation});
        let size = candidate_cargo_audit::serialized_size(&record, observation_bytes)?;
        observation_bytes = observation_bytes
            .checked_sub(size + 1)
            .ok_or("audit observations exceed 32 MiB")?;
        audits.push(record);
    }
    live()?;
    let result = json!({"findings":findings,"metrics":{
        "implementation":format!("rustsec/{}",rustsec::VERSION),"scope":"crates.io advisories and yanked versions",
        "evidence_sha256":evidence.digest(),"advisory_commit":evidence.auditor().advisory_commit(),
        "captured_at":evidence.auditor().captured_at(),"checked_at":evidence.checked_at(),
        "advisories":evidence.auditor().advisory_count(),"registry_versions":evidence.auditor().registry_count(),
        "owner_plan":plan,"audits":audits,"work_used":1_000_000-work}});
    candidate_cargo_audit::serialized_size(&result, MAX_BYTES)?;
    Ok(result)
}

#[cfg(test)]
#[path = "candidate_cargo_run_tests.rs"]
mod tests;
