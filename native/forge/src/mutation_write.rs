//! Publish planned campaign manifests and their `registry.d/` fragments.
//!
//! A manifest that has run carries a recorded survivor baseline, so replacing
//! it without `--force` is refused: an empty baseline would erase the ratchet
//! while still reporting green.

use crate::json_canon::python_pretty;
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const FRAGMENT_DIRECTORY: &str = "registry.d";

/// Publish `text` at `path` through a sibling temporary and one rename.
pub fn atomic_write(path: &Path, text: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| format!("mkdir {}: {error}", parent.display()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no file name", path.display()))?;
    let temporary = parent.join(format!(".{name}.{}.tmp", std::process::id()));
    let published = fs::File::create(&temporary)
        .and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .and_then(|()| fs::rename(&temporary, path));
    if let Err(error) = published {
        // The temporary is this process's own; the publish error is what matters.
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot publish {}: {error}", path.display()));
    }
    Ok(())
}

/// The fragment that registers `manifest` and its bytes, or None when it stands.
fn pending_fragment(
    registry: &Path,
    campaign_id: &str,
    manifest: &str,
    repo_root: &Path,
) -> Result<Option<(PathBuf, String)>, String> {
    let directory = registry.parent().unwrap_or(repo_root);
    let fragment = directory
        .join(FRAGMENT_DIRECTORY)
        .join(format!("{campaign_id}.json"));
    let expected = json!({"manifest": manifest});
    if fragment.exists() {
        let declared: Value = fs::read_to_string(&fragment)
            .map_err(|error| error.to_string())
            .and_then(|text| serde_json::from_str(&text).map_err(|error| error.to_string()))
            .map_err(|error| format!("cannot read fragment {}: {error}", fragment.display()))?;
        if declared != expected {
            let shown = fragment.strip_prefix(repo_root).unwrap_or(&fragment);
            return Err(format!(
                "{} already registers a different manifest",
                shown.display()
            ));
        }
        return Ok(None);
    }
    let body = serde_json::to_string_pretty(&expected).map_err(|error| error.to_string())?;
    Ok(Some((fragment, body + "\n")))
}

fn relative_text(path: &Path, repo_root: &Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Write and register each manifest; every file written, manifest then fragment.
pub fn write_manifests(
    manifests: &[Value],
    repo_root: &Path,
    registry_relative: &str,
    campaigns_relative: &str,
    force: bool,
) -> Result<Vec<String>, String> {
    let registry = repo_root.join(registry_relative);
    let mut written = Vec::new();
    for manifest in manifests {
        let campaign_id = manifest
            .get("campaign_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "planned manifest has no campaign_id".to_string())?;
        let relative = format!("{campaigns_relative}/{campaign_id}.json");
        let destination = repo_root.join(&relative);
        if destination.exists() && !force {
            return Err(format!(
                "{relative} already exists; refusing to replace a recorded survivor \
                 baseline with an empty one (pass --force to overwrite)"
            ));
        }
        let fragment = pending_fragment(&registry, campaign_id, &relative, repo_root)?;
        let body = python_pretty(manifest.clone()).map_err(|error| error.to_string())?;
        atomic_write(&destination, &body)?;
        written.push(relative);
        if let Some((path, body)) = fragment {
            atomic_write(&path, &body)?;
            written.push(relative_text(&path, repo_root));
        }
    }
    Ok(written)
}

#[cfg(test)]
#[path = "mutation_write_tests.rs"]
mod tests;
