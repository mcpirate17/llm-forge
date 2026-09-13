//! Native port of `conductor.candidate_review.identity`: one identity for
//! claiming a path and for writing it. See the Python module's docstring for
//! the full "codex claimed as X but denied itself" motivation -- this port
//! preserves every rule verbatim, including the vendor-is-not-a-lane
//! distinction and the derive-then-fall-back-to-vendor order in
//! `resolve_owner`.
//!
//! Derivation is filesystem-only (no `git` subprocess), matching the Python
//! module's own stated design ("a `git rev-parse` per call would put a
//! subprocess in the hook's latency path").

use std::collections::HashMap;
use std::path::Path;

/// `VENDOR_MARKERS`: env var -> vendor name, checked in this order.
const VENDOR_MARKERS: &[(&str, &str)] = &[
    ("QWEN_PROJECT_DIR", "qwen"),
    ("GROK_PROJECT_DIR", "grok"),
    ("CLAUDE_PROJECT_DIR", "claude"),
    ("CODEX_HOME", "codex"),
];

const OWNER_MAX: usize = 64;

/// The running lane cannot be named, so nothing may be claimed or written.
#[derive(Debug)]
pub struct OwnerIdentityError(pub String);

impl std::fmt::Display for OwnerIdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for OwnerIdentityError {}

fn is_vendor(owner: &str) -> bool {
    let folded = owner.trim().to_lowercase();
    VENDOR_MARKERS.iter().any(|(_, vendor)| *vendor == folded)
}

/// `vendor_of`: the vendor whose launcher this process is under, or `""`.
fn vendor_of(env: &HashMap<String, String>) -> String {
    for (marker, vendor) in VENDOR_MARKERS {
        if env
            .get(*marker)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
        {
            return vendor.to_string();
        }
    }
    String::new()
}

/// `vendor_for`: the vendor a lane belongs to -- its launcher's, else its own
/// name prefix.
pub fn vendor_for(owner: &str, env: &HashMap<String, String>) -> String {
    let marker = vendor_of(env);
    if !marker.is_empty() {
        return marker;
    }
    let normalized = normalize(owner);
    let head = normalized.split('-').next().unwrap_or("");
    if VENDOR_MARKERS.iter().any(|(_, vendor)| *vendor == head) {
        head.to_string()
    } else {
        String::new()
    }
}

