//! Deterministic context projection shared by native hooks and Python adapters.
//! Decisions remain outside this budget. Protected fragments are never omitted.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DEFAULT_MAX_BYTES: usize = 16_000;
static SPILL_ID: AtomicU64 = AtomicU64::new(0);

fn required(fragment: &Fragment) -> bool {
    fragment.protected
        || matches!(
            fragment.category.as_str(),
            "instructions" | "policy" | "blocker" | "approval" | "error"
        )
}

fn omission_marker(count: usize, first: &str, root: Option<&Path>) -> String {
    match root {
        Some(root) => format!(
            "CONTEXT BUDGET: {count} fragment(s) omitted; ordered SHA-256 recovery manifest: {}",
            root.join(format!("omitted-{first}.json")).display()
        ),
        None => format!(
            "CONTEXT BUDGET: {count} fragment(s) omitted; request expansion by SHA-256 {first}"
        ),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Fragment {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub category: String,
    pub content: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub protected: bool,
}

impl Fragment {
    pub fn hook(name: &str, content: String, error: bool) -> Self {
        let protected = error
            || matches!(
                name,
                "session_policy"
                    | "session_preamble"
                    | "session_start"
                    | "session_handoff"
                    | "active_state"
                    | "local_ai_policy"
            );
        Self {
            id: name.to_owned(),
            version: String::new(),
            category: if error {
                "error"
            } else if protected {
                "instructions"
            } else {
                "state"
            }
            .to_owned(),
            content,
            priority: 0,
            protected,
        }
    }

    pub fn hash(&self) -> String {
        format!("{:x}", Sha256::digest(self.content.as_bytes()))
    }
}

#[derive(Debug, Serialize)]
pub struct Projection {
    pub text: String,
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub duplicates: usize,
    pub omitted: Vec<String>,
    pub protected_overflow: bool,
    pub token_budget_kind: &'static str,
}

/// Full fragments are recoverable by content hash; no model summary is trusted.
fn spill(fragment: &Fragment, root: &Path) -> std::io::Result<()> {
    publish(
        root,
        &format!("{}.json", fragment.hash()),
        &serde_json::to_vec(fragment)?,
    )
}

fn publish(root: &Path, name: &str, encoded: &[u8]) -> std::io::Result<()> {
    fs::create_dir_all(root)?;
    let path = root.join(name);
    // A complete file is published atomically; concurrent identical writers agree.
    let tmp = root.join(format!(
        ".{}.{}.{}.tmp",
        name,
        std::process::id(),
        SPILL_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    let result = (|| {
        file.write_all(encoded)?;
        file.sync_all()?;
        fs::rename(&tmp, &path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

fn recovery_marker(omitted: &[String], root: Option<&Path>) -> std::io::Result<String> {
    let reference = if let Some(root) = root {
        let encoded = serde_json::to_vec(omitted)?;
        let hash = format!("{:x}", Sha256::digest(&encoded));
        publish(root, &format!("omitted-{hash}.json"), &encoded)?;
        hash
    } else {
        omitted[0].clone()
    };
    Ok(omission_marker(omitted.len(), &reference, root))
}

fn finish_projection(
    mut kept: Vec<String>,
    mut omitted: Vec<String>,
    unique: &[Fragment],
    max_bytes: usize,
    spill_root: Option<&Path>,
    input_bytes: usize,
    duplicates: usize,
) -> Projection {
    if !omitted.is_empty() {
        match recovery_marker(&omitted, spill_root) {
            Ok(marker) => kept.push(marker),
            Err(_) => {
                // Fragments alone are insufficient: a lost manifest would hide
                // which artifacts belong to this response. Recover the prose.
                for fragment in unique.iter().filter(|f| omitted.contains(&f.hash())) {
                    kept.push(fragment.content.clone());
                }
                omitted.clear();
                kept.push(
                    "CONTEXT BUDGET: recovery manifest write failed; context retained.".into(),
                );
            }
        }
    }
    let mut text = kept.join("\n\n");
    let protected_overflow = text.len() > max_bytes;
    if protected_overflow {
        text.push_str(
            "\n\nCONTEXT BUDGET EXCEEDED: required context and recovery metadata retained in full.",
        );
    }
    Projection {
        output_bytes: text.len(),
        text,
        input_bytes,
        duplicates,
        omitted,
        protected_overflow,
        token_budget_kind: "utf8-byte-upper-bound",
    }
}

fn deduplicate(fragments: &[Fragment]) -> (Vec<Fragment>, usize) {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut duplicates = 0;
    let mut unique: Vec<Fragment> = Vec::new();
    for fragment in fragments {
        // Whitespace inside code, JSON strings and quoted paths can be semantic.
        // Only byte-identical fragments deduplicate; empty prose adds no context.
        let key = fragment.content.clone();
        if key.trim().is_empty() {
            continue;
        }
        if let Some(&index) = seen.get(&key) {
            duplicates += 1;
            unique[index].protected |= required(fragment);
            unique[index].priority = unique[index].priority.max(fragment.priority);
            if fragment.category == "instructions" {
                unique[index].category = fragment.category.clone();
            }
        } else {
            seen.insert(key, unique.len());
            let mut fragment = fragment.clone();
            fragment.protected = required(&fragment);
            unique.push(fragment);
        }
    }
    unique.sort_by_key(|fragment| {
        (
            fragment.category != "instructions",
            std::cmp::Reverse(fragment.priority),
        )
    });
    (unique, duplicates)
}

/// Stable prefix first, then descending priority with original order on ties.
/// UTF-8 bytes form a conservative token upper bound, never an exact token count.
pub fn compose(fragments: &[Fragment], max_bytes: usize, spill_root: Option<&Path>) -> Projection {
    let (unique, duplicates) = deduplicate(fragments);
    let input_bytes = fragments
        .iter()
        .map(|fragment| fragment.content.len())
        .sum();
    let mut kept = Vec::new();
    let mut omitted = Vec::new();
    let mut retained_bytes = 0;
    // Reserve room for a recoverable omission marker rather than partial prose.
    let reserve = if unique.iter().map(|f| f.content.len() + 2).sum::<usize>() > max_bytes {
        omission_marker(unique.len(), &"0".repeat(64), spill_root)
            .len()
            .saturating_add(2)
    } else {
        0
    };
    for fragment in &unique {
        let size = fragment.content.len() + usize::from(!kept.is_empty()) * 2;
        if fragment.protected || retained_bytes + size <= max_bytes.saturating_sub(reserve) {
            kept.push(fragment.content.clone());
            retained_bytes += size;
        } else if spill_root.is_none_or(|root| spill(fragment, root).is_ok()) {
            omitted.push(fragment.hash());
        } else {
            // A failed artifact write must never silently discard context.
            kept.push(fragment.content.clone());
            retained_bytes += size;
            kept.push("CONTEXT BUDGET: artifact write failed; context retained.".to_owned());
            retained_bytes += 61;
        }
    }
    finish_projection(
        kept,
        omitted,
        &unique,
        max_bytes,
        spill_root,
        input_bytes,
        duplicates,
    )
}

pub fn compose_hook(fragments: &[Fragment]) -> String {
    let max_bytes = match std::env::var("HOOK_CONTEXT_MAX_BYTES") {
        Ok(raw) => {
            match raw.parse::<usize>() {
                Ok(limit) if limit >= 512 => limit,
                _ => {
                    eprintln!("HOOK_CONTEXT_MAX_BYTES must be an integer >=512; using {DEFAULT_MAX_BYTES}");
                    DEFAULT_MAX_BYTES
                }
            }
        }
        Err(_) => DEFAULT_MAX_BYTES,
    };
    let root = std::env::var_os("CONTEXT_FRAGMENT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("LEDGER_ROOT")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| "/mnt/data/llm/ledger".into())
                .join("context_fragments")
        });
    compose(fragments, max_bytes, Some(&root)).text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_prefix_dedup_preserves_case_and_protected_information() {
        let fragments = vec![
            Fragment::hook("progress", "State\n ready".into(), false),
            Fragment::hook("progress", "State ready".into(), false),
            Fragment::hook("session_policy", "MISSION".into(), false),
            Fragment::hook("progress", "State\n ready".into(), false),
            Fragment::hook("progress", "state ready".into(), false),
        ];
        let projected = compose(&fragments, 1000, None);
        assert_eq!(
            projected.text,
            "MISSION\n\nState\n ready\n\nState ready\n\nstate ready"
        );
        assert_eq!(projected.duplicates, 1);
    }

    #[test]
    fn bounded_projection_omits_whole_fragments_and_reports_overflow() {
        let optional = Fragment::hook("progress", "界".repeat(500), false);
        let projected = compose(&[optional], 512, None);
        assert!(projected.output_bytes <= 512);
        assert_eq!(projected.omitted.len(), 1);
        let protected = Fragment::hook("session_policy", "policy".repeat(100), false);
        let projected = compose(&[protected], 512, None);
        assert!(projected.protected_overflow);
        assert!(projected.text.contains(&"policy".repeat(100)));
    }

    #[test]
    fn duplicate_optional_cannot_displace_protected_copy() {
        let optional = Fragment::hook("progress", "mandatory".repeat(100), false);
        let protected = Fragment::hook("session_policy", optional.content.clone(), false);
        let projected = compose(&[optional, protected], 512, None);
        assert!(projected.omitted.is_empty());
        assert!(projected.protected_overflow);
    }

    #[test]
    fn instruction_category_is_required_without_explicit_protection() {
        let mut fragment = Fragment::hook("custom", "required instructions".repeat(100), false);
        fragment.category = "instructions".into();
        let projected = compose(std::slice::from_ref(&fragment), 512, None);
        assert!(projected.omitted.is_empty());
        assert!(projected.protected_overflow);
        assert!(projected.text.contains(&fragment.content));
    }

    #[test]
    fn omitted_content_is_atomically_recoverable() {
        let root = std::env::temp_dir().join(format!(
            "context-fragment-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let fragment = Fragment::hook("progress", "source details".repeat(500), false);
        let projected = compose(std::slice::from_ref(&fragment), 512, Some(&root));
        let recovered: Fragment = serde_json::from_slice(
            &std::fs::read(root.join(format!("{}.json", fragment.hash()))).unwrap(),
        )
        .unwrap();
        assert_eq!(recovered.content, fragment.content);
        assert_eq!(projected.omitted, vec![fragment.hash()]);
        let manifest = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().starts_with("omitted-"))
            .unwrap();
        let hashes: Vec<String> =
            serde_json::from_slice(&std::fs::read(manifest.path()).unwrap()).unwrap();
        assert_eq!(hashes, projected.omitted);
        assert!(projected.text.contains(manifest.path().to_str().unwrap()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn multiple_omissions_have_an_ordered_recovery_manifest() {
        let root = std::env::temp_dir().join(format!(
            "context-many-{}-{}",
            std::process::id(),
            SPILL_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let fragments = [
            Fragment::hook("one", "first".repeat(200), false),
            Fragment::hook("two", "second".repeat(200), false),
        ];
        let projected = compose(&fragments, 512, Some(&root));
        assert_eq!(projected.omitted.len(), 2);
        assert!(projected.output_bytes <= 512);
        let manifest = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().to_string_lossy().starts_with("omitted-"))
            .unwrap();
        let hashes: Vec<String> =
            serde_json::from_slice(&std::fs::read(manifest.path()).unwrap()).unwrap();
        assert_eq!(
            hashes,
            fragments.iter().map(Fragment::hash).collect::<Vec<_>>()
        );
        for hash in hashes {
            assert!(root.join(format!("{hash}.json")).is_file());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
