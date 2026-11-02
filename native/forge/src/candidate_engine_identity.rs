//! Loaded native engine identity bound to an owned candidate snapshot.
//! The caller supplies the CLI's compile-time source stamp, not an environment
//! variable. These measurements are not a signature, approval, or proof that
//! source bytes were built honestly into the executable.
use crate::{
    candidate_assessment::{self, CheckResult, CheckStatus, Finding, Severity},
    candidate_policy::{self, Policy},
    candidate_snapshot::Snapshot,
    candidate_source::{Candidate, Source, TreeEntry},
    source_fingerprint::Fingerprint,
};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use serde_json::{json, Map};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
    time::Instant,
};

const MAX_EXECUTABLE: u64 = 1024 * 1024 * 1024;
const MAX_INPUT_JSON: usize = 64 * 1024 * 1024;
const REQUIRED: &[&str] = &[
    "native/forge/Cargo.toml",
    "native/forge/Cargo.lock",
    "native/conductor-native/Cargo.toml",
    "native/conductor-native/Cargo.lock",
    "native/slop-core/Cargo.toml",
    "native/slop-core/Cargo.lock",
    "native/forge-cli/Cargo.toml",
    "native/forge-cli/build.rs",
    "native/forge-cli/source_identity.rs",
    "ledger/routing_policy.toml",
    "native/forge/tests/fixtures/ledger/calibration.json",
];
#[cfg(test)]
const CRATES: &[&str] = &[
    "native/forge",
    "native/conductor-native",
    "native/slop-core",
];
const SOURCES: &[&str] = &[
    "native/forge/src/",
    "native/conductor-native/src/",
    "native/slop-core/src/",
];
const OPTIONAL: &[&str] = &[
    "native/forge/build.rs",
    "native/conductor-native/build.rs",
    "native/slop-core/build.rs",
];

#[derive(Debug, Serialize)]
pub struct Evidence {
    pub schema_version: u32,
    pub candidate_tree: String,
    pub candidate_input_sha256: String,
    pub policy_path: String,
    pub policy_sha256: String,
    pub normalized_policy_sha256: String,
    pub executable_sha256: String,
    pub executable_bytes: u64,
    pub executable_dev: u64,
    pub executable_inode: u64,
    pub compiled_source_sha256: String,
    pub compiled_git_commit: Option<String>,
    pub compiled_source_matches_git: bool,
    pub candidate_source_sha256: Option<String>,
    pub candidate_source_files: usize,
    pub candidate_git_pin: Option<String>,
    pub candidate_lock_sha256: Option<String>,
    pub mode: &'static str,
    pub provenance: &'static str,
}

/// Construct solely from the trusted CLI compilation unit's `env!` literals.
pub struct BuildStamp {
    pub source_sha256: &'static str,
    pub git_commit: &'static str,
    pub source_matches_git: &'static str,
}

