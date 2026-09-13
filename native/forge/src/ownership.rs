//! Native port of `conductor.candidate_review.ownership`: structured,
//! expiring ownership claims shared by linked Git worktrees. Only the
//! read-only surface `crg_gate_verify_bash` needs -- loading claims, the
//! expiry/idle-lapse math, `paths_overlap`, and `touch_claim` -- ports here.
//! Claim *creation* (`create_claim`, `normalize_claim_path`'s write-path
//! validation, `release_claim`) is out of scope: `verify_bash` never writes a
//! claim, only checks and touches an existing one.
//!
//! `claim_store_path`/`claim_activity_path` diverge from the Python module
//! deliberately: Python's `git_common_dir` shells a `git rev-parse
//! --git-common-dir` subprocess, which this hook path exists to avoid. This
//! port instead reuses the filesystem-only common-dir resolution
//! `crg_gate.rs`'s own `checkout_of` already computes (the same algorithm
//! `crg_gate.py`'s `_checkout_of` uses for `REPO_COMMON_DIR`) -- equivalent
//! for every layout this fleet actually runs (a main checkout plus linked
//! worktrees, no submodules), and disclosed as a known divergence.

use crate::instant::{self, format_hm, format_ymd_hm};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const MAX_CLAIM_HOURS: f64 = 24.0;
pub const MAX_ACTIVE_CLAIM_HOURS: f64 = 2.0;
pub const IDLE_LAPSE_MINUTES: f64 = 45.0;
pub const OVERRUN_IDLE_MINUTES: f64 = 10.0;
const TOUCH_DEBOUNCE_SECONDS: f64 = 60.0;
const CLAIM_SCHEMA_VERSION: i64 = 1;
const ACTIVITY_SCHEMA_VERSION: i64 = 1;

#[derive(Debug)]
pub struct OwnershipError(pub String);

impl std::fmt::Display for OwnershipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for OwnershipError {}

fn err<T>(message: impl Into<String>) -> Result<T, OwnershipError> {
    Err(OwnershipError(message.into()))
}

/// `_instant`: parse an ISO-8601 string, requiring a timezone offset.
fn parse_instant(raw: &str, claim_id: &str, label: &str) -> Result<f64, OwnershipError> {
    instant::parse(raw)
        .ok_or_else(|| OwnershipError(format!("claim {claim_id} {label} is unparseable: {raw:?}")))
}

#[derive(Debug, Clone)]
pub struct OwnershipClaim {
    pub claim_id: String,
    pub owner: String,
    pub paths: Vec<String>,
    pub justification: String,
    pub created_at: String,
    pub expires_at: String,
    pub expected_at: Option<String>,
    pub last_seen: Option<String>,
}

impl OwnershipClaim {
    pub fn expiry(&self) -> Result<f64, OwnershipError> {
        parse_instant(&self.expires_at, &self.claim_id, "expiry")
    }

    pub fn creation(&self) -> Result<f64, OwnershipError> {
        parse_instant(&self.created_at, &self.claim_id, "creation time")
    }

    pub fn expected(&self) -> Result<f64, OwnershipError> {
        let hard = self.hard_deadline()?;
        match &self.expected_at {
            None => Ok(hard),
            Some(raw) => {
                let expected = parse_instant(raw, &self.claim_id, "expected time")?;
                Ok(expected.min(hard))
            }
        }
    }

    pub fn activity(&self) -> Result<Option<f64>, OwnershipError> {
        match &self.last_seen {
            None => Ok(None),
            Some(raw) => Ok(Some(parse_instant(raw, &self.claim_id, "activity stamp")?)),
        }
    }

    /// The longest this claim may hold, whatever it asked for and however busy.
    pub fn hard_deadline(&self) -> Result<f64, OwnershipError> {
        let expiry = self.expiry()?;
        let creation = self.creation()?;
        Ok(expiry.min(creation + MAX_ACTIVE_CLAIM_HOURS * 3600.0))
    }

    /// Past the estimate. Still holds the path, but on a much shorter fuse.
    pub fn overrun(&self, now: f64) -> Result<bool, OwnershipError> {
        Ok(now > self.expected()?)
    }

    pub fn idle_window_seconds(&self, now: f64) -> Result<f64, OwnershipError> {
        let minutes = if self.overrun(now)? {
            OVERRUN_IDLE_MINUTES
        } else {
            IDLE_LAPSE_MINUTES
        };
        Ok(minutes * 60.0)
    }

    pub fn idle_deadline(&self, now: f64) -> Result<f64, OwnershipError> {
        let anchor = match self.activity()? {
            Some(a) => a,
            None => self.creation()?,
        };
        Ok(anchor + self.idle_window_seconds(now)?)
    }

