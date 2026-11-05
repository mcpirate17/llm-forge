//! Disposable git snapshots for autonomous mutation from a dirty checkout.
//!
//! A private raw-blob export and synthetic Git commit of the checkout's tracked
//! and source-like untracked files. It writes no objects into the host repository. `remove` proves it is gone;
//! `Drop` is the backstop for every early return.

use conductor_native::project_config;
use conductor_native::project_paths;
use std::collections::BTreeSet;
use std::env;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
#[path = "mutation_snapshot_ignore.rs"]
mod ignore_rules;
#[path = "mutation_snapshot_native.rs"]
mod native;

const SOURCE_SUFFIXES: [&str; 20] = [
    ".c", ".cc", ".cpp", ".cu", ".h", ".hpp", ".js", ".json", ".jsx", ".md", ".py", ".pyx", ".pxd",
    ".rs", ".sh", ".toml", ".ts", ".tsx", ".yaml", ".yml",
];
const FIXED_EXCLUDED_PREFIXES: [&str; 3] = [
    "conductor/mutation_campaigns/receipts/",
    "research/reports/",
    "tasks/",
];
const REGISTRY_INDEX: usize = 1;
const RECEIPT_ROOT_INDEX: usize = 3;

/// What never rides along: receipts, reports, task notes and the notes tree.
fn excluded_prefixes(repo: &Path) -> Result<Vec<String>, String> {
    let mut prefixes: Vec<String> = FIXED_EXCLUDED_PREFIXES.map(str::to_string).to_vec();
    let paths = project_paths::resolve(repo)?;
    if let Some(parent) = Path::new(&paths[REGISTRY_INDEX].0).parent() {
        let parent = parent.to_string_lossy();
        if !parent.is_empty() {
            prefixes.push(format!("{parent}/receipts/"));
        }
    }
    prefixes.push(format!("{}/", paths[RECEIPT_ROOT_INDEX].0));
    let notes = project_paths::notes_root(repo)?;
    let notes = notes.strip_prefix(repo).unwrap_or(&notes);
    prefixes.push(format!("{}/", notes.to_string_lossy()));
    Ok(prefixes)
}

/// A path inside test material, where the suffix allowlist cannot apply.
fn fixture_path(path: &str) -> bool {
    path.split('/')
        .any(|part| matches!(part, "tests" | "fixtures") || part.starts_with("test_"))
}

fn configured_extra_suffixes(repo: &Path) -> Result<BTreeSet<String>, String> {
    let config = project_config::load(repo)?;
    let Some(raw) = config.conductor.get("snapshot_extra_suffixes") else {
        return Ok(BTreeSet::new());
    };
    let source = "[tool.conductor].snapshot_extra_suffixes";
    let shape = || format!("{source} must be a list of file suffixes");
    let mut suffixes = BTreeSet::new();
    for item in raw.as_array().ok_or_else(shape)? {
        let text = item.as_str().ok_or_else(shape)?.trim().to_lowercase();
        if text.is_empty() {
            return Err(format!("{source} must not contain an empty suffix"));
        }
        suffixes.insert(if text.starts_with('.') {
            text
        } else {
            format!(".{text}")
        });
    }
    Ok(suffixes)
}

/// Explicit optional Python guest; native engines never discover one implicitly.
pub fn configured_snapshot_python(repo: &Path) -> Result<Option<String>, String> {
    let config = project_config::load(repo)?;
    if let Some(raw) = config.conductor.get("snapshot_python") {
        return raw
            .as_str()
            .filter(|text| !text.trim().is_empty())
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| {
                "[tool.conductor].snapshot_python must be a path to a Python interpreter".into()
            });
    }
    Ok(None)
}

/// Fest requires the configured guest, its host venv, or `python3` from PATH.
pub fn snapshot_python(repo: &Path) -> Result<String, String> {
    if let Some(interpreter) = configured_snapshot_python(repo)? {
        return Ok(interpreter);
    }
    let venv = repo.join(".venv/bin/python");
    if venv.is_file() {
        return Ok(venv.to_string_lossy().into_owned());
    }
    on_path("python3").map_or_else(
        || {
            Err(
                "no Python interpreter: set [tool.conductor].snapshot_python, provide \
                 .venv/bin/python or put python3 on PATH"
                    .to_string(),
            )
        },
        |found| Ok(found.to_string_lossy().into_owned()),
    )
}

/// `shutil.which`: the first executable `name` on PATH.
pub fn on_path(name: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .map(|directory| directory.join(name))
        .find(|candidate| is_executable(candidate))
}

pub fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Source and config files a snapshot needs, without generated artifacts.
pub fn snapshot_untracked_paths(repo: &Path) -> Result<Vec<String>, String> {
    native::untracked(repo)
}

fn private_root() -> Result<PathBuf, String> {
    let template = env::temp_dir().join("llm-orchestrator-snapshot-XXXXXX");
    let template = CString::new(template.as_os_str().as_bytes())
        .map_err(|error| format!("temporary path holds a NUL: {error}"))?;
    let mut bytes = template.into_bytes_with_nul();
    // SAFETY: `bytes` is a writable NUL-terminated template mkdtemp fills in place.
    let made = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) };
    if made.is_null() {
        return Err(format!(
            "cannot create a snapshot directory: {}",
            std::io::Error::last_os_error()
        ));
    }
    bytes.pop();
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// One disposable checkout of the host's current state.
#[derive(Debug)]
pub struct Snapshot {
    pub root: PathBuf,
    pub worktree: PathBuf,
    pub commit: String,
    pub included_untracked: Vec<String>,
    removed: bool,
    preserved: bool,
}

impl Snapshot {
    /// Build a clean snapshot without writing objects into the host repository.
    pub fn create(repo: &Path) -> Result<Self, String> {
        let root = private_root()?;
        let worktree = root.join("worktree");
        let mut snapshot = Self {
            root,
            worktree,
            commit: String::new(),
            included_untracked: Vec::new(),
            removed: false,
            preserved: false,
        };
        snapshot.populate(repo)?;
        Ok(snapshot)
    }

    fn populate(&mut self, repo: &Path) -> Result<(), String> {
        let (commit, untracked) = native::populate(repo, &self.root, &self.worktree)?;
        self.commit = commit;
        self.included_untracked = untracked;
        Ok(())
    }

    /// Disable automatic deletion while execution cleanup is uncertain. Explicit
    /// removal remains available after the owning coordinator proves cleanup.
    pub(crate) fn preserve(&mut self) {
        self.preserved = true;
    }

    /// Delete the snapshot and prove it is gone.
    pub fn remove(mut self) -> Result<(), String> {
        self.removed = true;
        let result = fs::remove_dir_all(&self.root);
        if self.root.exists() {
            let detail = result.err().map(|error| error.to_string());
            return Err(format!(
                "snapshot {} survived removal: {}",
                self.root.display(),
                detail.unwrap_or_else(|| "it still exists".to_string())
            ));
        }
        Ok(())
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if self.removed {
            return;
        }
        if self.preserved {
            eprintln!("forge: retained mutation snapshot: {}", self.root.display());
            return;
        }
        if let Err(error) = fs::remove_dir_all(&self.root) {
            if self.root.exists() {
                eprintln!(
                    "forge: snapshot {} survived cleanup: {error}",
                    self.root.display()
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "mutation_snapshot_tests.rs"]
mod tests;
