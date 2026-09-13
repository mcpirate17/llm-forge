//! `conductor/active_state.json`: the compact (<500 token) Tier-0 state
//! summary, ported from `conductor.active_state`. Standing mandates from the
//! project session policy, the first four live `## ` headings of
//! `.current_work.md`, and the unexpired governance claims -- so agents never
//! parse the large markdown files on every turn. Every rule, key order and
//! message mirrors the Python, which stays the spec until LLM deletes it.

use crate::instant;
use crate::ownership::{self, OwnershipClaim};
use crate::session_policy::load_session_policy;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

/// Python's dataclass field order is the JSON key order; serde_json's
/// `preserve_order` (on in this crate) keeps struct order stable too.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ActiveClaim {
    pub claim_id: String,
    pub owner: String,
    pub paths: Vec<String>,
    pub justification: String,
    pub expires_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ActiveState {
    pub schema_version: i64,
    pub last_updated: String,
    pub standing_mandates: Vec<String>,
    pub active_headings: Vec<String>,
    pub active_claims: Vec<ActiveClaim>,
}

/// First `limit` `## ` headings of `.current_work.md`, skipping "Active
/// Coordination" (case-insensitive) -- the always-present section header is
/// noise in a token-conscious inject. A missing or unreadable file is an
/// empty list, never an error: the inject must survive a half-cloned repo.
pub fn parse_top_headings(current_work: &Path, limit: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(current_work) else {
        return Vec::new();
    };
    let mut headings = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("##") else {
            continue;
        };
        // `^##\s+(.+)$`: at least one whitespace byte after the marker.
        if !rest.starts_with(|c: char| c.is_whitespace()) {
            continue;
        }
        let heading = rest.trim();
        if heading.to_lowercase() == "active coordination" {
            continue;
        }
        headings.push(heading.to_owned());
        if headings.len() >= limit {
            break;
        }
    }
    headings
}

/// Unexpired claims from the governance ownership store, as the state JSON
/// carries them (`claim_id, owner, paths, justification, expires_at`).
pub fn parse_active_claims(repo: &Path) -> Result<Vec<ActiveClaim>, ownership::OwnershipError> {
    let (claims, _digest) = ownership::load_claims(repo)?;
    let now = instant::now();
    let mut active = Vec::new();
    for claim in &claims {
        if claim.active(now)? {
            active.push(active_claim(claim));
        }
    }
    Ok(active)
}

fn active_claim(claim: &OwnershipClaim) -> ActiveClaim {
    ActiveClaim {
        claim_id: claim.claim_id.clone(),
        owner: claim.owner.clone(),
        paths: claim.paths.clone(),
        justification: claim.justification.clone(),
        expires_at: claim.expires_at.clone(),
    }
}

pub fn generate_active_state(repo: &Path) -> Result<ActiveState> {
    let headings = parse_top_headings(&repo.join(".current_work.md"), 4);
    let active_claims = parse_active_claims(repo).map_err(anyhow::Error::from)?;
    let policy = load_session_policy(repo).map_err(|err| anyhow::anyhow!("{err}"))?;
    Ok(ActiveState {
        schema_version: 1,
        last_updated: instant::isoformat_utc(instant::now()),
        standing_mandates: policy.standing_mandates,
        active_headings: headings,
        active_claims,
    })
}

/// Reject stale or malformed authorization data before it reaches a session.
/// Mirrors `validate_active_state` rule for rule and message for message.
pub fn validate_active_state(state: &ActiveState, now: f64) -> Result<(), String> {
    if state.schema_version != 1 {
        return Err(format!(
            "unsupported active-state schema: {} (expected 1)",
            state.schema_version
        ));
    }
    let updated = instant::parse(&state.last_updated)
        .ok_or_else(|| format!("active-state last_updated is invalid: '{}'", state.last_updated))?;
    if updated > now + 60.0 {
        return Err("active-state last_updated is implausibly in the future".to_string());
    }
    if state.active_headings.len() > 4 {
        return Err(format!(
            "active-state contains {} headings (maximum 4)",
            state.active_headings.len()
        ));
    }
    for claim in &state.active_claims {
        let expiry = instant::parse(&claim.expires_at)
            .ok_or_else(|| "active-state claim has an invalid expires_at".to_string())?;
        if expiry <= now {
            return Err(format!(
                "active-state contains expired claim {}",
                claim.claim_id
            ));
        }
    }
    Ok(())
}

/// Write one validated state snapshot without exposing a partial JSON file:
/// temp file in the same directory, fsync, rename, fsync the directory --
/// `tempfile.mkstemp` + `os.replace` in the Python. Readers either see the
/// previous snapshot or the new one, never a torn write.
pub fn write_state_atomic(path: &Path, state: &ActiveState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut payload = serde_json::to_string_pretty(state).expect("plain data always serializes");
    payload.push('\n');
    let temp = path.parent().unwrap_or(Path::new(".")).join(format!(
        ".{}.{}.tmp",
        path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default(),
        std::process::id()
    ));
    let outcome = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(payload.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        if let Some(parent) = path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                dir.sync_all()?;
            }
        }
        Ok(())
    })();
    if outcome.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    outcome
}