    /// The earliest of the hard cap and the idle window in force at `now`.
    pub fn deadline(&self, now: f64) -> Result<f64, OwnershipError> {
        Ok(self.hard_deadline()?.min(self.idle_deadline(now)?))
    }

    pub fn active(&self, now: f64) -> Result<bool, OwnershipError> {
        Ok(self.deadline(now)? > now)
    }

    /// Why this claim is no longer active -- for the message that denies a write.
    pub fn lapse_reason(&self, now: f64) -> Result<String, OwnershipError> {
        if self.active(now)? {
            return Ok(String::new());
        }
        let idle_deadline = self.idle_deadline(now)?;
        let hard_deadline = self.hard_deadline()?;
        if idle_deadline <= now && idle_deadline <= hard_deadline {
            let activity = self.activity()?;
            let since = activity.unwrap_or(self.creation()?);
            let what = if activity.is_some() {
                "last write"
            } else {
                "creation"
            };
            let window = self.idle_window_seconds(now)? / 60.0;
            let overran = if self.overrun(now)? {
                format!(
                    "overran its expected {}Z, so it ",
                    format_hm(self.expected()?)
                )
            } else {
                String::new()
            };
            return Ok(format!(
                "idle since {what} at {}Z ({overran}lapses after {}without a write)",
                format_ymd_hm(since),
                format_g(window),
            ));
        }
        Ok(format!("expired at {}Z", format_ymd_hm(hard_deadline)))
    }
}

/// Python's `{window:g}m` -- `%g`-style: an integer prints without a decimal
/// point, otherwise the shortest round-tripping representation.
fn format_g(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}m ", value as i64)
    } else {
        let mut s = format!("{value}");
        s.push('m');
        s.push(' ');
        s
    }
}

/// The git common dir a checkout resolves to, filesystem-only -- see the
/// module docstring for why this diverges from Python's subprocess-based
/// `git_common_dir`. `None` outside any checkout.
pub fn common_dir(repo_root: &Path) -> Option<PathBuf> {
    crate::crg_gate::checkout_of(repo_root).map(|(_root, common)| common)
}

pub fn claim_store_path(repo_root: &Path) -> Option<PathBuf> {
    common_dir(repo_root).map(|d| d.join("governance").join("ownership-claims.json"))
}

pub fn claim_activity_path(repo_root: &Path) -> Option<PathBuf> {
    common_dir(repo_root).map(|d| d.join("governance").join("claim-activity.json"))
}

pub fn load_activity(repo_root: &Path) -> Result<HashMap<String, String>, OwnershipError> {
    let Some(path) = claim_activity_path(repo_root) else {
        return Ok(HashMap::new());
    };
    if !path.is_file() {
        return Ok(HashMap::new());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|exc| OwnershipError(format!("claim activity log is unreadable: {exc}")))?;
    let payload: Value = serde_json::from_str(&text)
        .map_err(|exc| OwnershipError(format!("claim activity log is unreadable: {exc}")))?;
    let obj = payload
        .as_object()
        .filter(|o| {
            let keys: std::collections::BTreeSet<&str> = o.keys().map(String::as_str).collect();
            keys == ["schema_version", "seen"].into_iter().collect()
        })
        .ok_or_else(|| {
            OwnershipError("claim activity log has an invalid top-level schema".into())
        })?;
    let schema_ok = obj.get("schema_version") == Some(&Value::from(ACTIVITY_SCHEMA_VERSION));
    let seen = obj.get("seen").and_then(Value::as_object);
    let Some(seen) = seen.filter(|_| schema_ok) else {
        return err("claim activity log schema version or entries are invalid");
    };
    let mut out = HashMap::new();
    for (key, value) in seen {
        let value = match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out.insert(key.clone(), value);
    }
    Ok(out)
}

fn write_activity(repo_root: &Path, seen: &HashMap<String, String>) -> Result<(), OwnershipError> {
    let Some(path) = claim_activity_path(repo_root) else {
        return err("claim activity log: repository has no resolvable common directory");
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|exc| OwnershipError(format!("claim activity log is unwritable: {exc}")))?;
    }
    let mut seen_sorted: Vec<(&String, &String)> = seen.iter().collect();
    seen_sorted.sort_by(|a, b| a.0.cmp(b.0));
    let seen_value = Value::Object(
        seen_sorted
            .into_iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect(),
    );
    let payload =
        serde_json::json!({"schema_version": ACTIVITY_SCHEMA_VERSION, "seen": seen_value});
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&payload).unwrap())
        .map_err(|exc| OwnershipError(format!("claim activity log is unwritable: {exc}")))?;
    std::fs::rename(&tmp, &path)
        .map_err(|exc| OwnershipError(format!("claim activity log is unwritable: {exc}")))?;
    Ok(())
}

