//! Turning a sweep's findings into a backlog.
//!
//! A sweep emits one finding per ablation. That is the right unit for the probe and
//! the wrong unit for a human: the 2026-08-31 sweep of 231 modules produced 2,776
//! findings covering 931 distinct functions, so roughly two thirds of the report was
//! the same function said again with a different rule attached.
//!
//! Worse, 86% of it was `research/tools/` -- one-off experiment scripts, where "no
//! test names this function" is the expected state and not a defect. Reported flat,
//! the 61 untested functions in shipped code are buried under 408 that nobody should
//! act on.
//!
//! So this module does three things the raw findings cannot:
//!
//! 1. **Collapse** findings to one item per (module, function, verdict).
//! 2. **Tier** each item by whether its module ships, using prefixes the caller owns
//!    -- the classification is repository policy, not something a parser should guess.
//! 3. **Diff** against the previous sweep, so a report says what is *new* rather than
//!    restating a backlog every run.
//!
//! Item identity deliberately excludes the line number. Lines move when anything above
//! them is edited; an item whose id changed because a docstring grew would read as one
//! item fixed and another appearing, and a burndown built on that is noise. Renaming a
//! function does mint a new id, which is honest: it is a different function.

use std::collections::{BTreeMap, BTreeSet};

/// One finding as the sweep emits it.
#[derive(Clone, Debug)]
pub struct Finding {
    pub module: String,
    pub qualname: String,
    pub verdict: String,
    pub rule: String,
    pub line: usize,
    pub description: String,
}

/// One unit of work: a function, a verdict, and every rule that reached it.
#[derive(Clone, Debug)]
pub struct Item {
    pub id: String,
    pub module: String,
    pub qualname: String,
    pub verdict: String,
    pub tier: String,
    pub rules: Vec<String>,
    pub findings: usize,
    pub line: usize,
    pub description: String,
}

/// What changed since the previous sweep.
#[derive(Clone, Debug, Default)]
pub struct Diff {
    pub new: Vec<Item>,
    pub carried: Vec<Item>,
    pub fixed: Vec<String>,
}

const SHIPPED: &str = "shipped";
const EXPLORATORY: &str = "exploratory";

/// A short, stable content hash. FNV-1a over the identity fields, hex.
///
/// Not cryptographic and does not need to be: it identifies a row in a backlog the
/// repository owns, and a collision costs one merged report line, not a security
/// property. Rolled by hand to keep the crate's dependency surface at tree-sitter and
/// pyo3.
fn stable_id(module: &str, qualname: &str, verdict: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in [module, "\u{1}", qualname, "\u{1}", verdict] {
        for b in part.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// `shipped` if any prefix matches, else `exploratory`.
///
/// Prefix matching on a repository-relative path, not a glob: the caller passes
/// `conductor/`, `aria_core/` and so on. A trailing slash is not required, but without
/// one `research/tool` would also claim `research/tools/` -- so the caller's list is
/// taken literally and it is the caller's job to be precise.
fn tier_for(module: &str, shipped_prefixes: &[String]) -> &'static str {
    if shipped_prefixes
        .iter()
        .any(|p| module.starts_with(p.as_str()))
    {
        SHIPPED
    } else {
        EXPLORATORY
    }
}

/// Collapse findings into ranked items.
///
/// Ranking is: shipped before exploratory, then the most findings, then module and
/// qualname so the order is total and a report diffs cleanly against itself.
pub fn aggregate(findings: &[Finding], shipped_prefixes: &[String]) -> Vec<Item> {
    let mut by_key: BTreeMap<(String, String, String), Item> = BTreeMap::new();
    for f in findings {
        let key = (f.module.clone(), f.qualname.clone(), f.verdict.clone());
        let entry = by_key.entry(key).or_insert_with(|| Item {
            id: stable_id(&f.module, &f.qualname, &f.verdict),
            module: f.module.clone(),
            qualname: f.qualname.clone(),
            verdict: f.verdict.clone(),
            tier: tier_for(&f.module, shipped_prefixes).to_string(),
            rules: Vec::new(),
            findings: 0,
            line: f.line,
            description: f.description.clone(),
        });
        entry.findings += 1;
        if !entry.rules.contains(&f.rule) {
            entry.rules.push(f.rule.clone());
        }
        // The earliest line in the function is the one to open the editor at.
        if f.line < entry.line {
            entry.line = f.line;
        }
    }
    let mut items: Vec<Item> = by_key.into_values().collect();
    for item in &mut items {
        item.rules.sort();
    }
    items.sort_by(|a, b| {
        let rank = |t: &str| if t == SHIPPED { 0 } else { 1 };
        rank(&a.tier)
            .cmp(&rank(&b.tier))
            .then(b.findings.cmp(&a.findings))
            .then(a.module.cmp(&b.module))
            .then(a.qualname.cmp(&b.qualname))
    });
    items
}

/// Split this sweep's items against the ids the previous sweep recorded.
///
/// `fixed` is every previously-known id absent now. That is only meaningful when the
/// two sweeps covered the same modules -- a narrower sweep would otherwise report
/// everything it skipped as fixed. The caller is responsible for that; see the
/// `scope` recorded alongside the ledger.
pub fn diff(previous_ids: &[String], now: &[Item]) -> Diff {
    let known: BTreeSet<&str> = previous_ids.iter().map(|s| s.as_str()).collect();
    let present: BTreeSet<&str> = now.iter().map(|i| i.id.as_str()).collect();
    let mut out = Diff::default();
    for item in now {
        if known.contains(item.id.as_str()) {
            out.carried.push(item.clone());
        } else {
            out.new.push(item.clone());
        }
    }
    for id in previous_ids {
        if !present.contains(id.as_str()) {
            out.fixed.push(id.clone());
        }
    }
    out.fixed.sort();
    out.fixed.dedup();
    out
}

/// Counts per (tier, verdict), for the report's summary table.
pub fn tally(items: &[Item]) -> BTreeMap<(String, String), usize> {
    let mut out = BTreeMap::new();
    for i in items {
        *out.entry((i.tier.clone(), i.verdict.clone())).or_insert(0) += 1;
    }
    out
}
