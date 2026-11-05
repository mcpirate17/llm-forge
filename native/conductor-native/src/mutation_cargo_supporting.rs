//! Bounded ordinary Cargo discovery-input binding, not a compiler closure.
use crate::mutation_cargo_inventory::{self, Entry};
use crate::mutation_cargo_supporting_binding::SupportingInputs;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Hash a complete independently enumerated snapshot's bounded relevant bytes.
/// The caller must bind directory names and verify them again after use.
pub fn build(
    entries: &BTreeMap<String, Entry>,
    required: &BTreeMap<String, String>,
) -> Result<SupportingInputs, String> {
    let files = mutation_cargo_inventory::observe(entries)?;
    for required in ["Cargo.toml", "Cargo.lock"] {
        if !files.contains_key(required) {
            return Err(format!(
                "required Cargo supporting input is missing: {required}"
            ));
        }
    }
    for path in required.keys() {
        if !files.contains_key(path) {
            return Err(format!(
                "required source or test missing from supporting inventory: {path}"
            ));
        }
    }
    for path in entries.keys() {
        if path.split('/').any(|name| {
            matches!(
                name,
                ".cargo-mutants.toml" | "rust-toolchain" | "rust-toolchain.toml"
            )
        }) {
            return Err(format!("unsupported Cargo input: {path}"));
        }
    }
    let mut hash = Sha256::new();
    frame(&mut hash, b"forge.ordinary-cargo.supporting-inputs.v1");
    for (path, sha) in &files {
        frame(&mut hash, path.as_bytes());
        frame(&mut hash, sha.as_bytes());
    }
    Ok(SupportingInputs {
        schema_version: 1,
        algorithm: "sha256-framed-path-content-v1".into(),
        scope: "cargo-root-rust-discovery-v1".into(),
        digest: format!("{:x}", hash.finalize()),
        files: files.len(),
    })
}
fn frame(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> BTreeMap<String, Entry> {
        BTreeMap::from([
            (
                "Cargo.toml".into(),
                Entry::Regular(Some(b"[package]".to_vec())),
            ),
            (
                "Cargo.lock".into(),
                Entry::Regular(Some(b"version=4".to_vec())),
            ),
            ("src".into(), Entry::Directory),
            (
                "src/lib.rs".into(),
                Entry::Regular(Some(b"pub mod a;".to_vec())),
            ),
            (
                "src/a.rs".into(),
                Entry::Regular(Some(b"pub fn a() {}".to_vec())),
            ),
        ])
    }
    #[test]
    fn changed_or_added_relevant_inputs_change_identity() {
        let baseline = build(&fixture(), &BTreeMap::new()).unwrap();
        for path in ["Cargo.toml", "Cargo.lock", "src/lib.rs"] {
            let mut changed = fixture();
            changed.insert(path.into(), Entry::Regular(Some(b"changed".to_vec())));
            assert_ne!(
                baseline,
                build(&changed, &BTreeMap::new()).unwrap(),
                "{path}"
            );
        }
        let mut added = fixture();
        added.insert("src/new.rs".into(), Entry::Regular(Some(b"new".to_vec())));
        assert_ne!(baseline, build(&added, &BTreeMap::new()).unwrap());
    }
    #[test]
    fn selected_sources_must_be_in_the_complete_inventory() {
        let entries = fixture();
        let absent = BTreeMap::from([("target/hidden.rs".into(), "a".repeat(64))]);
        assert!(build(&entries, &absent).is_err());
    }
    #[test]
    fn non_rust_runtime_data_is_outside_discovery_scope() {
        let entries = fixture();
        let baseline = build(&entries, &BTreeMap::new()).unwrap();
        let mut with_data = fixture();
        with_data.insert("fixture.txt".into(), Entry::Regular(None));
        assert_eq!(baseline, build(&with_data, &BTreeMap::new()).unwrap());
        assert_eq!(baseline.scope, "cargo-root-rust-discovery-v1");
    }
    #[test]
    fn missing_special_or_unsupported_config_refuses() {
        let mut entries = fixture();
        entries.remove("Cargo.lock");
        assert!(build(&entries, &BTreeMap::new()).is_err());
        let mut entries = fixture();
        entries.insert("src/lib.rs".into(), Entry::SymlinkOrSpecial);
        assert!(build(&entries, &BTreeMap::new()).is_err());
        let mut entries = fixture();
        entries.insert("rust-toolchain".into(), Entry::Regular(None));
        assert!(build(&entries, &BTreeMap::new()).is_err());
    }
}
