//! Commit-subject digests: the `commit_subject` join key shared by both
//! sides of the join (`docs/ledger.md`). The session that typed a landed
//! commit has the commit's subject in its own transcript -- the
//! `git commit -m` / `gh pr create --title` Bash command -- and the landed
//! side has it in `git log`'s `%s`. Matching a hash of that subject is much
//! stronger evidence than a time overlap and needs no new trailer.
//!
//! Both sides normalize identically here and keep only a sha256 digest
//! truncated to 16 hex chars: enough bits to key a join over a few thousand
//! subjects without accidental collisions, and never the subject text
//! itself -- the ledger stores shapes, not content.

use std::sync::OnceLock;

use regex::Regex;
use sha2::{Digest, Sha256};

/// Subjects shorter than this, after normalization, are refused a digest on
/// BOTH sides: `fix` or `wip` matches far too many unrelated commands for
/// the join to mean anything, so `subject_digest` returns the empty string
/// and the join treats an empty digest as "no key" (`docs/ledger.md`
/// "Known limits").
pub const MIN_SUBJECT_CHARS: usize = 12;

fn pr_suffix_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"[ \t]*\(#\d+\)$").expect("valid trailing pr-number regex"))
}

/// Normalize a raw subject the way both sides of the join do: trim, collapse
/// internal whitespace runs to one space, and strip a trailing `(#N)` -- the
/// squash-merge suffix GitHub appends to the PR title, which the session
/// that ran `gh pr create --title` never typed (and which the landed side
/// always carries).
pub fn normalize_subject(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    pr_suffix_pattern().replace(&collapsed, "").into_owned()
}

/// sha256 of the normalized subject, truncated to 16 hex chars -- or the
/// empty string when the normalized subject is shorter than
/// `MIN_SUBJECT_CHARS` (refused, never matched; see the module doc).
pub fn subject_digest(raw: &str) -> String {
    let normalized = normalize_subject(raw);
    if normalized.chars().count() < MIN_SUBJECT_CHARS {
        return String::new();
    }
    let digest = Sha256::digest(normalized.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    hex[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_trims_collapses_and_strips_the_pr_suffix() {
        assert_eq!(normalize_subject("  fix:   a\tthing\n"), "fix: a thing");
        assert_eq!(normalize_subject("feat: x (#47)  "), "feat: x");
        assert_eq!(
            normalize_subject("feat: x (#47)"),
            normalize_subject("feat: x")
        );
        // A number in parentheses that is not a trailing `(#N)` stays.
        assert_eq!(normalize_subject("fix (2): thing"), "fix (2): thing");
    }

    #[test]
    fn short_subjects_are_refused_not_hashed() {
        assert_eq!(subject_digest("fix: bug"), "");
        assert_eq!(subject_digest(""), "");
        assert_eq!(subject_digest("   (#12)   "), "");
        // Exactly at the floor hashes; below it does not.
        assert!(!subject_digest("abcdefghijkl").is_empty());
        assert_eq!(subject_digest("abcdefghijk"), "");
    }

    #[test]
    fn digests_are_sixteen_hex_chars_and_stable() {
        let a = subject_digest(
            "feat(forge): commit_subject join credits the session that typed it (#48)",
        );
        let b =
            subject_digest("feat(forge): commit_subject join credits the session that typed it");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(
            a,
            subject_digest("feat(forge): some other subject entirely")
        );
    }
}