/// Caller passes the literal `env!("FORGE_SOURCE_SHA256")` from the trusted
/// CLI compilation unit. Do not supply process environment or candidate data.
/// `snapshot` must remain owned by the coordinator through the review.
/// Failure to collect must become a blocking engine-integrity result.
pub fn collect(
    source: &Source,
    snapshot: &Snapshot,
    candidate: &Candidate,
    policy: &Policy,
    today: &str,
    stamp: &BuildStamp,
) -> Result<(Evidence, CheckResult)> {
    let started = Instant::now();
    ensure!(
        valid_sha256(stamp.source_sha256),
        "native CLI source stamp missing or malformed"
    );
    let clean = match stamp.source_matches_git {
        "1" => true,
        "0" => false,
        _ => anyhow::bail!("native CLI clean-source stamp is malformed"),
    };
    let commit = match stamp.git_commit {
        "unknown" => None,
        value if valid_commit(value) => Some(value.to_owned()),
        _ => anyhow::bail!("native CLI full revision stamp is malformed"),
    };
    ensure!(
        !clean || commit.is_some(),
        "clean native build lacks a full Git revision"
    );
    ensure!(
        snapshot.report().tree_oid == candidate.tree_oid,
        "candidate and owned snapshot tree differ"
    );
    let _held = snapshot.directory()?;
    check_policy(snapshot, policy, today)?;
    let candidate_input_sha256 = hash_json(candidate)?;
    let normalized_policy_sha256 = hash_json(&policy.document)?;
    let binary = running_executable()?;
    let entries = source.list_tree(&candidate.tree_oid)?;
    let (candidate_source_sha256, candidate_source_files) =
        source_tree_fingerprint(source, &entries)?;
    let (candidate_git_pin, candidate_lock_sha256, held_lock) = if candidate_source_sha256.is_none()
    {
        installed_pin(snapshot, &entries)?
    } else {
        (None, None, None)
    };
    let _held = snapshot.directory()?;
    check_policy(snapshot, policy, today)?;
    if let Some(held) = &held_lock {
        held.verify()?;
    } else if candidate_source_sha256.is_none() {
        ensure_lock_absent(snapshot)?;
    }
    let mode = if candidate_source_sha256.is_some() {
        "in_tree"
    } else {
        "installed"
    };
    let evidence = Evidence {
        schema_version: 1,
        candidate_tree: candidate.tree_oid.clone(),
        candidate_input_sha256,
        policy_path: policy.path.clone(),
        policy_sha256: policy.digest.clone(),
        normalized_policy_sha256,
        executable_sha256: binary.sha256,
        executable_bytes: binary.bytes,
        executable_dev: binary.dev,
        executable_inode: binary.ino,
        compiled_source_sha256: stamp.source_sha256.into(),
        compiled_git_commit: commit,
        compiled_source_matches_git: clean,
        candidate_source_sha256,
        candidate_source_files,
        candidate_git_pin,
        candidate_lock_sha256,
        mode,
        provenance: "self_observation_not_attestation",
    };
    let result = result_for(&evidence, started.elapsed().as_millis());
    Ok((evidence, result))
}
fn check_policy(snapshot: &Snapshot, policy: &Policy, today: &str) -> Result<()> {
    let loaded = candidate_policy::load(&snapshot.path(), Some(&policy.path), today)?;
    ensure!(
        loaded.digest == policy.digest && loaded.document == policy.document,
        "captured candidate policy changed or differs from supplied policy"
    );
    Ok(())
}
fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BinaryIdentity {
    pub(crate) sha256: String,
    pub(crate) bytes: u64,
    pub(crate) dev: u64,
    pub(crate) ino: u64,
}
fn running_executable() -> Result<BinaryIdentity> {
    // `/proc/self/exe` opens the inode backing this process, including when the
    // original path was atomically replaced. The proc link is intentional.
    let file = File::open("/proc/self/exe").context("open running native executable")?;
    executable_file(file, || Ok(()))
}
pub(crate) fn executable_file(
    mut file: File,
    before_final_stat: impl FnOnce() -> Result<()>,
) -> Result<BinaryIdentity> {
    let before = file.metadata()?;
    ensure!(
        before.is_file() && before.len() <= MAX_EXECUTABLE,
        "native executable is not a bounded regular file"
    );
    let mut hash = Sha256::new();
    let mut length = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        length = length
            .checked_add(count as u64)
            .context("executable length overflow")?;
        ensure!(
            length <= MAX_EXECUTABLE,
            "native executable exceeds byte limit"
        );
        hash.update(&buffer[..count]);
    }
    before_final_stat()?;
    let after = file.metadata()?;
    ensure!(
        length == before.len() && stable(&before, &after),
        "native executable changed while hashing"
    );
    Ok(BinaryIdentity {
        sha256: format!("{:x}", hash.finalize()),
        bytes: length,
        dev: before.dev(),
        ino: before.ino(),
    })
}
fn stable(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mode() == b.mode()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
struct HashWriter {
    hash: Sha256,
    len: usize,
}
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.len = self
            .len
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_INPUT_JSON)
            .ok_or_else(|| std::io::Error::other("engine input exceeds JSON byte limit"))?;
        self.hash.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn hash_json(value: &impl Serialize) -> Result<String> {
    let mut writer = HashWriter {
        hash: Sha256::new(),
        len: 0,
    };
    serde_json::to_writer(&mut writer, value)?;
    Ok(format!("{:x}", writer.hash.finalize()))
}

