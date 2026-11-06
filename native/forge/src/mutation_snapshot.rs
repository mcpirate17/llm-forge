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
#[path = "mutation_snapshot_inputs.rs"]
mod inputs;
pub(crate) use inputs::Replacement as SnapshotReplacement;
pub(crate) use inputs::Selection as SnapshotSelection;
#[path = "mutation_snapshot_ignore.rs"]
mod ignore_rules;
#[path = "mutation_snapshot_native.rs"]
mod native;
pub(crate) use native::live::LiveBinding;

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

fn private_root(parent: &Path) -> Result<PathBuf, String> {
    let parent = parent
        .canonicalize()
        .map_err(|error| format!("cannot resolve snapshot parent: {error}"))?;
    if !parent.is_dir() {
        return Err("snapshot parent must be a directory".into());
    }
    let template = parent.join("llm-orchestrator-snapshot-XXXXXX");
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

/// Independently observe the same live inputs used by snapshot construction.
pub(crate) fn live_input_identity(
    repo: &Path,
    selection: &SnapshotSelection,
) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs, String> {
    native::live_identity(repo, selection).map_err(|e| format!("snapshot live identity: {e:#}"))
}

/// Independently observe an immutable candidate Git tree.
pub(crate) fn git_input_identity(
    source: &crate::candidate_source::Source,
    tree: &str,
    selection: &SnapshotSelection,
) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs, String> {
    native::git_identity(source, tree, selection)
        .map_err(|e| format!("snapshot Git identity: {e:#}"))
}

/// Recompute the materialized snapshot independently of its synthetic Git metadata.
pub(crate) fn materialized_input_identity(
    root: &Path,
    selection: &SnapshotSelection,
) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs, String> {
    native::materialized_identity(root, selection)
        .map_err(|e| format!("snapshot materialized identity: {e:#}"))
}

/// Bind an engine-generated replacement to the original selected source before applying it.
pub(crate) fn attribution_replacement(
    root: &Path,
    row: &serde_json::Value,
    pins: &std::collections::BTreeMap<String, String>,
) -> Result<SnapshotReplacement, String> {
    native::attribution_replacement(root, row, pins)
        .map_err(|e| format!("snapshot attribution replacement: {e:#}"))
}

/// Hash every original input while verifying one exact expected automatic mutation.
pub(crate) fn materialized_input_identity_with_replacement(
    root: &Path,
    selection: &SnapshotSelection,
    replacement: &SnapshotReplacement,
) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs, String> {
    native::materialized_identity_replaced(root, selection, Some(replacement))
        .map_err(|e| format!("snapshot attribution identity: {e:#}"))
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
    allocation: native::live::Allocation,
}

impl Snapshot {
    /// Build a clean snapshot without writing objects into the host repository.
    pub fn create(repo: &Path) -> Result<Self, String> {
        let mut snapshot = Self::allocate(&env::temp_dir())?;
        snapshot.populate(repo)?;
        Ok(snapshot)
    }

    /// Allocate only the empty private root for sandbox admission.
    pub(crate) fn allocate(parent: &Path) -> Result<Self, String> {
        let root = private_root(parent)?
            .canonicalize()
            .map_err(|e| format!("snapshot allocation path: {e}"))?;
        let allocation = native::live::Allocation::capture(&root)
            .map_err(|e| format!("snapshot allocation identity: {e:#}"))?;
        let worktree = root.join("worktree");
        Ok(Self {
            root,
            worktree,
            commit: String::new(),
            included_untracked: Vec::new(),
            removed: false,
            preserved: false,
            allocation,
        })
    }

    /// Explicit live selection; no mutation ignore/suffix/output policy is reused.
    pub(crate) fn populate_live(
        &mut self,
        repo: &Path,
        roots: &[String],
    ) -> Result<LiveBinding, String> {
        self.allocation
            .verify(&self.root)
            .map_err(|e| format!("snapshot root: {e:#}"))?;
        let binding = native::live::populate_selected(repo, &self.root, &self.worktree, roots)
            .map_err(|e| format!("live snapshot: {e:#}"))?;
        self.commit = binding.commit().to_owned();
        Ok(binding)
    }

    pub(crate) fn verify_live(&self, repo: &Path, binding: &LiveBinding) -> Result<(), String> {
        self.allocation
            .verify(&self.root)
            .map_err(|e| format!("snapshot root: {e:#}"))?;
        binding
            .verify_host(repo)
            .and_then(|()| binding.verify_snapshot(&self.worktree))
            .map_err(|e| format!("live snapshot verification: {e:#}"))
    }

    pub(crate) fn populate(&mut self, repo: &Path) -> Result<(), String> {
        let (commit, untracked) = native::populate(repo, &self.root, &self.worktree)?;
        self.commit = commit;
        self.included_untracked = untracked;
        Ok(())
    }

    /// Populate with coordinator-derived output exclusions and return captured input identity.
    pub(crate) fn populate_bound(
        &mut self,
        repo: &Path,
        selection: &SnapshotSelection,
    ) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs, String> {
        let (commit, untracked, identity) =
            native::populate_bound(repo, &self.root, &self.worktree, selection)?;
        self.commit = commit;
        self.included_untracked = untracked;
        Ok(identity)
    }

    /// Recheck the current host and materialized snapshot against the captured identity.
    pub(crate) fn verify_bound(
        &self,
        repo: &Path,
        selection: &SnapshotSelection,
        expected: &conductor_native::mutation_cargo_supporting_binding::SupportingInputs,
    ) -> Result<(), String> {
        if live_input_identity(repo, selection)? != *expected {
            return Err("host snapshot inputs changed".into());
        }
        if materialized_input_identity(&self.worktree, selection)? != *expected {
            return Err("materialized snapshot inputs changed".into());
        }
        Ok(())
    }

    /// Remove coordinator-created private Git metadata before a no-Git payload.
    /// The synthetic commit remains diagnostic only; no Git history is promised.
    pub(crate) fn remove_git_metadata(&self) -> Result<(), String> {
        let path = self.worktree.join(".git");
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot inspect snapshot Git metadata: {error}"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("snapshot Git metadata is not a private directory".into());
        }
        fs::remove_dir_all(&path)
            .map_err(|error| format!("cannot remove snapshot Git metadata: {error}"))?;
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "cannot verify removed snapshot Git metadata: {error}"
            )),
            Ok(_) => Err("snapshot Git metadata survived removal".into()),
        }
    }

    /// Disable automatic deletion while execution cleanup is uncertain. Explicit
    /// removal remains available after the owning coordinator proves cleanup.
    pub(crate) fn preserve(&mut self) {
        self.preserved = true;
    }

    /// Delete the snapshot and prove it is gone.
    pub fn remove(mut self) -> Result<(), String> {
        self.removed = true;
        self.allocation
            .remove(&self.root)
            .map_err(|e| format!("snapshot {} cleanup unverified: {e:#}", self.root.display()))
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
        if let Err(error) = self.allocation.remove(&self.root) {
            eprintln!(
                "forge: snapshot {} cleanup unverified: {error:#}",
                self.root.display()
            );
        }
    }
}

#[cfg(test)]
#[path = "mutation_snapshot_inputs_tests.rs"]
mod input_tests;

#[cfg(test)]
#[path = "mutation_snapshot_tests.rs"]
mod tests;
