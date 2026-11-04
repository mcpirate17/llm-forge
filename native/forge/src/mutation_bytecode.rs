//! Bytecode-cache isolation for child interpreters.
//!
//! CPython trusts a cache entry when the source's mtime and size match, so a
//! same-size rewrite in the same second leaves stale bytecode behind. A mutation
//! engine does that on purpose. Every child reads a run-private
//! `PYTHONPYCACHEPREFIX`, and each mutated file's entries are evicted before the
//! child that must not trust them starts. `PYTHONDONTWRITEBYTECODE` stays unset:
//! unmutated modules compile once rather than once per mutant.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Written into every scratch: the engines' pytest plugin deletes only marked ones.
const RUN_MARKER_NAME: &str = ".bytecode-isolation-run";

/// The run-private isolation root for children launched in `cwd`.
pub fn scratch_root_for(cwd: &Path) -> PathBuf {
    cwd.join(".bytecode-isolation")
}

/// Delete the cached bytecode of `mutated_paths` under the run's prefix.
///
/// Entries are listed rather than computed, so no interpreter cache tag is
/// needed: a prefix mirrors the source's absolute directory, and an entry is
/// `<stem>.<tag>[.opt-N].pyc`.
pub fn evict_mutated_caches(mutated_paths: &[PathBuf], scratch: &Path) -> Result<(), String> {
    let prefix = scratch.join("pycache");
    let Ok(root) = prefix.canonicalize() else {
        return Ok(());
    };
    for source in mutated_paths {
        let absolute = source
            .canonicalize()
            .map_err(|error| format!("cannot resolve {}: {error}", source.display()))?;
        let stem = absolute
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| format!("{} has no file stem", absolute.display()))?;
        let parent = absolute.parent().unwrap_or(Path::new("/"));
        let mirrored = root.join(parent.strip_prefix("/").unwrap_or(parent));
        evict_entries(&mirrored, stem, &root)?;
    }
    Ok(())
}

fn evict_entries(directory: &Path, stem: &str, root: &Path) -> Result<(), String> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Ok(());
    };
    let wanted = format!("{stem}.");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&wanted) || !name.ends_with(".pyc") {
            continue;
        }
        let cache = entry.path();
        let resolved = cache
            .canonicalize()
            .map_err(|error| format!("cannot resolve {}: {error}", cache.display()))?;
        if !resolved.starts_with(root) {
            return Err(format!(
                "refusing to evict {}: it is not under the run's cache prefix {}",
                cache.display(),
                root.display()
            ));
        }
        fs::remove_file(&resolved)
            .map_err(|error| format!("cannot evict {}: {error}", resolved.display()))?;
    }
    Ok(())
}

fn prepare(scratch: &Path, prefix: &Path) -> std::io::Result<()> {
    fs::create_dir_all(prefix)?;
    let probe = prefix.join(format!(".writable-{}", std::process::id()));
    fs::write(&probe, b"")?;
    fs::remove_file(&probe)?;
    fs::write(scratch.join(RUN_MARKER_NAME), b"")
}

/// `base` bound to the run's private bytecode cache, mutated sources evicted.
///
/// A run that cannot have its own cache fails rather than falling back to shared
/// ones, because that fallback is the stale read this module exists to prevent.
pub fn isolated_python_env(
    base: &BTreeMap<String, String>,
    scratch: &Path,
    mutated_paths: &[PathBuf],
) -> Result<BTreeMap<String, String>, String> {
    let prefix = scratch.join("pycache");
    prepare(scratch, &prefix).map_err(|error| {
        format!(
            "bytecode isolation scratch {} is not usable: {error}",
            scratch.display()
        )
    })?;
    evict_mutated_caches(mutated_paths, scratch)?;
    let mut environment = base.clone();
    environment.remove("PYTHONDONTWRITEBYTECODE");
    environment.insert(
        "PYTHONPYCACHEPREFIX".into(),
        prefix.to_string_lossy().into_owned(),
    );
    Ok(environment)
}

#[cfg(test)]
#[path = "mutation_bytecode_tests.rs"]
mod tests;
