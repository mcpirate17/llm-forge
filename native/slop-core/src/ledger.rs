//! Turning a sweep's findings into a backlog.
//!
//! A sweep emits one finding per ablation. That is the right unit for the probe and
//! the wrong unit for a person: the 2026-08-31 sweep of 231 modules produced 2,776
//! findings covering 931 distinct functions, so roughly two thirds of the report was
//! the same function said again with a different rule attached.
//!
//! So this module does what the raw findings cannot:
//!
//! 1. **Collapse** findings to one item per (module, function, verdict), ranked by
//!    verdict and then by how many findings reached it.
//! 2. **Diff** against the previous sweep, so a report says what is *new* rather than
//!    restating a backlog every run.
//!
//! Item identity deliberately excludes the line number. Lines move when anything above
//! them is edited; an item whose id changed because a docstring grew would read as one
//! item fixed and another appearing, and a burndown built on that is noise. Renaming a
//! function does mint a new id, which is honest: it is a different function.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// One finding as the sweep emits it.
#[derive(Clone, Debug, Deserialize)]
pub struct Finding {
    pub module: String,
    pub qualname: String,
    pub verdict: String,
    pub rule: String,
    #[serde(rename = "lineno")]
    pub line: usize,
    pub description: String,
}

/// One unit of work: a function, a verdict, and every rule that reached it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Item {
    pub id: String,
    pub module: String,
    pub qualname: String,
    pub verdict: String,
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

/// A short, stable content hash. FNV-1a over the identity fields, hex.
///
/// Not cryptographic and does not need to be: it identifies a row in a backlog the
/// repository owns, and a collision costs one merged report line.
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

/// A construct no test drives, then a function no test called, then the rest.
fn rank(verdict: &str) -> u8 {
    match verdict {
        "REACHABLE_BUT_UNTESTED" => 0,
        "NOT_EXERCISED" => 1,
        _ => 2,
    }
}

/// Collapse findings into ranked items.
///
/// Ranking is: by verdict, then the most findings, then module and qualname so the
/// order is total and a report diffs cleanly against itself.
pub fn aggregate(findings: &[Finding]) -> Vec<Item> {
    let mut by_key: BTreeMap<(String, String, String), Item> = BTreeMap::new();
    for f in findings {
        let key = (f.module.clone(), f.qualname.clone(), f.verdict.clone());
        let entry = by_key.entry(key).or_insert_with(|| Item {
            id: stable_id(&f.module, &f.qualname, &f.verdict),
            module: f.module.clone(),
            qualname: f.qualname.clone(),
            verdict: f.verdict.clone(),
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
        rank(&a.verdict)
            .cmp(&rank(&b.verdict))
            .then(b.findings.cmp(&a.findings))
            .then(a.module.cmp(&b.module))
            .then(a.qualname.cmp(&b.qualname))
    });
    items
}

/// Split this sweep's items against the ids the previous sweep recorded.
///
/// `fixed` is every previously-known id absent now. That is only meaningful when the
/// two sweeps measured the same functions -- a narrower sweep would otherwise report
/// everything it skipped as fixed -- so the caller passes only the ids it measured.
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

/// Items per verdict, for the report's summary table.
pub fn tally<'i>(items: impl IntoIterator<Item = &'i Item>) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for i in items {
        *out.entry(i.verdict.clone()).or_insert(0) += 1;
    }
    out
}