fn relevant(path: &str) -> bool {
    REQUIRED.contains(&path)
        || OPTIONAL.contains(&path)
        || SOURCES.iter().any(|prefix| path.starts_with(prefix))
}
fn source_tree_fingerprint(
    source: &Source,
    entries: &[TreeEntry],
) -> Result<(Option<String>, usize)> {
    if !entries.iter().any(|entry| {
        [
            "native/forge/",
            "native/forge-cli/",
            "native/conductor-native/",
            "native/slop-core/",
        ]
        .iter()
        .any(|prefix| entry.path.starts_with(prefix) || entry.path == prefix.trim_end_matches('/'))
    }) {
        return Ok((None, 0));
    }
    let files: BTreeMap<&str, &TreeEntry> = entries
        .iter()
        .filter(|entry| relevant(&entry.path))
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    ensure!(
        files.len() <= 4096,
        "candidate native source inventory exceeds 4096 files"
    );
    for required in REQUIRED {
        ensure!(
            files.contains_key(required),
            "candidate native source inventory is incomplete: {required}"
        );
    }
    for prefix in SOURCES {
        ensure!(
            files.keys().any(|path| path.starts_with(prefix)),
            "candidate native source directory is absent: {prefix}"
        );
    }
    let mut fingerprint = Fingerprint::new();
    // Build-time inventory uses Path ordering (components), which differs from
    // string byte order for siblings such as forge/ and forge-cli/.
    let mut ordered: Vec<_> = files.iter().collect();
    ordered.sort_by(|(a, _), (b, _)| Path::new(a).cmp(Path::new(b)));
    for (path, entry) in ordered {
        ensure!(
            matches!(entry.mode.as_str(), "100644" | "100755") && entry.object_type == "blob",
            "candidate native source must be a regular Git blob: {path}"
        );
        ensure!(
            entry.size.is_some_and(|size| size <= 2 * 1024 * 1024),
            "candidate native source blob exceeds 2 MiB: {path}"
        );
        let oid = git2::Oid::from_str(&entry.oid)?;
        let blob = source.repository().find_blob(oid)?;
        ensure!(
            blob.size() as u64 == entry.size.context("candidate source size missing")?,
            "candidate native source blob length changed"
        );
        ensure!(
            git2::Oid::hash_object(git2::ObjectType::Blob, blob.content())? == oid,
            "candidate native source blob object ID differs"
        );
        fingerprint.add(Path::new(path), blob.content())?;
    }
    Ok((Some(fingerprint.finish()), files.len()))
}
fn result_for(evidence: &Evidence, elapsed_ms: u128) -> CheckResult {
    let problem = match evidence.candidate_source_sha256.as_deref() {
        Some(value) if value == evidence.compiled_source_sha256 => None,
        Some(_) => Some((
            "dirty-engine-source",
            "running native engine's compiled source stamp differs from the exact candidate tree",
        )),
        None if !evidence.compiled_source_matches_git => Some((
            "engine-native-build-unverified",
            "running native build is not bound to a clean full source revision",
        )),
        None if evidence.candidate_git_pin.is_none() => Some((
            "engine-absent-from-candidate",
            "candidate does not contain native engine sources or an exact installed-engine Git pin",
        )),
        None if evidence.candidate_git_pin != evidence.compiled_git_commit => Some((
            "engine-pin-mismatch",
            "candidate engine pin differs from the running native build revision",
        )),
        None => None,
    };
    let findings = problem
        .into_iter()
        .map(|(rule_id, message)| {
            let mut finding = Finding {
                check_id: "engine-integrity".into(),
                rule_id: rule_id.into(),
                severity: Severity::Critical,
                message: message.into(),
                path: None,
                line: None,
                column: None,
                help: None,
                evidence: Map::new(),
                fingerprint: String::new(),
                exception_id: None,
                inherited: false,
            };
            candidate_assessment::finalize_finding(&mut finding);
            finding
        })
        .collect::<Vec<_>>();
    CheckResult {
        check_id: "engine-integrity".into(),
        status: if findings.is_empty() {
            CheckStatus::Passed
        } else {
            CheckStatus::Failed
        },
        duration_ms: u64::try_from(elapsed_ms).unwrap_or(u64::MAX),
        findings,
        files: Vec::new(),
        command: Vec::new(),
        tool_version: None,
        cache_key: None,
        cache_hit: false,
        exit_code: None,
        skipped_reason: None,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        metrics: Map::from_iter([
            ("mode".into(), json!(evidence.mode)),
            (
                "candidate_source_sha256".into(),
                json!(evidence.candidate_source_sha256),
            ),
            (
                "compiled_source_sha256".into(),
                json!(evidence.compiled_source_sha256),
            ),
            (
                "executable_sha256".into(),
                json!(evidence.executable_sha256),
            ),
            (
                "compiled_git_commit".into(),
                json!(evidence.compiled_git_commit),
            ),
            (
                "compiled_source_matches_git".into(),
                json!(evidence.compiled_source_matches_git),
            ),
            (
                "candidate_git_pin".into(),
                json!(evidence.candidate_git_pin),
            ),
        ]),
    }
}