/// `normalize`: fold an arbitrary lane string into the owner charset, or `""`.
/// Mirrors Python's `re.sub(r"[^a-z0-9._-]+", "-", owner.strip().casefold())
/// .strip("-.")[:64]`.
pub fn normalize(owner: &str) -> String {
    let folded = owner.trim().to_lowercase();
    let mut out = String::with_capacity(folded.len());
    let mut in_run = false;
    for ch in folded.chars() {
        let safe = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-');
        if safe {
            out.push(ch);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    let trimmed = out.trim_matches(|c| c == '-' || c == '.');
    let truncated: String = trimmed.chars().take(OWNER_MAX).collect();
    truncated
}

/// `lane_of`: the lane name of a checkout -- its worktree name, else its
/// branch. Filesystem-only: reads `.git` (file for a linked worktree,
/// directory for the main checkout) and, for the latter, `.git/HEAD`.
pub fn lane_of(repo_root: &Path) -> String {
    let marker = repo_root.join(".git");
    if marker.is_file() {
        let name = repo_root.file_name().and_then(|n| n.to_str()).unwrap_or("");
        return normalize(name);
    }
    if !marker.is_dir() {
        return String::new();
    }
    let Ok(head) = std::fs::read_to_string(marker.join("HEAD")) else {
        return String::new();
    };
    let head = head.trim();
    let Some(branch) = head.strip_prefix("ref: refs/heads/") else {
        return String::new(); // detached: no branch, so no lane
    };
    normalize(branch)
}

/// `resolve_owner`: the identity this lane claims and writes under. `Err`
/// when no identity can be derived (mirrors `OwnerIdentityError`).
pub fn resolve_owner(
    repo_root: Option<&Path>,
    env: &HashMap<String, String>,
) -> Result<String, OwnerIdentityError> {
    let declared = normalize(
        env.get("GOVERNANCE_OWNER")
            .map(String::as_str)
            .unwrap_or(""),
    );
    if !declared.is_empty() && !is_vendor(&declared) {
        return Ok(declared);
    }
    if let Some(root) = repo_root {
        let lane = lane_of(root);
        if !lane.is_empty() && !is_vendor(&lane) {
            return Ok(lane);
        }
    }
    let vendor = {
        let v = vendor_of(env);
        if v.is_empty() {
            declared
        } else {
            v
        }
    };
    if !vendor.is_empty() {
        return Ok(vendor);
    }
    Err(OwnerIdentityError(
        "no governance identity: this process is under no known agent launcher and \
         its checkout has no lane name — export GOVERNANCE_OWNER=<lane> \
         (a worktree or branch name, not a vendor)"
            .to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    struct ScratchRepo(std::path::PathBuf);

    impl ScratchRepo {
        fn new(label: &str) -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "forge-identity-test-{}-{label}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            ScratchRepo(dir)
        }
    }

    impl Drop for ScratchRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn normalize_folds_unsafe_characters_and_casefolds() {
        assert_eq!(normalize("  Codex Audit/Scan!! "), "codex-audit-scan");
        assert_eq!(normalize("--.trim.me.--"), "trim.me");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn normalize_truncates_to_64_bytes() {
        let long = "a".repeat(100);
        assert_eq!(normalize(&long).len(), 64);
    }

    #[test]
    fn is_vendor_matches_known_vendors_case_insensitively() {
        assert!(is_vendor("Codex"));
        assert!(is_vendor(" claude "));
        assert!(!is_vendor("codex-audit"));
    }

    #[test]
    fn vendor_of_checks_markers_in_declared_order() {
        let e = env(&[("CLAUDE_PROJECT_DIR", "/x"), ("CODEX_HOME", "/y")]);
        assert_eq!(vendor_of(&e), "claude");
        assert_eq!(vendor_of(&HashMap::new()), "");
    }

    #[test]
    fn vendor_for_prefers_the_launcher_marker_over_the_name_prefix() {
        let e = env(&[("CODEX_HOME", "/y")]);
        assert_eq!(vendor_for("claude-something", &e), "codex");
    }

    #[test]
    fn vendor_for_falls_back_to_a_recognized_name_prefix() {
        assert_eq!(vendor_for("codex-audit-scan", &HashMap::new()), "codex");
        assert_eq!(vendor_for("llm-b0-lane", &HashMap::new()), "");
    }

    #[test]
    fn lane_of_reads_the_worktree_name_from_a_linked_worktree_git_file() {
        let repo = ScratchRepo::new("worktree");
        let named = repo.0.join("llm-b0-my-lane");
        std::fs::create_dir_all(&named).unwrap();
        std::fs::write(named.join(".git"), "gitdir: /elsewhere/.git\n").unwrap();
        assert_eq!(lane_of(&named), "llm-b0-my-lane");
    }

    #[test]
    fn lane_of_reads_the_branch_from_a_main_checkout() {
        let repo = ScratchRepo::new("main");
        std::fs::create_dir_all(repo.0.join(".git")).unwrap();
        std::fs::write(
            repo.0.join(".git/HEAD"),
            "ref: refs/heads/forge/my-branch\n",
        )
        .unwrap();
        assert_eq!(lane_of(&repo.0), "forge-my-branch");
    }

    #[test]
    fn lane_of_is_empty_for_a_detached_head() {
        let repo = ScratchRepo::new("detached");
        std::fs::create_dir_all(repo.0.join(".git")).unwrap();
        std::fs::write(repo.0.join(".git/HEAD"), "abcdef0123456789\n").unwrap();
        assert_eq!(lane_of(&repo.0), "");
    }

    #[test]
    fn lane_of_is_empty_outside_any_checkout() {
        let repo = ScratchRepo::new("none");
        assert_eq!(lane_of(&repo.0), "");
    }

    #[test]
    fn resolve_owner_prefers_a_declared_non_vendor_governance_owner() {
        let e = env(&[("GOVERNANCE_OWNER", "llm-b0")]);
        assert_eq!(resolve_owner(None, &e).unwrap(), "llm-b0");
    }

    #[test]
    fn resolve_owner_ignores_a_declared_vendor_literal() {
        let repo = ScratchRepo::new("declared-vendor");
        std::fs::create_dir_all(repo.0.join(".git")).unwrap();
        std::fs::write(repo.0.join(".git/HEAD"), "ref: refs/heads/my-lane\n").unwrap();
        let e = env(&[("GOVERNANCE_OWNER", "codex")]);
        assert_eq!(resolve_owner(Some(&repo.0), &e).unwrap(), "my-lane");
    }

    #[test]
    fn resolve_owner_falls_back_to_the_checkout_lane() {
        let repo = ScratchRepo::new("lane-fallback");
        std::fs::create_dir_all(repo.0.join(".git")).unwrap();
        std::fs::write(repo.0.join(".git/HEAD"), "ref: refs/heads/my-lane\n").unwrap();
        assert_eq!(
            resolve_owner(Some(&repo.0), &HashMap::new()).unwrap(),
            "my-lane"
        );
    }

    #[test]
    fn resolve_owner_falls_back_to_the_vendor_marker_when_the_lane_is_unnameable() {
        let repo = ScratchRepo::new("vendor-fallback");
        // Detached HEAD: no lane.
        std::fs::create_dir_all(repo.0.join(".git")).unwrap();
        std::fs::write(repo.0.join(".git/HEAD"), "abcdef\n").unwrap();
        let e = env(&[("CLAUDE_PROJECT_DIR", "/x")]);
        assert_eq!(resolve_owner(Some(&repo.0), &e).unwrap(), "claude");
    }

    #[test]
    fn resolve_owner_errors_when_nothing_can_name_the_lane() {
        let repo = ScratchRepo::new("unnameable");
        assert!(resolve_owner(Some(&repo.0), &HashMap::new()).is_err());
        assert!(resolve_owner(None, &HashMap::new()).is_err());
    }
}
