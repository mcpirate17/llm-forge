//! Shared workspace profile output policy derived only from captured project configuration.
use crate::mutation_snapshot::SnapshotSelection;
use anyhow::{ensure, Context, Result};
use conductor_native::{project_config::ProjectConfig, project_paths};
use std::path::Path;

fn registry_parent(config: Option<&ProjectConfig>) -> Result<String> {
    let registry =
        project_paths::mutation_registry_from_config(config).map_err(anyhow::Error::msg)?;
    Ok(Path::new(&registry)
        .parent()
        .and_then(Path::to_str)
        .filter(|path| !path.is_empty())
        .context("workspace mutation registry must have a non-root parent")?
        .to_owned())
}
fn namespaces(config: Option<&ProjectConfig>) -> Result<Vec<String>> {
    let mut roots = vec![
        registry_parent(config)?,
        project_paths::mutation_receipt_root_from_config(config).map_err(anyhow::Error::msg)?,
    ];
    roots.sort();
    roots.dedup();
    let all = roots.clone();
    roots.retain(|root| !all.iter().any(|other| other != root && inside(root, other)));
    Ok(roots)
}
fn inside(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}
/// Output namespaces are trusted configuration, never receipt-supplied paths.
pub(crate) fn selection(
    config: Option<&ProjectConfig>,
    protected: &[String],
) -> Result<SnapshotSelection> {
    let mut protected = protected.to_vec();
    protected.extend(["forge.toml".to_owned(), "pyproject.toml".to_owned()]);
    SnapshotSelection::new(&namespaces(config)?, &protected)
}
/// A receipt may be published only under the configured receipt root.
pub(crate) fn receipt_in_namespace(config: Option<&ProjectConfig>, path: &str) -> Result<()> {
    conductor_native::mutation_snapshot_inputs::validate_path(path).map_err(anyhow::Error::msg)?;
    let root =
        project_paths::mutation_receipt_root_from_config(config).map_err(anyhow::Error::msg)?;
    ensure!(
        conductor_native::mutation_snapshot_inputs::within_namespace(path, &root),
        "workspace receipt must be within the configured receipt root"
    );
    Ok(())
}
/// Generated manifests must be inside the configured registry parent.
pub(crate) fn manifest_in_namespace(config: Option<&ProjectConfig>, path: &str) -> Result<()> {
    conductor_native::mutation_snapshot_inputs::validate_path(path).map_err(anyhow::Error::msg)?;
    let root = registry_parent(config)?;
    ensure!(
        conductor_native::mutation_snapshot_inputs::within_namespace(path, &root),
        "workspace manifest must be within the registry parent"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_outputs_cannot_cover_source_or_escape_namespace() {
        let selected = selection(None, &["src/main.rs".into()]).unwrap();
        assert!(selected.contains("conductor/mutation_campaigns/registry.json"));
        assert!(receipt_in_namespace(None, "research/reports/mutation_testing/a.json").is_ok());
        assert!(receipt_in_namespace(None, "src/a.rs").is_err());
        assert!(selection(None, &["conductor/mutation_campaigns/src/a.rs".into()]).is_err());
    }
}
