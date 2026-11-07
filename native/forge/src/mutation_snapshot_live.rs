//! Explicit live profile over the existing scanner/materializer. No mutation-policy fallback.
use super::*;
use crate::candidate_inputs::DirectoryBinding;
use conductor_native::mutation_cargo_supporting_binding::SupportingInputs;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;

#[derive(Debug)]
pub(crate) struct Allocation {
    parent: Dir,
    parent_path: PathBuf,
    name: OsString,
    path: PathBuf,
    identity: (u64, u64, u32, u32),
}
impl Allocation {
    pub(crate) fn capture(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        let parent_path = path
            .parent()
            .context("snapshot root has no parent")?
            .to_owned();
        let parent = open_absolute_directory(&parent_path)?;
        let name = path
            .file_name()
            .context("snapshot root has no name")?
            .to_owned();
        let m = parent.symlink_metadata(&name)?;
        ensure!(
            m.is_dir()
                && !m.is_symlink()
                && m.uid() == unsafe { libc::geteuid() }
                && m.mode() & 0o077 == 0,
            "unsafe allocated snapshot root"
        );
        Ok(Self {
            parent,
            parent_path,
            name,
            path,
            identity: (m.dev(), m.ino(), m.uid(), m.mode()),
        })
    }
    pub(crate) fn verify(&self, path: &Path) -> Result<()> {
        ensure!(path == self.path, "snapshot root path changed");
        let named_parent = open_absolute_directory(&self.parent_path)?.dir_metadata()?;
        let held_parent = self.parent.dir_metadata()?;
        ensure!(
            (named_parent.dev(), named_parent.ino()) == (held_parent.dev(), held_parent.ino()),
            "snapshot parent changed"
        );
        let m = self.parent.symlink_metadata(&self.name)?;
        ensure!(
            m.is_dir() && !m.is_symlink() && (m.dev(), m.ino(), m.uid(), m.mode()) == self.identity,
            "allocated snapshot root identity changed"
        );
        Ok(())
    }
    pub(crate) fn remove(&self, path: &Path) -> Result<()> {
        self.verify(path)?;
        let removal = self.parent.remove_dir_all(&self.name);
        match self.parent.symlink_metadata(&self.name) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                removal?;
                Ok(())
            }
            Err(e) => Err(e).context("cannot verify snapshot absence after removal"),
            Ok(_) => {
                self.verify(path)
                    .context("snapshot cleanup ownership changed")?;
                match removal {
                    Err(error) => anyhow::bail!("owned snapshot survived removal: {error}"),
                    Ok(()) => anyhow::bail!("owned snapshot survived removal"),
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum LiveProfile {
    Explicit,
    TrackedOnly,
}
pub(super) struct Roots(BTreeMap<String, bool>, LiveProfile);
impl Roots {
    fn capture(root: &Dir, paths: &[String], profile: LiveProfile) -> Result<Self> {
        match profile {
            LiveProfile::Explicit => ensure!(
                !paths.is_empty() && paths.len() <= 256,
                "live input roots must contain 1..256 paths"
            ),
            LiveProfile::TrackedOnly => ensure!(
                paths.is_empty(),
                "tracked-only live capture takes no extra roots"
            ),
        }
        let mut entries = BTreeMap::new();
        for path in paths {
            let normalized = if path == "." { "" } else { path.as_str() };
            if !normalized.is_empty() {
                conductor_native::mutation_snapshot_inputs::validate_path(normalized)
                    .map_err(anyhow::Error::msg)?;
            }
            ensure!(
                entries
                    .keys()
                    .all(|old: &String| !overlaps(old, normalized)),
                "live input roots overlap or repeat"
            );
            let meta = if normalized.is_empty() {
                root.dir_metadata()?
            } else {
                let (dir, name) = parent(root, normalized)?;
                dir.symlink_metadata(&name)?
            };
            ensure!(
                !meta.is_symlink() && (meta.is_dir() || meta.is_file()),
                "explicit live input root must be an existing ordinary file or directory"
            );
            entries.insert(normalized.to_owned(), meta.is_dir());
        }
        Ok(Self(entries, profile))
    }
    fn profile(&self) -> &'static str {
        match self.1 {
            LiveProfile::Explicit => "explicit-live-inputs-v1",
            LiveProfile::TrackedOnly => "tracked-live-inputs-v1",
        }
    }
    fn scope(&self) -> &'static str {
        match self.1 {
            LiveProfile::Explicit => "all HEAD/index tracked live bytes plus explicit roots and mandatory root pyproject.toml, including ignored data; no output exclusions; no original-history or execution authentication",
            LiveProfile::TrackedOnly => "HEAD/index tracked live bytes plus mandatory root pyproject.toml only; no extra untracked or ignored roots; no output exclusions; no original-history or execution authentication",
        }
    }
    pub(super) fn admits(&self, path: &str, directory: bool) -> bool {
        self.0.iter().any(|(root, is_dir)| {
            root.is_empty()
                || path == root
                || (*is_dir && descendant(path, root))
                || (directory && descendant(root, path))
        })
    }
    pub(super) fn selected_directory(&self, path: &str) -> bool {
        self.0
            .iter()
            .any(|(root, dir)| *dir && (root.is_empty() || root == path || descendant(path, root)))
    }
    fn verify(&self, root: &Dir) -> Result<()> {
        for (path, directory) in &self.0 {
            let m = if path.is_empty() {
                root.dir_metadata()?
            } else {
                let (dir, name) = parent(root, path)?;
                dir.symlink_metadata(name)?
            };
            ensure!(
                !m.is_symlink() && m.is_dir() == *directory && (m.is_dir() || m.is_file()),
                "live input root changed type: {path}"
            );
        }
        Ok(())
    }
}
fn descendant(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|suffix| suffix.starts_with('/'))
}
fn overlaps(a: &str, b: &str) -> bool {
    a.is_empty() || b.is_empty() || a == b || descendant(a, b) || descendant(b, a)
}

