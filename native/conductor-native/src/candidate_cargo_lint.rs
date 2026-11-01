//! Shared candidate crate roster/selection only: no process, environment or config loading.
//! Caller pins the directory and keeps its descendants immutable during planning/use.
mod inventory;
mod read;
mod roster;
mod scope;
#[cfg(test)]
mod scope_tests;
#[cfg(test)]
mod tests;
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
pub use read::read_optional;
pub use roster::Roster;
use serde::{Deserialize, Serialize};
use std::io;
const MAX_PATH: usize = 4096;
const MAX_DEPTH: usize = 128;
const MAX_SELECTED: usize = 100_000;
const MAX_BYTES: usize = 8 * 1024 * 1024;
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Fmt,
    Clippy,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum SkipKind {
    Unstyled,
    NotLinted,
    Prerequisite,
}
#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct InputOwner {
    pub input: String,
    pub crate_path: String,
    pub manifest: String,
}
#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct Skip {
    pub crate_path: String,
    pub manifest: String,
    pub kind: SkipKind,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct LintPlan {
    pub inventory: Vec<String>,
    pub manifests: Vec<String>,
    pub input_owners: Vec<InputOwner>,
    pub skips: Vec<Skip>,
    pub scope_expansions: Vec<ScopeExpansion>,
}
#[derive(Clone, Copy, Debug, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ScopeReason {
    Workspace,
    BorrowedTarget,
}
#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct ScopeExpansion {
    pub manifest: String,
    pub reason: ScopeReason,
    pub trigger: String,
}
pub(super) struct Budget {
    probes: usize,
    entries: usize,
    work: usize,
    retained: usize,
}
impl Budget {
    fn new() -> Self {
        Self {
            probes: 0,
            entries: 0,
            work: 0,
            retained: 0,
        }
    }
    fn probe(&mut self) -> Result<(), String> {
        self.probes = self.probes.checked_add(1).ok_or("lint probe overflow")?;
        if self.probes > 1_000_000 {
            return Err("lint metadata/work probes exceed 1000000".into());
        }
        Ok(())
    }
    fn entry(&mut self) -> Result<(), String> {
        self.entries += 1;
        if self.entries > 100_000 {
            return Err("lint examined entries exceed 100000".into());
        }
        Ok(())
    }
    fn matching(&mut self, pattern: &str, name: &str) -> Result<(), String> {
        self.work = self
            .work
            .checked_add(
                (pattern.len() + 1)
                    .checked_mul(name.len() + 1)
                    .ok_or("lint glob work overflow")?,
            )
            .ok_or("lint glob work overflow")?;
        if self.work > 100_000_000 {
            return Err("lint glob work exceeds 100000000".into());
        }
        Ok(())
    }
    fn retain(&mut self, bytes: usize) -> Result<(), String> {
        self.retained = self
            .retained
            .checked_add(bytes)
            .ok_or("lint retained path size overflow")?;
        if self.retained > 24 * 1024 * 1024 {
            return Err("lint retained path bytes exceed 24 MiB".into());
        }
        Ok(())
    }
}
pub fn plan(
    root: &Dir,
    roster: &Roster,
    selected: &[&str],
    mode: Mode,
) -> Result<LintPlan, String> {
    admit_selected(selected)?;
    let mut budget = Budget::new();
    let found = inventory::collect(root, &roster.patterns, &mut budget)?;
    let unknown: Vec<&str> = found
        .iter()
        .map(|p| crate_of_manifest(p))
        .filter(|p| !roster.tested.contains(*p) && !roster.excluded.contains(*p))
        .collect();
    if !unknown.is_empty() {
        return Err(diagnostic("unclassified crates", &unknown));
    }
    let mut selected = selected.to_vec();
    selected.sort_unstable();
    selected.dedup();
    let mut input_owners = Vec::with_capacity(selected.len());
    let mut orphans = Vec::new();
    for input in selected {
        match owner(root, input, &mut budget)? {
            Some(crate_path) => {
                budget.retain(input.len() + crate_path.len() * 2 + manifest(&crate_path).len())?;
                input_owners.push(InputOwner {
                    input: input.into(),
                    manifest: manifest(&crate_path),
                    crate_path,
                });
            }
            None => orphans.push(input),
        }
    }
    if !orphans.is_empty() {
        return Err(diagnostic("no owning Cargo.toml", &orphans));
    }
    let scoped = scope::expand(root, &found, &input_owners, &mut budget)?;
    let mut manifests = Vec::new();
    let mut skips = Vec::new();
    for scoped_manifest in scoped.manifests {
        let crate_path = crate_of_manifest(&scoped_manifest).to_owned();
        let reason = match mode {
            Mode::Fmt if roster.unstyled.contains(&crate_path) => {
                Some((SkipKind::Unstyled, "listed in crates.unstyled".into()))
            }
            Mode::Clippy if !roster.linted.contains(&crate_path) => {
                Some((SkipKind::NotLinted, "not in crates.linted".into()))
            }
            Mode::Clippy => prerequisite(root, roster, &crate_path, &mut budget)?
                .map(|r| (SkipKind::Prerequisite, r)),
            _ => None,
        };
        let manifest = manifest(&crate_path);
        budget.retain(manifest.len())?;
        if let Some((kind, reason)) = reason {
            budget.retain(reason.len())?;
            skips.push(Skip {
                crate_path,
                manifest,
                kind,
                reason,
            });
        } else {
            manifests.push(manifest);
        }
    }
    manifests.sort_unstable();
    let plan = LintPlan {
        inventory: found.into_iter().collect(),
        manifests,
        input_owners,
        skips,
        scope_expansions: scoped.expansions,
    };
    crate::candidate_cargo_audit::serialized_size(&plan, 24 * 1024 * 1024)?;
    Ok(plan)
}
fn crate_of_manifest(path: &str) -> &str {
    path.rsplit_once('/').map(|v| v.0).unwrap_or(".")
}
fn admit_selected(selected: &[&str]) -> Result<(), String> {
    if selected.len() > MAX_SELECTED {
        return Err("lint selected count exceeds 100000".into());
    }
    let mut bytes = 0usize;
    for path in selected {
        path_parts(path, true)?;
        bytes = bytes
            .checked_add(path.len() + 1)
            .ok_or("lint input byte overflow")?;
        if bytes > MAX_BYTES {
            return Err("lint selected paths exceed 8 MiB".into());
        }
    }
    Ok(())
}
pub(super) fn path_parts(raw: &str, root: bool) -> Result<Vec<&str>, String> {
    if root && raw == "." {
        return Ok(Vec::new());
    }
    if raw.is_empty() || raw.len() > MAX_PATH || raw.starts_with('/') || raw.contains('\0') {
        return Err("lint path must be bounded canonical relative UTF-8".into());
    }
    let parts: Vec<_> = raw.split('/').collect();
    if parts.len() > MAX_DEPTH
        || parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return Err("lint path components/depth invalid".into());
    }
    Ok(parts)
}
pub(super) fn joined(parent: &str, name: &str) -> Result<String, String> {
    let result = if parent == "." {
        name.into()
    } else {
        format!("{parent}/{name}")
    };
    path_parts(&result, false)?;
    Ok(result)
}
fn manifest(crate_path: &str) -> String {
    if crate_path == "." {
        "Cargo.toml".into()
    } else {
        format!("{crate_path}/Cargo.toml")
    }
}
pub(super) fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    options
}
pub(super) fn metadata(
    dir: &Dir,
    name: &str,
    budget: &mut Budget,
) -> Result<Option<cap_std::fs::Metadata>, String> {
    budget.probe()?;
    match dir.symlink_metadata(name) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Err("lint paths must not traverse symlinks".into())
        }
        Ok(meta) => Ok(Some(meta)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error("inspect path", &e)),
    }
}
pub(super) fn directory(dir: &Dir, name: &str, budget: &mut Budget) -> Result<Dir, String> {
    budget.probe()?;
    dir.open_with(name, &options())
        .map(|f| Dir::from_std_file(f.into_std()))
        .map_err(|e| io_error("open directory", &e))
}
fn owner(root: &Dir, input: &str, budget: &mut Budget) -> Result<Option<String>, String> {
    let parts = path_parts(input, true)?;
    budget.probe()?;
    let mut dirs = vec![(
        ".".to_owned(),
        root.try_clone().map_err(|e| io_error("clone root", &e))?,
    )];
    for (index, part) in parts.iter().enumerate() {
        let (prefix, dir) = dirs.last().expect("root exists");
        match metadata(dir,part,budget)? {
            None=>break,
            Some(meta) if meta.is_dir()=>{let child=directory(dir,part,budget)?;dirs.push((joined(prefix,part)?,child));},
            Some(meta) if meta.is_file() && index+1==parts.len()=>break,
            _=>return Err("lint selected path must be a regular file or directory, with real directory parents".into()),
        }
    }
    for (prefix, dir) in dirs.iter().rev() {
        if let Some(meta) = metadata(dir, "Cargo.toml", budget)? {
            if !meta.is_file() {
                return Err("lint owning Cargo.toml must be a regular file".into());
            }
            let name = manifest(prefix);
            path_parts(&name, false)?;
            return Ok(Some(prefix.clone()));
        }
    }
    Ok(None)
}
fn prerequisite(
    root: &Dir,
    roster: &Roster,
    crate_path: &str,
    budget: &mut Budget,
) -> Result<Option<String>, String> {
    let Some(declared) = roster.prerequisites.get(crate_path) else {
        return Ok(None);
    };
    if let Some(artifact) = &declared.artifact {
        let parts = path_parts(artifact, false)?;
        if let Some(dir) = read::parent(root, artifact, budget)? {
            if let Some(meta) = metadata(&dir, parts[parts.len() - 1], budget)? {
                if !meta.is_file() {
                    return Err("lint prerequisite artifact must be a regular file".into());
                }
                return Ok(None);
            }
        }
    }
    Ok(Some(format!(
        "{crate_path}: {}; build it with `{}`",
        declared.reason, declared.command
    )))
}
fn diagnostic(label: &str, paths: &[&str]) -> String {
    format!(
        "cargo-lint {label}: {:?}{}",
        &paths[..paths.len().min(16)],
        if paths.len() > 16 {
            " (additional paths omitted)"
        } else {
            ""
        }
    )
}
pub(super) fn io_error(action: &str, e: &io::Error) -> String {
    format!(
        "lint {action}: kind={:?} errno={:?}",
        e.kind(),
        e.raw_os_error()
    )
}
