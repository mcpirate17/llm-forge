//! Bounded pre-commit recovery of the index and the admitted working tree.
//! Same-UID hostile replacement is outside the trusted-parent contract.
use crate::candidate_source::Source;
use anyhow::{bail, ensure, Context, Result};
use git2::{Index, IndexEntry, IndexTime, Oid, Signature};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Read,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub max_entries: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_file_bytes: 64 * 1024 * 1024,
            max_total_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}
#[derive(Debug)]
pub(crate) struct Recovery {
    pub reference: String,
    pub staged_tree: Oid,
    pub worktree_tree: Oid,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    mode: u32,
    links: u64,
    len: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}
fn stamp(m: &fs::Metadata) -> Stamp {
    Stamp {
        dev: m.dev(),
        ino: m.ino(),
        mode: m.mode(),
        links: m.nlink(),
        len: m.len(),
        mtime: m.mtime(),
        mtime_ns: m.mtime_nsec(),
        ctime: m.ctime(),
        ctime_ns: m.ctime_nsec(),
    }
}
struct Capture<'a> {
    source: &'a Source,
    limits: Limits,
    tracked: BTreeSet<String>,
    visited: usize,
    total: u64,
    stamps: BTreeMap<PathBuf, Stamp>,
    work: Index,
}
impl<'a> Capture<'a> {
    fn admit(&mut self, path: &Path, rel: &str, before: &fs::Metadata) -> Result<()> {
        let tracked = self.tracked.contains(rel);
        if !tracked
            && self
                .source
                .repository()
                .status_should_ignore(Path::new(rel))?
        {
            return Ok(());
        }
        let (mode, bytes) = if before.file_type().is_symlink() {
            let target = fs::read_link(path).context("read admitted symlink")?;
            let data = target.as_os_str().as_bytes().to_vec();
            ensure!(
                data.len() <= 4096,
                "snapshot symlink target exceeds 4096 bytes: {rel}"
            );
            (0o120000, data)
        } else if before.is_file() {
            let mut f = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(path)
                .with_context(|| format!("open snapshot path {rel}"))?;
            ensure!(
                stamp(&f.metadata()?) == stamp(before),
                "snapshot file changed before read: {rel}"
            );
            ensure!(
                before.len() <= self.limits.max_file_bytes,
                "snapshot file exceeds byte limit: {rel}"
            );
            let mut bytes = Vec::new();
            f.by_ref()
                .take(self.limits.max_file_bytes.saturating_add(1))
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 <= self.limits.max_file_bytes,
                "snapshot file grew past byte limit: {rel}"
            );
            ensure!(
                stamp(&f.metadata()?) == stamp(before),
                "snapshot file changed during read: {rel}"
            );
            let mode = if before.mode() & 0o111 != 0 {
                0o100755
            } else {
                0o100644
            };
            (mode, bytes)
        } else {
            bail!("snapshot refuses unsupported file type: {rel}");
        };
        ensure!(
            bytes.len() as u64 <= self.limits.max_file_bytes,
            "snapshot entry exceeds file byte limit: {rel}"
        );
        self.total = self
            .total
            .checked_add(bytes.len() as u64)
            .context("snapshot byte count overflow")?;
        ensure!(
            self.total <= self.limits.max_total_bytes,
            "snapshot exceeds total byte limit"
        );
        ensure!(
            stamp(&fs::symlink_metadata(path)?) == stamp(before),
            "snapshot path changed during read: {rel}"
        );
        let oid = self.source.repository().blob(&bytes)?;
        let entry = IndexEntry {
            ctime: IndexTime::new(0, 0),
            mtime: IndexTime::new(0, 0),
            dev: 0,
            ino: 0,
            mode,
            uid: 0,
            gid: 0,
            file_size: bytes.len() as u32,
            id: oid,
            flags: 0,
            flags_extended: 0,
            path: rel.as_bytes().to_vec(),
        };
        self.work.add(&entry)?;
        self.stamps.insert(path.to_owned(), stamp(before));
        Ok(())
    }
    fn walk(&mut self, directory: &Path, prefix: &str) -> Result<()> {
        let before = fs::symlink_metadata(directory)?;
        ensure!(
            before.is_dir() && !before.file_type().is_symlink(),
            "snapshot directory changed or is a symlink: {}",
            directory.display()
        );
        self.stamps.insert(directory.to_owned(), stamp(&before));
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            self.visited = self
                .visited
                .checked_add(1)
                .context("snapshot entry count overflow")?;
            ensure!(
                self.visited <= self.limits.max_entries,
                "snapshot entry limit exceeded (includes ignored entries)"
            );
            let name = entry.file_name();
            let name = name.to_str().context("snapshot refuses non-UTF-8 paths")?;
            if prefix.is_empty() && name == ".git" {
                continue;
            }
            ensure!(
                name != ".git",
                "snapshot refuses nested Git metadata or submodule"
            );
            let rel = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            crate::candidate_source::safe_path(rel.as_bytes())?;
            ensure!(
                rel.split('/').count() <= 256,
                "snapshot path exceeds 256 components"
            );
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                let prefix = format!("{rel}/");
                let has_tracked = self
                    .tracked
                    .range(prefix.clone()..)
                    .next()
                    .is_some_and(|tracked| tracked.starts_with(&prefix));
                if !has_tracked
                    && self
                        .source
                        .repository()
                        .status_should_ignore(Path::new(&prefix))?
                {
                    continue;
                }
                self.walk(&path, &rel)?;
            } else {
                self.admit(&path, &rel, &metadata)?;
            }
        }
        ensure!(
            stamp(&fs::symlink_metadata(directory)?) == stamp(&before),
            "snapshot directory changed during capture: {}",
            directory.display()
        );
        Ok(())
    }
    fn verify(&self) -> Result<()> {
        for (path, expected) in &self.stamps {
            ensure!(
                stamp(&fs::symlink_metadata(path)?) == *expected,
                "snapshot path drifted: {}",
                path.display()
            );
        }
        Ok(())
    }
}
#[derive(Debug, PartialEq, Eq)]
enum IndexObservation {
    Present(Vec<u8>),
    Absent { parent: PathBuf, stamp: Stamp },
}
fn index_path(source: &Source) -> Result<PathBuf> {
    let path = match std::env::var_os("GIT_INDEX_FILE") {
        Some(raw) => {
            let path = PathBuf::from(raw);
            if path.is_absolute() {
                path
            } else {
                source.root().join(path)
            }
        }
        None => source.repository().path().join("index"),
    };
    ensure!(
        path.as_os_str().as_bytes().len() <= 4096 && path.file_name().is_some(),
        "invalid snapshot index path"
    );
    Ok(path)
}
fn absent_index(path: &Path) -> Result<(Index, IndexObservation)> {
    let parent = path.parent().context("index path has no parent")?;
    let canonical = parent
        .canonicalize()
        .context("absent index parent must exist")?;
    let before = fs::metadata(parent)?;
    ensure!(before.is_dir(), "absent index parent must be a directory");
    let missing = fs::symlink_metadata(path);
    ensure!(
        matches!(missing, Err(ref error) if error.kind() == std::io::ErrorKind::NotFound),
        "index appeared while checking absence"
    );
    ensure!(
        parent.canonicalize()? == canonical && stamp(&fs::metadata(parent)?) == stamp(&before),
        "index parent changed while checking absence"
    );
    Ok((
        Index::new()?,
        IndexObservation::Absent {
            parent: canonical,
            stamp: stamp(&before),
        },
    ))
}
fn index_state(source: &Source) -> Result<(Index, IndexObservation)> {
    let path = index_path(source)?;
    let before = match fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return absent_index(&path),
        Err(error) => return Err(error).context("inspect snapshot index"),
    };
    let raw = crate::bounded_file::read(&path, 64 * 1024 * 1024)?;
    ensure!(
        stamp(&before) == stamp(&fs::symlink_metadata(&path)?),
        "index changed during snapshot read"
    );
    let hash = Sha256::digest(&raw).to_vec();
    let index = crate::candidate_source::parse_index(&raw)?;
    for entry in index.iter() {
        ensure!(
            entry.flags & 0xf000 == 0 && entry.flags_extended == 0,
            "snapshot refuses staged index flags, including sparse or assume-unchanged"
        );
    }
    Ok((index, IndexObservation::Present(hash)))
}
#[derive(Debug, PartialEq, Eq)]
enum HeadObservation {
    Unborn(String),
    Born {
        symbolic: Option<String>,
        commit: Oid,
    },
}
fn head_state(source: &Source) -> Result<HeadObservation> {
    let head = source
        .repository()
        .find_reference("HEAD")
        .context("inspect HEAD")?;
    let symbolic = head.symbolic_target()?.map(str::to_owned);
    if let Some(ref name) = symbolic {
        ensure!(
            name.starts_with("refs/heads/") && git2::Reference::is_valid_name(name),
            "invalid symbolic HEAD target"
        );
        match source.repository().find_reference(name) {
            Ok(reference) => {
                return Ok(HeadObservation::Born {
                    symbolic: Some(name.clone()),
                    commit: reference.peel_to_commit()?.id(),
                })
            }
            Err(error) if error.code() == git2::ErrorCode::NotFound => {
                return Ok(HeadObservation::Unborn(name.clone()))
            }
            Err(error) => return Err(error).context("inspect symbolic HEAD target"),
        }
    }
    Ok(HeadObservation::Born {
        symbolic: None,
        commit: head.peel_to_commit()?.id(),
    })
}
fn verify_binding(source: &Source, head: &HeadObservation, index: &IndexObservation) -> Result<()> {
    ensure!(&head_state(source)? == head, "HEAD changed during snapshot");
    ensure!(
        &index_state(source)?.1 == index,
        "staged index changed during snapshot"
    );
    Ok(())
}
fn tracked(source: &Source, trees: &[String], limits: Limits) -> Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    let mut staged_total = 0u64;
    for (i, tree) in trees.iter().enumerate() {
        for entry in source.list_tree(tree)? {
            ensure!(
                entry.mode != "160000",
                "snapshot refuses Git submodule: {}",
                entry.path
            );
            ensure!(
                matches!(entry.mode.as_str(), "100644" | "100755" | "120000"),
                "snapshot refuses unsupported tracked mode: {}",
                entry.path
            );
            if i == 1 {
                let size = entry.size.context("snapshot staged blob has no size")?;
                ensure!(
                    size <= limits.max_file_bytes,
                    "staged blob exceeds file byte limit: {}",
                    entry.path
                );
                staged_total = staged_total
                    .checked_add(size)
                    .context("staged snapshot byte overflow")?;
                ensure!(
                    staged_total <= limits.max_total_bytes,
                    "staged tree exceeds snapshot byte limit"
                );
            }
            result.insert(entry.path);
            ensure!(
                result.len() <= limits.max_entries,
                "snapshot tracked path limit exceeded"
            );
        }
    }
    Ok(result)
}
fn commit_tree(
    source: &Source,
    tree: Oid,
    parents: &[&git2::Commit<'_>],
    message: &str,
) -> Result<Oid> {
    let sig = Signature::now("Forge snapshot", "forge-snapshot@localhost")?;
    let tree = source.repository().find_tree(tree)?;
    Ok(source
        .repository()
        .commit(None, &sig, &sig, message, &tree, parents)?)
}
/// Check bounded index shape and unsupported staged flags without writing it.
pub(crate) fn validate_checkout_index(source: &Source) -> Result<()> {
    let _ = index_state(source)?;
    Ok(())
}
/// Captures staged and worktree trees, then creates one non-overwriting recovery ref.
/// The staged tree is REF^1^{tree}; the worktree tree is REF^2^{tree}.
pub(crate) fn create(source: &Source, limits: Limits) -> Result<Recovery> {
    create_inner(source, limits, || Ok(()))
}
fn create_inner(
    source: &Source,
    limits: Limits,
    on_captured: impl FnOnce() -> Result<()>,
) -> Result<Recovery> {
    create_named_inner(source, limits, None, on_captured, |_, _| Ok(()))
}
/// Capture with a caller-selected recovery ref. The callback must durably
/// journal the exact name and commit immediately before ref publication.
pub(crate) fn create_named(
    source: &Source,
    limits: Limits,
    reference: &str,
    before_reference: impl FnOnce(&str, Oid) -> Result<()>,
) -> Result<Recovery> {
    ensure!(
        reference.starts_with("refs/snapshots/") && git2::Reference::is_valid_name(reference),
        "invalid snapshot ref"
    );
    create_named_inner(source, limits, Some(reference), || Ok(()), before_reference)
}
fn create_named_inner(
    source: &Source,
    limits: Limits,
    selected_ref: Option<&str>,
    on_captured: impl FnOnce() -> Result<()>,
    before_reference: impl FnOnce(&str, Oid) -> Result<()>,
) -> Result<Recovery> {
    ensure!(
        limits.max_entries > 0 && limits.max_entries <= 100_000,
        "invalid snapshot entry limit"
    );
    ensure!(
        limits.max_file_bytes > 0
            && limits.max_file_bytes <= 64 * 1024 * 1024
            && limits.max_total_bytes > 0
            && limits.max_total_bytes <= 2 * 1024 * 1024 * 1024,
        "invalid snapshot byte limits"
    );
    let head_state = head_state(source)?;
    let head = match &head_state {
        HeadObservation::Born { commit, .. } => Some(source.repository().find_commit(*commit)?),
        HeadObservation::Unborn(_) => None,
    };
    let (mut index, index_observation) = index_state(source)?;
    let staged_oid = index.write_tree_to(source.repository())?;
    let staged = staged_oid.to_string();
    let base_tree = head
        .as_ref()
        .map(|commit| commit.tree_id().to_string())
        .unwrap_or_else(|| "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_owned());
    let paths = tracked(source, &[base_tree, staged.clone()], limits)?;
    let mut capture = Capture {
        source,
        limits,
        tracked: paths,
        visited: 0,
        total: 0,
        stamps: BTreeMap::new(),
        work: Index::new()?,
    };
    capture.walk(source.root(), "")?;
    on_captured()?;
    capture.verify()?;
    verify_binding(source, &head_state, &index_observation)?;
    let worktree_oid = capture.work.write_tree_to(source.repository())?;
    let tree_limits = crate::candidate_snapshot::Limits {
        max_blob_bytes: limits.max_file_bytes,
        max_tree_bytes: limits.max_total_bytes,
    };
    crate::candidate_snapshot::validate_recovery_tree(source, &staged, tree_limits)?;
    crate::candidate_snapshot::validate_recovery_tree(
        source,
        &worktree_oid.to_string(),
        tree_limits,
    )?;
    let parents: Vec<_> = head.iter().collect();
    let staged_commit = commit_tree(source, staged_oid, &parents, "Forge staged index recovery")?;
    let work_commit = commit_tree(
        source,
        worktree_oid,
        &parents,
        "Forge working tree recovery",
    )?;
    let staged_parent = source.repository().find_commit(staged_commit)?;
    let work_parent = source.repository().find_commit(work_commit)?;
    let merge = commit_tree(
        source,
        worktree_oid,
        &[&staged_parent, &work_parent],
        "Forge recovery snapshot",
    )?;
    capture.verify()?;
    verify_binding(source, &head_state, &index_observation)?;
    let reference = match selected_ref {
        Some(reference) => reference.to_owned(),
        None => format!(
            "refs/snapshots/forge/{}",
            crate::native_id::new("snapshot")?
        ),
    };
    before_reference(&reference, merge)?;
    source
        .repository()
        .reference(&reference, merge, false, "Forge recovery snapshot")?;
    Ok(Recovery {
        reference,
        staged_tree: staged_oid,
        worktree_tree: worktree_oid,
    })
}
#[cfg(test)]
#[path = "checkout_sync_snapshot_tests.rs"]
mod checkout_sync_tests;
#[cfg(test)]
#[path = "candidate_commit_snapshot_tests.rs"]
mod tests;
