//! Shared `session_[A-Za-z0-9]+` id extraction (design step 4 join key,
//! `docs/design/cost_ledger.md` section 6 step 4): `reader.rs` calls this
//! once per text/tool_result block while streaming a transcript;
//! `landed.rs` calls it once per commit body when scanning `git log`. One
//! regex, one definition, so the two sides of the join can never drift on
//! what counts as a session id.
//!
//! A harness Claude-Session URL's trailing `/` (some trailers have one,
//! some do not) never reaches a match: `/` is outside `[A-Za-z0-9]`, so
//! the regex already stops at the id boundary without extra handling.
//!
//! False-positive guard (found empirically against this repo's own commit
//! #37, whose PR body prose names the `session_id`/`session_rollup` schema
//! fields): a bare regex match on `session_[A-Za-z0-9]+` also matches those
//! english words, since nothing about the pattern requires the suffix to
//! look like an id rather than an identifier. A real Claude session id is a
//! ~24-char base62 string (`session_01PoLjRxqVQGqy41fMDG26vX`) and, at that
//! length and alphabet, is all but certain to contain a digit; no schema
//! field name ever does. So `find_session_ids` keeps a match only when its
//! suffix contains at least one ASCII digit -- cheap, and it costs nothing
//! real: an actual session id missing a digit entirely is not a case this
//! codebase has ever produced or observed.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

fn pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"session_[A-Za-z0-9]+").expect("valid session id regex"))
}

/// Finds every session id in `text` and inserts it into `ids`. Only the
/// matched id substring is ever kept -- never the surrounding text. Skips a
/// match whose suffix (after `session_`) has no digit at all -- see the
/// module doc comment's false-positive note.
pub fn find_session_ids(text: &str, ids: &mut BTreeSet<String>) {
    for m in pattern().find_iter(text) {
        let matched = m.as_str();
        let suffix = &matched["session_".len()..];
        if suffix.bytes().any(|b| b.is_ascii_digit()) {
            ids.insert(matched.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_session_id_is_kept() {
        let mut ids = BTreeSet::new();
        find_session_ids(
            "Claude-Session: https://claude.ai/code/session_01PoLjRxqVQGqy41fMDG26vX\n",
            &mut ids,
        );
        assert_eq!(
            ids.into_iter().collect::<Vec<_>>(),
            vec!["session_01PoLjRxqVQGqy41fMDG26vX".to_string()]
        );
    }

    #[test]
    fn schema_field_names_are_not_mistaken_for_ids() {
        let mut ids = BTreeSet::new();
        find_session_ids(
            "session_id and session_rollup and session_start are schema names, not ids",
            &mut ids,
        );
        assert!(
            ids.is_empty(),
            "expected no false-positive matches, got {ids:?}"
        );
    }

    #[test]
    fn a_real_id_survives_alongside_schema_words_in_the_same_text() {
        let mut ids = BTreeSet::new();
        find_session_ids(
            "session_rollup joins on session_01PoLjRxqVQGqy41fMDG26vX, not session_id",
            &mut ids,
        );
        assert_eq!(
            ids.into_iter().collect::<Vec<_>>(),
            vec!["session_01PoLjRxqVQGqy41fMDG26vX".to_string()]
        );
    }
}
