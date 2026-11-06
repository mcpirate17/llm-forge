//! Coordinator-supplied snapshot output exclusions and their protected inputs.
use anyhow::{ensure, Result};
use conductor_native::mutation_snapshot_inputs::validate_path;
use std::collections::BTreeSet;

/// A single automatically generated replacement, bound before attribution mutates it.
/// This is never deserialized from a receipt and never changes the selected path set.
pub(crate) struct Replacement {
    path: String,
    mode: u32,
    original: Vec<u8>,
    expected_mutant_sha256: String,
}
impl Replacement {
    pub(super) fn from_row(
        path: String,
        mode: u32,
        original: Vec<u8>,
        row: &serde_json::Value,
    ) -> Result<Self> {
        use anyhow::Context;
        validate_path(&path).map_err(anyhow::Error::msg)?;
        ensure!(
            matches!(mode, 0o100644 | 0o100755),
            "mutant target must be regular"
        );
        let offset = usize::try_from(
            row["byte_offset"]
                .as_u64()
                .context("missing mutant offset")?,
        )?;
        let length = usize::try_from(
            row["byte_length"]
                .as_u64()
                .context("missing mutant length")?,
        )?;
        let end = offset
            .checked_add(length)
            .context("mutant offset overflow")?;
        let expected = row["original_text"]
            .as_str()
            .context("missing original mutant text")?
            .as_bytes();
        let changed = row["mutated_text"]
            .as_str()
            .context("missing mutated text")?
            .as_bytes();
        ensure!(
            original.get(offset..end) == Some(expected),
            "engine mutation does not match pinned original bytes"
        );
        let size = original
            .len()
            .checked_sub(length)
            .and_then(|n| n.checked_add(changed.len()))
            .context("mutant size overflow")?;
        ensure!(size <= 32 * 1024 * 1024, "mutant exceeds 32 MiB");
        let mut mutated = Vec::with_capacity(size);
        mutated.extend_from_slice(&original[..offset]);
        mutated.extend_from_slice(changed);
        mutated.extend_from_slice(&original[end..]);
        ensure!(
            mutated != original,
            "engine mutation does not change source"
        );
        Ok(Self {
            path,
            mode,
            original,
            expected_mutant_sha256: crate::mutation_campaign::sha256_hex(&mutated),
        })
    }
    pub(super) fn path(&self) -> &str {
        &self.path
    }
    pub(super) fn original_for<'a>(&'a self, mode: u32, bytes: &[u8]) -> Result<&'a [u8]> {
        ensure!(mode == self.mode, "attribution target mode changed");
        ensure!(
            crate::mutation_campaign::sha256_hex(bytes) == self.expected_mutant_sha256,
            "attribution source differs from the exact automatic mutation"
        );
        Ok(&self.original)
    }
}

/// Exact files or directory roots chosen by trusted coordinator policy, never a receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selection {
    excluded: BTreeSet<String>,
}
impl Selection {
    pub(crate) fn new(excluded: &[String], protected: &[String]) -> Result<Self> {
        let mut paths = BTreeSet::new();
        for path in excluded {
            validate_path(path).map_err(anyhow::Error::msg)?;
            ensure!(paths.insert(path.clone()), "duplicate snapshot exclusion");
        }
        ensure!(paths.len() <= 4096, "too many snapshot exclusions");
        for path in protected {
            validate_path(path).map_err(anyhow::Error::msg)?;
            ensure!(
                !paths.iter().any(|out| overlaps(out, path)),
                "snapshot exclusion overlaps protected input: {path}"
            );
        }
        Ok(Self { excluded: paths })
    }
    pub(crate) fn contains(&self, path: &str) -> bool {
        self.excluded.iter().any(|root| {
            path == root
                || path
                    .strip_prefix(root)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        })
    }
    pub(crate) fn excluded(&self) -> &BTreeSet<String> {
        &self.excluded
    }
}
/// Controls and recognized source/test paths may never be hidden in an output namespace.
pub(super) fn forbidden_output_input(path: &str) -> bool {
    path.split('/').any(|part| {
        matches!(
            part,
            ".cargo"
                | "src"
                | "tests"
                | "Cargo.toml"
                | "Cargo.lock"
                | "build.rs"
                | "forge.toml"
                | "pyproject.toml"
                | "rust-toolchain"
                | "rust-toolchain.toml"
        ) || [
            ".rs", ".py", ".c", ".cc", ".cpp", ".h", ".hpp", ".js", ".ts", ".tsx", ".jsx",
        ]
        .iter()
        .any(|suffix| part.ends_with(*suffix))
    })
}
fn overlaps(a: &str, b: &str) -> bool {
    a == b
        || a.strip_prefix(b).is_some_and(|s| s.starts_with('/'))
        || b.strip_prefix(a).is_some_and(|s| s.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_namespaces_cannot_hide_discovery_controls_or_sources() {
        for path in [
            "campaigns/Cargo.toml",
            "campaigns/nested/Cargo.lock",
            "campaigns/.cargo/config.toml",
            "campaigns/src/a.rs",
            "campaigns/tests/asset.dat",
            "campaigns/build.rs",
            "campaigns/hidden.py",
        ] {
            assert!(forbidden_output_input(path), "{path}");
        }
        assert!(!forbidden_output_input("campaigns/registry.d/a.json"));
    }
    #[test]
    fn exclusion_is_bounded_and_cannot_cover_protected_inputs() {
        assert!(Selection::new(&["campaigns".into()], &["campaigns/source.rs".into()]).is_err());
        assert!(Selection::new(&["../escape".into()], &[]).is_err());
        assert!(Selection::new(&[".git/config".into()], &[]).is_err());
        let selection =
            Selection::new(&["campaigns/receipts".into()], &["src/a.rs".into()]).unwrap();
        assert!(selection.contains("campaigns/receipts/a.json"));
        assert!(!selection.contains("campaigns/receipts-other/a.json"));
    }
}
