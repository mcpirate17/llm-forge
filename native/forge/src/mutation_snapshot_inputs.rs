//! Coordinator-supplied snapshot output exclusions and their protected inputs.
use anyhow::{ensure, Result};
use conductor_native::mutation_snapshot_inputs::validate_path;
use std::collections::BTreeSet;

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
