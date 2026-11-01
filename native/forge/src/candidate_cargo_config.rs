//! Snapshot-confined host roster resolution using the shared native path policy.
use anyhow::{Context, Result};
use cap_std::fs::Dir;
use conductor_native::{candidate_cargo_lint as lint, project_config, project_paths};
use std::path::Path;

pub(super) fn load(root: &Dir, label: &Path) -> Result<(String, lint::Roster)> {
    let override_set = match std::env::var("CONDUCTOR_CRATE_ROSTER") {
        Ok(value) => !value.trim().is_empty(),
        Err(std::env::VarError::NotPresent) => false,
        Err(std::env::VarError::NotUnicode(_)) => {
            anyhow::bail!("CONDUCTOR_CRATE_ROSTER must be UTF-8")
        }
    };
    let config = if override_set {
        project_config::parse_legacy(&label.join("pyproject.toml"), "")
    } else if let Some(bytes) = read(root, "forge.toml", project_config::CONFIG_LIMIT)? {
        project_config::parse_native(
            &label.join("forge.toml"),
            std::str::from_utf8(&bytes).context("forge.toml must be UTF-8")?,
        )
    } else {
        let bytes = read(root, "pyproject.toml", project_config::CONFIG_LIMIT)?.unwrap_or_default();
        project_config::parse_legacy(
            &label.join("pyproject.toml"),
            std::str::from_utf8(&bytes).context("pyproject.toml must be UTF-8")?,
        )
    }
    .map_err(anyhow::Error::msg)?;
    let (path, _) = project_paths::crate_roster_relative(&config).map_err(anyhow::Error::msg)?;
    let bytes = read(root, &path, 1024 * 1024)?
        .with_context(|| format!("required crate roster is absent: {}", path.escape_default()))?;
    let roster = lint::Roster::parse(&bytes).map_err(anyhow::Error::msg)?;
    Ok((path, roster))
}
fn read(root: &Dir, path: &str, maximum: usize) -> Result<Option<Vec<u8>>> {
    lint::read_optional(root, path, maximum).map_err(anyhow::Error::msg)
}