#[cfg(test)]
#[path = "candidate_engine_identity_tests.rs"]
mod tests;

fn ensure_lock_absent(snapshot: &Snapshot) -> Result<()> {
    let root = snapshot.directory()?;
    ensure!(
        conductor_native::candidate_cargo_lint::read_optional(&root, "uv.lock", 8 * 1024 * 1024)
            .map_err(anyhow::Error::msg)?
            .is_none(),
        "uncaptured uv.lock appeared in candidate snapshot"
    );
    Ok(())
}
fn installed_pin(
    snapshot: &Snapshot,
    entries: &[TreeEntry],
) -> Result<(
    Option<String>,
    Option<String>,
    Option<crate::candidate_inputs::BoundFile>,
)> {
    let Some(entry) = entries.iter().find(|entry| entry.path == "uv.lock") else {
        ensure_lock_absent(snapshot)?;
        return Ok((None, None, None));
    };
    ensure!(
        matches!(entry.mode.as_str(), "100644" | "100755") && entry.object_type == "blob",
        "candidate uv.lock must be a regular Git blob"
    );
    let (held, bytes) = crate::candidate_inputs::snapshot(
        &snapshot.path(),
        "uv.lock",
        &entry.oid,
        8 * 1024 * 1024,
    )?;
    let sha = format!("{:x}", Sha256::digest(&bytes));
    let text = std::str::from_utf8(&bytes).context("candidate uv.lock must be UTF-8")?;
    let value: toml::Value =
        toml::from_str(text).context("candidate uv.lock must be valid TOML")?;
    let pin = uv_pin(&value)?;
    held.verify()?;
    Ok((pin, Some(sha), Some(held)))
}
fn uv_pin(value: &toml::Value) -> Result<Option<String>> {
    let Some(packages) = value.get("package") else {
        return Ok(None);
    };
    let packages = packages
        .as_array()
        .context("uv.lock package must be an array")?;
    ensure!(
        packages.len() <= 100_000,
        "uv.lock package count exceeds limit"
    );
    let mut selected = None;
    for package in packages {
        let package = package
            .as_table()
            .context("uv.lock package must be a table")?;
        if package.get("name").and_then(toml::Value::as_str) != Some("conductor-tooling") {
            continue;
        }
        ensure!(
            selected.is_none(),
            "uv.lock has multiple conductor-tooling package records"
        );
        selected = Some(package);
    }
    let git = selected
        .and_then(|package| package.get("source"))
        .and_then(toml::Value::as_table)
        .and_then(|source| source.get("git"))
        .and_then(toml::Value::as_str);
    Ok(git
        .and_then(|git| git.rsplit_once('#'))
        .map(|(_, pin)| pin)
        .filter(|pin| valid_commit(pin))
        .map(str::to_owned))
}
