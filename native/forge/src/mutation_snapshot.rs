//! Disposable git snapshots for autonomous mutation from a dirty checkout.
//!
//! A snapshot is a shared clone in a private temporary directory, checked out at
//! a synthetic commit of the checkout's tracked and source-like untracked files.
//! It writes no objects into the host repository. `remove` proves it is gone;
//! `Drop` is the backstop for every early return.

use conductor_native::project_config;
use conductor_native::project_paths;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::{CString, OsStr};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SOURCE_SUFFIXES: [&str; 20] = [
    ".c", ".cc", ".cpp", ".cu", ".h", ".hpp", ".js", ".json", ".jsx", ".md", ".py", ".pyx", ".pxd",
    ".rs", ".sh", ".toml", ".ts", ".tsx", ".yaml", ".yml",
];
const FIXED_EXCLUDED_PREFIXES: [&str; 3] = [
    "conductor/mutation_campaigns/receipts/",
    "research/reports/",
    "tasks/",
];
const UNTRACKED_CHUNK: usize = 100;
const DETAIL_LIMIT: usize = 2000;
const REGISTRY_INDEX: usize = 1;
const RECEIPT_ROOT_INDEX: usize = 3;
const SNAPSHOT_IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Audit Orchestrator Snapshot"),
    ("GIT_AUTHOR_EMAIL", "orchestrator@localhost"),
    ("GIT_COMMITTER_NAME", "Audit Orchestrator Snapshot"),
    ("GIT_COMMITTER_EMAIL", "orchestrator@localhost"),
];

fn git(
    args: &[&OsStr],
    cwd: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .envs(environment)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("cannot start git: {error}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let shown: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let raw = if stderr.is_empty() { &stdout } else { &stderr };
    let detail = match raw.trim() {
        "" => "git command failed",
        text => text,
    };
    let detail: String = detail.chars().take(DETAIL_LIMIT).collect();
    Err(format!("git {} failed: {detail}", shown.join(" ")))
}

fn text_args<'a>(args: &'a [&'a str]) -> Vec<&'a OsStr> {
    args.iter().map(OsStr::new).collect()
}

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
    let listed = git(
        &text_args(&["ls-files", "--others", "--exclude-standard"]),
        repo,
        &BTreeMap::new(),
    )?;
    let excluded = excluded_prefixes(repo)?;
    let extra = configured_extra_suffixes(repo)?;
    let mut paths: Vec<String> = listed
        .lines()
        .filter(|path| !path.is_empty())
        .filter(|path| {
            !excluded
                .iter()
                .any(|prefix| path.starts_with(prefix.as_str()))
        })
        .filter(|path| {
            let suffix = Path::new(path)
                .extension()
                .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
                .unwrap_or_default();
            fixture_path(path)
                || SOURCE_SUFFIXES.contains(&suffix.as_str())
                || extra.contains(&suffix)
        })
        .map(str::to_string)
        .collect();
    paths.sort();
    Ok(paths)
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
        let resolved = repo
            .canonicalize()
            .map_err(|error| format!("cannot resolve {}: {error}", repo.display()))?;
        let none = BTreeMap::new();
        let mut clone = text_args(&["clone", "--shared", "--no-checkout", "--quiet"]);
        clone.push(resolved.as_os_str());
        clone.push(self.worktree.as_os_str());
        git(&clone, repo, &none)?;
        self.commit_state(repo)?;
        git(
            &text_args(&["checkout", "--detach", "--quiet", &self.commit]),
            &self.worktree,
            &none,
        )?;
        Ok(())
    }

    fn commit_state(&mut self, repo: &Path) -> Result<(), String> {
        let path_text = |path: &Path| path.to_string_lossy().into_owned();
        let mut environment: BTreeMap<String, String> = SNAPSHOT_IDENTITY
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        environment.insert("GIT_DIR".into(), path_text(&self.worktree.join(".git")));
        environment.insert("GIT_WORK_TREE".into(), path_text(repo));
        environment.insert("GIT_INDEX_FILE".into(), path_text(&self.root.join("index")));
        git(&text_args(&["read-tree", "HEAD"]), repo, &environment)?;
        git(&text_args(&["add", "-u"]), repo, &environment)?;
        let untracked = snapshot_untracked_paths(repo)?;
        for chunk in untracked.chunks(UNTRACKED_CHUNK) {
            let mut add = text_args(&["add", "--"]);
            add.extend(chunk.iter().map(OsStr::new));
            git(&add, repo, &environment)?;
        }
        let tree = git(&text_args(&["write-tree"]), repo, &environment)?;
        let parent = git(&text_args(&["rev-parse", "HEAD"]), repo, &BTreeMap::new())?;
        let commit = git(
            &text_args(&[
                "commit-tree",
                tree.trim(),
                "-p",
                parent.trim(),
                "-m",
                "orchestrator snapshot",
            ]),
            repo,
            &environment,
        )?;
        self.commit = commit.trim().to_string();
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