/// `touch_claim`: record a write under `claim_id`, resetting its idle timer.
/// Debounced. Returns whether the stamp was persisted.
pub fn touch_claim(repo_root: &Path, claim_id: &str, now: f64) -> Result<bool, OwnershipError> {
    let mut seen = load_activity(repo_root)?;
    if let Some(previous) = seen.get(claim_id) {
        let last = instant::parse(previous);
        if let Some(last) = last {
            let delta = now - last;
            if (0.0..TOUCH_DEBOUNCE_SECONDS).contains(&delta) {
                return Ok(false);
            }
        }
    }
    seen.insert(claim_id.to_string(), instant::isoformat_utc(now));
    write_activity(repo_root, &seen)?;
    Ok(true)
}

pub fn paths_overlap(first: &str, second: &str) -> bool {
    first == second
        || first.starts_with(&format!("{second}/"))
        || second.starts_with(&format!("{first}/"))
}

const REQUIRED_CLAIM_FIELDS: &[&str] = &[
    "claim_id",
    "owner",
    "paths",
    "justification",
    "created_at",
    "expires_at",
];

/// Python's `sha256_json`: `sha256(json.dumps(value, sort_keys=True,
/// separators=(",",":"), ensure_ascii=False))` -- the digest runs over
/// `json_canon::canonical_json`'s bytes, so it stays independent of
/// serde_json's `preserve_order` feature (see `json_canon.rs`).
pub(crate) fn sha256_json(value: &Value) -> String {
    let digest = Sha256::digest(crate::json_canon::canonical_json(value).as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn claim_from_payload(payload: &Value) -> Result<OwnershipClaim, OwnershipError> {
    let Some(obj) = payload.as_object() else {
        return err("ownership claim has an invalid schema");
    };
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    let required: std::collections::BTreeSet<&str> =
        REQUIRED_CLAIM_FIELDS.iter().copied().collect();
    let mut allowed = required.clone();
    allowed.insert("expected_at");
    if !required.is_subset(&keys) || !keys.is_subset(&allowed) {
        return err("ownership claim has an invalid schema");
    }
    let Some(paths_raw) = obj.get("paths").and_then(Value::as_array) else {
        return err("ownership claim paths must be a non-empty array");
    };
    if paths_raw.is_empty() {
        return err("ownership claim paths must be a non-empty array");
    }
    let mut normalized = Vec::with_capacity(paths_raw.len());
    for p in paths_raw {
        normalized.push(value_as_display_string(p));
    }
    let unique: std::collections::BTreeSet<&String> = normalized.iter().collect();
    if unique.len() != normalized.len() {
        return err("ownership claim contains duplicate paths");
    }
    let claim_id = value_as_display_string(obj.get("claim_id").unwrap_or(&Value::Null));
    let owner = value_as_display_string(obj.get("owner").unwrap_or(&Value::Null))
        .trim()
        .to_string();
    let justification = value_as_display_string(obj.get("justification").unwrap_or(&Value::Null))
        .trim()
        .to_string();
    let created_at = value_as_display_string(obj.get("created_at").unwrap_or(&Value::Null));
    let expires_at = value_as_display_string(obj.get("expires_at").unwrap_or(&Value::Null));
    let expected_at = match obj.get("expected_at") {
        None | Some(Value::Null) => None,
        Some(v) => Some(value_as_display_string(v)),
    };
    if claim_id.is_empty() || owner.is_empty() || justification.is_empty() {
        return err("ownership claim identity, owner, and justification are required");
    }
    let created = instant::parse(&created_at).ok_or_else(|| {
        OwnershipError(format!("claim {claim_id} creation time lacks a timezone"))
    })?;
    let claim = OwnershipClaim {
        claim_id: claim_id.clone(),
        owner,
        paths: normalized,
        justification,
        created_at: created_at.clone(),
        expires_at: expires_at.clone(),
        expected_at: expected_at.clone(),
        last_seen: None,
    };
    let expiry = claim.expiry()?;
    if expiry <= created || expiry - created > MAX_CLAIM_HOURS * 3600.0 {
        return err(format!("claim {claim_id} has an invalid lifetime"));
    }
    if let Some(raw) = &claim.expected_at {
        let expected = parse_instant(raw, &claim_id, "expected time")?;
        if expected <= created || expected > expiry {
            return err(format!(
                "claim {claim_id} expects to finish outside its own lifetime"
            ));
        }
    }
    let mut fields = serde_json::json!({
        "owner": claim.owner,
        "paths": claim.paths,
        "justification": claim.justification,
        "created_at": claim.created_at,
        "expires_at": claim.expires_at,
    });
    if let Some(raw) = &claim.expected_at {
        fields["expected_at"] = Value::String(raw.clone());
    }
    let identity = sha256_json(&fields);
    if claim.claim_id != format!("claim-{}", &identity[..20]) {
        return err(format!("claim {claim_id} is not bound to its content"));
    }
    Ok(claim)
}

/// Python's implicit `str(value)` coercion `_claim_from_payload` relies on
/// for `payload["claim_id"]` etc.: a JSON string as itself, anything else via
/// its JSON text (matches CPython's `str(int)`/`str(float)` closely enough
/// for a claim store this codebase's own tooling writes, never hand-edits).
fn value_as_display_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

/// `load_claims`: `(claims, digest)` -- digest covers the raw claim-store
/// JSON alone (never activity stamps), matching `sha256_json(payload)` in
/// Python exactly.
pub fn load_claims(repo_root: &Path) -> Result<(Vec<OwnershipClaim>, String), OwnershipError> {
    let Some(path) = claim_store_path(repo_root) else {
        let empty = serde_json::json!({"schema_version": CLAIM_SCHEMA_VERSION, "claims": []});
        return Ok((Vec::new(), sha256_json(&empty)));
    };
    if !path.is_file() {
        let empty = serde_json::json!({"schema_version": CLAIM_SCHEMA_VERSION, "claims": []});
        return Ok((Vec::new(), sha256_json(&empty)));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|exc| OwnershipError(format!("ownership claim store is unreadable: {exc}")))?;
    let payload: Value = serde_json::from_str(&text)
        .map_err(|exc| OwnershipError(format!("ownership claim store is unreadable: {exc}")))?;
    let obj = payload.as_object().ok_or_else(|| {
        OwnershipError("ownership claim store has an invalid top-level schema".into())
    })?;
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    if keys != ["schema_version", "claims"].into_iter().collect() {
        return err("ownership claim store has an invalid top-level schema");
    }
    if obj.get("schema_version") != Some(&Value::from(CLAIM_SCHEMA_VERSION)) {
        return err("ownership claim store schema version or claims are invalid");
    }
    let Some(items) = obj.get("claims").and_then(Value::as_array) else {
        return err("ownership claim store schema version or claims are invalid");
    };
    let mut claims = Vec::with_capacity(items.len());
    for item in items {
        claims.push(claim_from_payload(item)?);
    }
    let ids: std::collections::BTreeSet<&String> = claims.iter().map(|c| &c.claim_id).collect();
    if ids.len() != claims.len() {
        return err("ownership claim store contains duplicate claim IDs");
    }
    let seen = load_activity(repo_root)?;
    for claim in &mut claims {
        if let Some(last) = seen.get(&claim.claim_id) {
            claim.last_seen = Some(last.clone());
        }
    }
    Ok((claims, sha256_json(&payload)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct ScratchRepo(PathBuf);

    impl ScratchRepo {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "forge-ownership-test-{}-{label}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(dir.join(".git")).unwrap();
            ScratchRepo(dir)
        }

        fn write_claims(&self, claims: &[Value]) {
            let dir = self.0.join(".git").join("governance");
            std::fs::create_dir_all(&dir).unwrap();
            let payload = serde_json::json!({"schema_version": 1, "claims": claims});
            std::fs::write(
                dir.join("ownership-claims.json"),
                serde_json::to_string(&payload).unwrap(),
            )
            .unwrap();
        }
    }

    impl Drop for ScratchRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn claim_json(owner: &str, paths: &[&str], created_at: &str, expires_at: &str) -> Value {
        let fields = serde_json::json!({
            "owner": owner,
            "paths": paths,
            "justification": "because",
            "created_at": created_at,
            "expires_at": expires_at,
        });
        let identity = sha256_json(&fields);
        serde_json::json!({
            "claim_id": format!("claim-{}", &identity[..20]),
            "owner": owner,
            "paths": paths,
            "justification": "because",
            "created_at": created_at,
            "expires_at": expires_at,
        })
    }

    #[test]
    fn paths_overlap_matches_exact_prefix_and_ancestor() {
        assert!(paths_overlap("src/a.py", "src/a.py"));
        assert!(paths_overlap("src/a", "src"));
        assert!(paths_overlap("src", "src/a"));
        assert!(!paths_overlap("src2", "src"));
    }

    /// The claim-id digest must not depend on map insertion order: the crate
    /// enables serde_json's `preserve_order` (the hooks installer preserves
    /// the host settings file's own key order), and before the explicit sort
    /// that feature silently changed every claim id and broke the parity
    /// corpus. The hashed bytes are Python's `json.dumps(sort_keys=True,
    /// separators=(",",":"))` form, byte for byte.
    #[test]
    fn sha256_json_sorts_keys_independently_of_map_order() {
        let sorted = serde_json::json!({"a": "1", "b": ["2", "3"], "c": {"d": "4", "e": "5"}});
        let mut shuffled = serde_json::Map::new();
        shuffled.insert("c".into(), serde_json::json!({"e": "5", "d": "4"}));
        shuffled.insert("b".into(), serde_json::json!(["2", "3"]));
        shuffled.insert("a".into(), serde_json::json!("1"));
        assert_eq!(sha256_json(&sorted), sha256_json(&Value::Object(shuffled)));
        let expected = Sha256::digest(br#"{"a":"1","b":["2","3"],"c":{"d":"4","e":"5"}}"#);
        let expected: String = expected.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(sha256_json(&sorted), expected);
    }

    #[test]
    fn load_claims_is_empty_with_a_stable_digest_when_no_store_exists() {
        let repo = ScratchRepo::new("empty");
        let (claims, digest) = load_claims(&repo.0).unwrap();
        assert!(claims.is_empty());
        assert_eq!(digest.len(), 64);
    }

    #[test]
    fn load_claims_reads_a_well_formed_store_and_verifies_the_id_hash() {
        let repo = ScratchRepo::new("wellformed");
        let now = instant::now();
        let created = instant::isoformat_utc(now - 60.0);
        let expires = instant::isoformat_utc(now + 3600.0);
        repo.write_claims(&[claim_json("llm-b0", &["src/foo.py"], &created, &expires)]);
        let (claims, _digest) = load_claims(&repo.0).unwrap();
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].owner, "llm-b0");
        assert!(claims[0].active(now).unwrap());
    }

    #[test]
    fn load_claims_rejects_a_claim_id_not_bound_to_its_content() {
        let repo = ScratchRepo::new("tampered");
        let now = instant::now();
        let created = instant::isoformat_utc(now - 60.0);
        let expires = instant::isoformat_utc(now + 3600.0);
        let mut claim = claim_json("llm-b0", &["src/foo.py"], &created, &expires);
        claim["claim_id"] = Value::String("claim-0000000000000000000".to_string());
        repo.write_claims(&[claim]);
        assert!(load_claims(&repo.0).is_err());
    }

    #[test]
    fn a_claim_past_its_hard_deadline_but_recently_active_reports_expired_not_idle() {
        let now = instant::now();
        // Hard cap (2h from creation) has already passed, but a very recent
        // touch keeps the idle window from being the binding reason.
        let claim = OwnershipClaim {
            claim_id: "claim-x".to_string(),
            owner: "llm-b0".to_string(),
            paths: vec!["src/foo.py".to_string()],
            justification: "because".to_string(),
            created_at: instant::isoformat_utc(now - 3.0 * 3600.0),
            expires_at: instant::isoformat_utc(now + 3600.0),
            expected_at: None,
            last_seen: Some(instant::isoformat_utc(now - 60.0)),
        };
        assert!(!claim.active(now).unwrap());
        assert!(claim.lapse_reason(now).unwrap().starts_with("expired at"));
    }

    #[test]
    fn a_claim_idle_past_its_lapse_window_is_inactive() {
        let repo = ScratchRepo::new("idle-lapsed");
        let now = instant::now();
        let created = instant::isoformat_utc(now - 50.0 * 60.0); // 50 min ago
        let expires = instant::isoformat_utc(now + 3600.0);
        repo.write_claims(&[claim_json("llm-b0", &["src/foo.py"], &created, &expires)]);
        let (claims, _) = load_claims(&repo.0).unwrap();
        assert!(!claims[0].active(now).unwrap());
        assert!(claims[0]
            .lapse_reason(now)
            .unwrap()
            .starts_with("idle since"));
    }

    #[test]
    fn touch_claim_persists_and_then_debounces_within_the_window() {
        let repo = ScratchRepo::new("touch");
        let now = instant::now();
        assert!(touch_claim(&repo.0, "claim-x", now).unwrap());
        assert!(!touch_claim(&repo.0, "claim-x", now + 1.0).unwrap());
        assert!(touch_claim(&repo.0, "claim-x", now + 61.0).unwrap());
    }
}
