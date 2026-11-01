//! Native claim transactions. Legacy direct Python writers do not all take this lock.
use crate::{
    instant,
    ownership::{self, OwnershipClaim},
};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

pub struct Lease(File);
impl Drop for Lease {
    fn drop(&mut self) {
        if unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) } != 0 {
            eprintln!(
                "forge: unlock governance lease: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

/// Locks the same inode as Python's governance CLI; inherited leases are not
/// trusted or reused by native mutation commands. Readable legacy lock files
/// are accepted; other users must not be able to alter their contents.
pub fn acquire(repo: &Path, timeout: Duration) -> Result<Lease> {
    if [
        "LLM_GOVERNANCE_COMMIT_LOCK_FD",
        "LLM_GOVERNANCE_COMMIT_LOCK_TOKEN",
    ]
    .iter()
    .any(|key| std::env::var_os(key).is_some())
    {
        bail!("inherited governance lease cannot be verified for native mutation");
    }
    let path = ownership::checked_claim_path(repo, "commit-review.lock")?
        .context("repository has no Git common directory")?;
    fs::create_dir_all(path.parent().context("governance lock parent")?)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.nlink() == 1
            && meta.mode() & 0o022 == 0,
        "governance lock must be owned, regular, singly linked and not writable by others"
    );
    let deadline = Instant::now() + timeout;
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(Lease(file));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock
            && error.kind() != std::io::ErrorKind::Interrupted
        {
            return Err(error.into());
        }
        if Instant::now() >= deadline {
            bail!(
                "governance lock remained busy for {:.1}s",
                timeout.as_secs_f64()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn fields(claim: &OwnershipClaim) -> Value {
    let mut result = json!({"claim_id":claim.claim_id,"owner":claim.owner,"paths":claim.paths,
        "justification":claim.justification,"created_at":claim.created_at,"expires_at":claim.expires_at});
    if let Some(expected) = &claim.expected_at {
        result["expected_at"] = json!(expected);
    }
    result
}
pub fn stored_claim(claim: &OwnershipClaim) -> Value {
    fields(claim)
}
fn publish(repo: &Path, claims: &[OwnershipClaim]) -> Result<()> {
    let path = ownership::checked_claim_path(repo, "ownership-claims.json")?
        .context("repository has no Git common directory")?;
    let data = json!({"schema_version":1,"claims":claims.iter().map(fields).collect::<Vec<_>>()});
    crate::state_publish::publish(&path, &serde_json::to_vec_pretty(&data)?)?;
    // Two atomic files are not a transaction; stale activity is ignored by ID.
    let mut seen = ownership::load_activity(repo)?;
    let retained: BTreeSet<_> = claims.iter().map(|claim| claim.claim_id.as_str()).collect();
    let before = seen.len();
    seen.retain(|id, _| retained.contains(id.as_str()));
    if seen.len() != before {
        ownership::write_activity(repo, &seen)?;
    }
    Ok(())
}

pub struct Create<'a> {
    pub owner: &'a str,
    pub paths: &'a [String],
    pub justification: &'a str,
    pub expected_minutes: f64,
    pub max_minutes: f64,
}
pub fn create(repo: &Path, request: Create<'_>) -> Result<OwnershipClaim> {
    let owner = request.owner.trim();
    let justification = request.justification.trim();
    ensure!(
        !owner.is_empty() && !justification.is_empty(),
        "owner and justification are required"
    );
    ensure!(
        request.expected_minutes.is_finite()
            && request.max_minutes.is_finite()
            && request.expected_minutes > 0.0
            && request.expected_minutes <= request.max_minutes
            && request.max_minutes <= 120.0,
        "claim durations must satisfy 0 < expected <= maximum <= 120 minutes"
    );
    let paths: Vec<String> = request
        .paths
        .iter()
        .map(|p| ownership::normalize_claim_path(p))
        .collect::<std::result::Result<BTreeSet<_>, _>>()?
        .into_iter()
        .collect();
    ensure!(!paths.is_empty(), "at least one exact path is required");
    let _lease = acquire(repo, Duration::from_secs(30))?;
    let now = instant::now();
    let (claims, _) = ownership::load_claims(repo)?;
    let mut active = Vec::new();
    for claim in claims {
        if !claim.active(now)? {
            continue;
        }
        for requested in &paths {
            for existing in &claim.paths {
                ensure!(
                    !ownership::paths_overlap(requested, existing),
                    "path {requested:?} overlaps active claim {} owned by {:?} at {existing:?}",
                    claim.claim_id,
                    claim.owner
                );
            }
        }
        active.push(claim);
    }
    let created = instant::isoformat_utc(now);
    let expiry = instant::isoformat_utc(now + request.max_minutes * 60.0);
    let expected = instant::isoformat_utc(now + request.expected_minutes * 60.0);
    let created_instant = instant::parse(&created).context("invalid creation time")?;
    let expiry_instant = instant::parse(&expiry).context("invalid expiry time")?;
    let expected_instant = instant::parse(&expected).context("invalid expected time")?;
    ensure!(
        expected_instant > created_instant && expected_instant <= expiry_instant,
        "claim duration is too small to represent"
    );
    let identity = ownership::sha256_json(
        &json!({"owner":owner,"paths":paths,"justification":justification,
        "created_at":created,"expires_at":expiry,"expected_at":expected}),
    );
    let claim = OwnershipClaim {
        claim_id: format!("claim-{}", &identity[..20]),
        owner: owner.into(),
        paths,
        justification: justification.into(),
        created_at: created,
        expires_at: expiry,
        expected_at: Some(expected),
        last_seen: None,
    };
    active.push(claim.clone());
    publish(repo, &active)?;
    Ok(claim)
}

pub fn release(repo: &Path, id: &str, owner: &str) -> Result<bool> {
    let _lease = acquire(repo, Duration::from_secs(30))?;
    let (mut claims, _) = ownership::load_claims(repo)?;
    let Some(index) = claims.iter().position(|claim| claim.claim_id == id) else {
        return Ok(false);
    };
    ensure!(
        claims[index].owner.to_lowercase() == owner.trim().to_lowercase(),
        "claim {id} is owned by {:?}, not {owner:?}",
        claims[index].owner
    );
    claims.remove(index);
    publish(repo, &claims)?;
    Ok(true)
}
pub fn touch(repo: &Path, id: &str, owner: &str) -> Result<bool> {
    let _lease = acquire(repo, Duration::from_secs(30))?;
    let (claims, _) = ownership::load_claims(repo)?;
    let claim = claims
        .iter()
        .find(|claim| claim.claim_id == id)
        .with_context(|| format!("no claim {id}"))?;
    ensure!(
        claim.owner == owner,
        "claim {id} is owned by {:?}, not {owner:?}",
        claim.owner
    );
    let now = instant::now();
    ensure!(
        claim.active(now)?,
        "claim {id} already lapsed: {}",
        claim.lapse_reason(now)?
    );
    Ok(ownership::touch_claim_unlocked(repo, id, now)?)
}
pub fn list(repo: &Path) -> Result<(Vec<OwnershipClaim>, String)> {
    let _lease = acquire(repo, Duration::from_secs(30))?;
    Ok(ownership::load_claims(repo)?)
}