/// Generate, validate, and atomically write `conductor/active_state.json`.
pub fn save_active_state(repo: &Path) -> Result<ActiveState> {
    let state = generate_active_state(repo)?;
    validate_active_state(&state, instant::now()).map_err(anyhow::Error::msg)?;
    let path = repo.join("conductor").join("active_state.json");
    write_state_atomic(&path, &state)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct ScratchRepo(std::path::PathBuf);
    impl ScratchRepo {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "forge-active-state-test-{label}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(dir.join(".git")).unwrap();
            ScratchRepo(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
        /// A claim whose id satisfies the store's content-hash check, using
        /// `ownership`'s own canonical form so the fixture can never drift
        /// from what `load_claims` verifies.
        fn write_claims(&self, claims: &[(&str, &str, &str)]) {
            let items: Vec<Value> = claims
                .iter()
                .map(|(owner, created, expires)| {
                    let fields = serde_json::json!({
                        "owner": owner,
                        "paths": ["src/foo.py"],
                        "justification": "because",
                        "created_at": created,
                        "expires_at": expires,
                    });
                    let identity = crate::ownership::sha256_json(&fields);
                    serde_json::json!({
                        "claim_id": format!("claim-{}", &identity[..20]),
                        "owner": owner,
                        "paths": ["src/foo.py"],
                        "justification": "because",
                        "created_at": created,
                        "expires_at": expires,
                    })
                })
                .collect();
            let dir = self.0.join(".git").join("governance");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("ownership-claims.json"),
                serde_json::to_string(&serde_json::json!({"schema_version": 1, "claims": items}))
                    .unwrap(),
            )
            .unwrap();
        }
    }
    impl Drop for ScratchRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn headings_skip_active_coordination_and_stop_at_the_limit() {
        let scratch = ScratchRepo::new("headings");
        let work = scratch.path().join(".current_work.md");
        std::fs::write(
            &work,
            "# Title\n\n## Active Coordination\nstuff\n\n## First\n\n   ## Second \n\n### Not a top heading\n\n## active coordination\n\n## Third\n## Fourth\n## Fifth\n",
        )
        .unwrap();
        assert_eq!(
            parse_top_headings(&work, 4),
            vec!["First", "Second", "Third", "Fourth"],
            "both casings of the coordination header are skipped and the limit holds"
        );
        assert!(parse_top_headings(&scratch.path().join("missing.md"), 4).is_empty());
    }

    #[test]
    fn only_unexpired_claims_reach_the_state() {
        let scratch = ScratchRepo::new("claims");
        let now = instant::now();
        let created = instant::isoformat_utc(now - 60.0);
        let live_expiry = instant::isoformat_utc(now + 3600.0);
        let past_expiry = instant::isoformat_utc(now - 3600.0);
        let older = instant::isoformat_utc(now - 7200.0);
        scratch.write_claims(&[
            ("llm-b0", &created, &live_expiry),
            ("llm-b1", &older, &past_expiry),
        ]);
        let claims = parse_active_claims(scratch.path()).unwrap();
        assert_eq!(claims.len(), 1, "the expired claim must not survive");
        assert_eq!(claims[0].owner, "llm-b0");
        assert_eq!(claims[0].expires_at, live_expiry);
        // And validation rejects the expired one when it is handed in.
        let expired_state = ActiveState {
            schema_version: 1,
            last_updated: instant::isoformat_utc(now),
            standing_mandates: vec![],
            active_headings: vec![],
            active_claims: vec![ActiveClaim {
                claim_id: "claim-x".to_string(),
                owner: "llm-b1".to_string(),
                paths: vec![],
                justification: String::new(),
                expires_at: past_expiry,
            }],
        };
        let err = validate_active_state(&expired_state, now).unwrap_err();
        assert!(err.contains("expired claim claim-x"), "{err}");
    }

    #[test]
    fn saving_writes_the_json_key_order_and_leaves_no_temp_behind() {
        let scratch = ScratchRepo::new("save");
        std::fs::write(scratch.path().join("pyproject.toml"), "").unwrap();
        let state = save_active_state(scratch.path()).unwrap();
        let path = scratch.path().join("conductor").join("active_state.json");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.ends_with("}\n"), "the Python writes a trailing newline");
        let value: Value = serde_json::from_str(&text).unwrap();
        let keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "schema_version",
                "last_updated",
                "standing_mandates",
                "active_headings",
                "active_claims"
            ],
            "the dataclass field order is the file's key order"
        );
        assert_eq!(value["schema_version"], Value::from(1));
        assert_eq!(value["active_headings"], Value::Array(vec![]));
        let leftovers: Vec<_> = std::fs::read_dir(scratch.path().join("conductor"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temp file may survive the rename");
        assert_eq!(state, serde_json::from_str::<ActiveState>(&text).unwrap());
    }
}