pub(crate) struct LiveBinding {
    host: HostBinding,
    host_root: PathBuf,
    snapshot_path: PathBuf,
    controls: super::super::ignore_rules::Inputs,
    controls_identity: Value,
    roots: Roots,
    head: Oid,
    paths: BTreeSet<String>,
    directories: BTreeSet<String>,
    identity: SupportingInputs,
    snapshot_root: DirectoryBinding,
    snapshot_git: HostBinding,
    snapshot_commit: String,
    snapshot_tree: String,
    index_sha256: String,
}
fn tracked(source: &Source, indexed: &BTreeSet<String>) -> Result<(Oid, BTreeSet<String>)> {
    let head = source.repository().head()?.peel_to_commit()?;
    let mut paths = indexed.clone();
    for entry in source.list_tree(&head.tree_id().to_string())? {
        conductor_native::mutation_snapshot_inputs::validate_path(&entry.path)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            entry.object_type == "blob"
                && matches!(entry.mode.as_str(), "100644" | "100755" | "120000"),
            "live snapshot refuses unsupported tracked modes"
        );
        paths.insert(entry.path);
    }
    ensure!(paths.len() <= MAX_FILES, "live tracked path limit exceeded");
    Ok((head.id(), paths))
}
fn git_controls(source: &Source, inputs: &mut super::super::ignore_rules::Inputs) -> Result<()> {
    for path in [
        source.repository().path().join("HEAD"),
        source.common_dir().join("packed-refs"),
        source.common_dir().join("shallow"),
        source.repository().path().join("commondir"),
        source.common_dir().join("objects/info/alternates"),
    ] {
        inputs.read(&path)?;
    }
    let mut name = "HEAD".to_owned();
    for _ in 0..32 {
        let reference = source.repository().find_reference(&name)?;
        let Some(target) = reference.symbolic_target()? else {
            return Ok(());
        };
        crate::candidate_source::safe_path(target.as_bytes())?;
        inputs.read(&source.common_dir().join(target))?;
        name = target.to_owned();
    }
    anyhow::bail!("live HEAD symbolic reference depth exceeded")
}
fn scan(
    source: &Source,
    indexed: &BTreeSet<String>,
    roots: &Roots,
    inputs: &mut super::super::ignore_rules::Inputs,
) -> Result<(Vec<String>, DirectoryStamps)> {
    let root = source.root();
    for path in [
        inputs.default_git_dir()?.join("attributes"),
        PathBuf::from("/etc/gitattributes"),
        PathBuf::from("/usr/etc/gitattributes"),
        source.common_dir().join("info/attributes"),
    ] {
        inputs.require_absent(&path)?;
    }
    let default = inputs.default_git_dir()?;
    inputs.read(&default.join("ignore"))?;
    inputs.read(&source.common_dir().join("info/exclude"))?;
    let empty = BTreeSet::new();
    let mut scanner = Scan {
        root,
        indexed,
        excluded: &[],
        extra: &empty,
        inputs,
        rules: Vec::new(),
        dirs: Vec::new(),
        paths: Vec::new(),
        count: 0,
        live_roots: Some(roots),
        live_path_bytes: 0,
    };
    scanner.walk(&open_absolute_directory(root)?, "", 0)?;
    scanner.paths.sort();
    Ok((scanner.paths, scanner.dirs))
}
fn selection() -> Result<super::super::inputs::Selection> {
    super::super::inputs::Selection::new(&[], &[])
}
struct Observation {
    head: Oid,
    paths: BTreeSet<String>,
    directories: BTreeSet<String>,
    identity: SupportingInputs,
}
fn observe(
    source: &Source,
    indexed: &BTreeSet<String>,
    roots: &Roots,
    inputs: &mut super::super::ignore_rules::Inputs,
) -> Result<Observation> {
    let (head, mut paths) = tracked(source, indexed)?;
    if inputs
        .read(&source.root().join("pyproject.toml"))?
        .is_some()
    {
        paths.insert("pyproject.toml".to_owned());
    }
    let tracked: Vec<_> = paths.iter().cloned().collect();
    tracked_attributes(source.root(), &tracked, inputs)?;
    let mut directories = BTreeSet::new();
    for path in &tracked {
        let mut directory = Path::new(path).parent();
        while let Some(dir) = directory {
            directories.insert(dir.to_owned());
            ensure!(
                directories.len() <= 16384,
                "live control directory limit exceeded"
            );
            directory = dir.parent();
        }
    }
    for dir in directories {
        inputs.read(&source.root().join(dir).join(".gitignore"))?;
    }
    let root = open_absolute_directory(source.root())?;
    roots.verify(&root)?;
    let (added, dirs) = scan(source, indexed, roots, inputs)?;
    paths.extend(added);
    ensure!(
        paths.len() <= MAX_FILES,
        "live selected path limit exceeded"
    );
    let path_bytes: usize = paths.iter().map(String::len).sum();
    ensure!(
        path_bytes <= 16 * 1024 * 1024,
        "live path bytes exceed 16 MiB"
    );
    for path in &paths {
        let (dir, name) = match parent(&root, path) {
            Ok(pair) => pair,
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                continue
            }
            Err(e) => return Err(e),
        };
        match dir.symlink_metadata(name) {
            Ok(m) => ensure!(
                (m.is_file() || m.is_symlink()) && m.nlink() == 1,
                "live input must be a single-link regular file or symlink: {path}"
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    let identity = hash_paths(&root, &paths, &selection()?, None)?;
    verify_directories(source.root(), &dirs)?;
    Ok(Observation {
        head,
        paths,
        directories: dirs.into_iter().map(|(p, _)| p).collect(),
        identity,
    })
}

pub(crate) fn populate_selected(
    repo: &Path,
    root: &Path,
    worktree: &Path,
    paths: &[String],
) -> Result<LiveBinding> {
    populate_profile(repo, root, worktree, paths, LiveProfile::Explicit)
}
pub(crate) fn populate_tracked(repo: &Path, root: &Path, worktree: &Path) -> Result<LiveBinding> {
    populate_profile(repo, root, worktree, &[], LiveProfile::TrackedOnly)
}
fn populate_profile(
    repo: &Path,
    root: &Path,
    worktree: &Path,
    paths: &[String],
    profile: LiveProfile,
) -> Result<LiveBinding> {
    let mut controls = super::super::ignore_rules::Inputs::new();
    let (source, indexed, host) = host(repo, &mut controls)?;
    git_controls(&source, &mut controls)?;
    let roots = Roots::capture(&open_absolute_directory(source.root())?, paths, profile)?;
    let Observation {
        head,
        paths,
        directories,
        identity: expected,
    } = observe(&source, &indexed, &roots, &mut controls)?;
    let all: Vec<_> = paths.iter().cloned().collect();
    let (commit, identity) = materialize(
        &source,
        root,
        worktree,
        &all,
        &[],
        head,
        Some(&selection()?),
    )?;
    ensure!(
        identity.as_ref() == Some(&expected),
        "live inputs changed during materialization"
    );
    let private = Source::open(worktree)?;
    let snapshot_tree = private.commit_tree(&commit)?;
    let snapshot_git = HostBinding::capture(&private)?;
    let snapshot_root = DirectoryBinding::repository(worktree, None)?;
    let (_, index_bytes) =
        crate::candidate_inputs::repository_live(&host.git_dir.join("index"), 32 * 1024 * 1024)?;
    let controls_identity = controls.identity()?;
    let binding = LiveBinding {
        host_root: source.root().to_owned(),
        snapshot_path: worktree.to_owned(),
        host,
        controls,
        controls_identity,
        roots,
        head,
        paths,
        directories,
        identity: expected,
        snapshot_root,
        snapshot_git,
        snapshot_commit: commit,
        snapshot_tree,
        index_sha256: format!("{:x}", Sha256::digest(index_bytes)),
    };
    binding.verify_host(repo)?;
    binding.verify_snapshot(worktree)?;
    Ok(binding)
}
fn verify_private_modes(
    source: &Source,
    root: &Dir,
    tree: &str,
    dirs: &DirectoryStamps,
) -> Result<()> {
    for entry in source.list_tree(tree)? {
        let (dir, name) = parent(root, &entry.path)?;
        let m = dir.symlink_metadata(name)?;
        ensure!(
            m.uid() == unsafe { libc::geteuid() } && m.nlink() == 1,
            "private snapshot leaf ownership or hardlink changed"
        );
        if entry.mode == "120000" {
            ensure!(m.is_symlink(), "private snapshot symlink changed type");
        } else {
            let expected = if entry.mode == "100755" { 0o755 } else { 0o644 };
            ensure!(
                m.is_file() && m.mode() & 0o7777 == expected,
                "private snapshot file permission mode changed"
            );
        }
    }
    for (path, _) in dirs {
        let m = open_directory(root, path)?.dir_metadata()?;
        ensure!(
            m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o022 == 0,
            "private snapshot directory ownership or permissions changed"
        );
    }
    Ok(())
}
impl LiveBinding {
    pub(crate) fn verify_host(&self, repo: &Path) -> Result<()> {
        let source = Source::open(repo)?;
        ensure!(
            source.root() == self.host_root.as_path(),
            "live binding belongs to another host"
        );
        self.host.verify(&source)?;
        self.controls.verify()?;
        let mut controls = super::super::ignore_rules::Inputs::new();
        let (fresh, indexed, current) = host(repo, &mut controls)?;
        git_controls(&fresh, &mut controls)?;
        let Observation {
            head,
            paths,
            directories: dirs,
            identity,
        } = observe(&fresh, &indexed, &self.roots, &mut controls)?;
        ensure!(
            head == self.head
                && paths == self.paths
                && dirs == self.directories
                && identity == self.identity,
            "live host input selection changed"
        );
        controls.verify()?;
        current.verify(&fresh)?;
        self.host.verify(&source)?;
        self.controls.verify()
    }
    pub(crate) fn verify_snapshot(&self, worktree: &Path) -> Result<()> {
        ensure!(
            worktree == self.snapshot_path.as_path(),
            "live snapshot binding path differs"
        );
        self.snapshot_root.verify()?;
        let source = Source::open(worktree)?;
        self.snapshot_git.verify(&source)?;
        ensure!(
            source
                .repository()
                .head()?
                .peel_to_commit()?
                .id()
                .to_string()
                == self.snapshot_commit
                && source.commit_tree(&self.snapshot_commit)? == self.snapshot_tree,
            "private live Git identity changed"
        );
        let empty = selection()?;
        let mut scanner = SnapshotScan {
            selection: &empty,
            paths: BTreeSet::new(),
            dirs: Vec::new(),
            entries: 0,
        };
        let root = open_absolute_directory(worktree)?;
        scanner.walk(&root, "", 0)?;
        verify_private_modes(&source, &root, &self.snapshot_tree, &scanner.dirs)?;
        let actual = hash_paths(&root, &scanner.paths, &empty, None)?;
        let mut expected_dirs = BTreeSet::from([String::new()]);
        for path in &scanner.paths {
            let mut parent = Path::new(path).parent();
            while let Some(dir) = parent {
                expected_dirs.insert(
                    dir.to_str()
                        .context("snapshot directory must be UTF8")?
                        .to_owned(),
                );
                parent = dir.parent();
            }
        }
        let actual_dirs: BTreeSet<_> = scanner.dirs.iter().map(|(p, _)| p.clone()).collect();
        ensure!(
            actual == self.identity && actual_dirs == expected_dirs,
            "private live snapshot contents or directory set changed"
        );
        verify_directories(worktree, &scanner.dirs)?;
        verify_private_modes(&source, &root, &self.snapshot_tree, &scanner.dirs)?;
        self.snapshot_git.verify(&source)?;
        self.snapshot_root.verify()
    }
    pub(crate) fn reject_output(&self, path: &Path) -> Result<()> {
        ensure!(path.is_absolute(), "live output path must be absolute");
        ensure!(
            path.components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
            "live output path is not normalized"
        );
        let canonical = match path.symlink_metadata() {
            Ok(_) => path.canonicalize()?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => path
                .parent()
                .context("output has no parent")?
                .canonicalize()?
                .join(path.file_name().context("output has no name")?),
            Err(e) => return Err(e.into()),
        };
        ensure!(canonical == path, "live output path contains a symlink");
        ensure!(
            !path.starts_with(&self.host.git_dir) && !path.starts_with(&self.host.common_dir),
            "output overlaps host Git metadata"
        );
        let host = self.host_root.as_path();
        ensure!(
            !path.starts_with(
                self.snapshot_path
                    .as_path()
                    .parent()
                    .context("snapshot root missing")?
            ),
            "output overlaps owned live snapshot scratch"
        );
        if let Ok(relative) = path.strip_prefix(host) {
            let relative = relative.to_str().context("output path must be UTF8")?;
            crate::candidate_source::safe_path(relative.as_bytes())?;
            ensure!(
                !self.roots.0.keys().any(|root| overlaps(root, relative))
                    && !self.paths.iter().any(|input| overlaps(input, relative)),
                "output overlaps selected live inputs"
            );
        }
        Ok(())
    }
    pub(crate) fn commit(&self) -> &str {
        &self.snapshot_commit
    }
    pub(crate) fn tree(&self) -> &str {
        &self.snapshot_tree
    }
    pub(crate) fn evidence(&self) -> Value {
        json!({"schema_version":1,"profile":self.roots.profile(),
            "host_root":self.host_root.as_path(),"host_head":self.head.to_string(),
            "host_index_sha256":self.index_sha256,"roots":self.roots.0,
            "selected_paths_count":self.paths.len(),"selected_directories_count":self.directories.len(),
            "host_git_dir":self.host.git_dir,"host_common_dir":self.host.common_dir,
            "snapshot_commit":self.snapshot_commit,"snapshot_tree":self.snapshot_tree,
            "inputs":self.identity,"controls_held":true,"controls":self.controls_identity,
            "scope":self.roots.scope()})
    }
}

#[cfg(test)]
#[path = "mutation_snapshot_live_tests.rs"]
mod tests;
