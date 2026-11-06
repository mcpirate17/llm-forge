//! Stable names for automatically generated mutants.
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn mutant_base(path: &str, operator: &str, original: &str, mutated: &str) -> String {
    let joined = [path, operator, original, mutated].join("\0");
    let hash = format!("{:x}", Sha256::digest(joined.as_bytes()));
    format!("{operator}-{}", &hash[..12])
}

/// Name a run's mutants in input order, disambiguating identical rewrites.
pub fn identify(entries: &[[&str; 4]]) -> Vec<String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    entries
        .iter()
        .map(|[path, operator, original, mutated]| {
            let base = mutant_base(path, operator, original, mutated);
            let occurrence = seen.entry(base.clone()).or_insert(0);
            let name = format!("{base}-{occurrence}");
            *occurrence += 1;
            name
        })
        .collect()
}
