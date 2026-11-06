//! Raw Git-object snapshot preparation; never runs Git or checkout filters.
use super::{configured_extra_suffixes, excluded_prefixes, fixture_path, SOURCE_SUFFIXES};
use crate::{candidate_snapshot, candidate_source::Source};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, MetadataExt, OpenOptions, OpenOptionsExt};
use git2::{
    Config, Index, IndexEntry, IndexTime, Oid, Repository, RepositoryInitOptions, Signature,
};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

const MAX_FILES: usize = 100_000;
const MAX_BLOB: u64 = 32 * 1024 * 1024;
const MAX_TOTAL: u64 = 2 * 1024 * 1024 * 1024;

fn config_string(config: &Config, key: &str) -> Result<Option<String>> {
    match config.get_string(key) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.code() == git2::ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn check_config(config: &Config) -> Result<()> {
    for key in ["core.attributesfile", "core.excludesfile"] {
        ensure!(
            config_string(config, key)?.is_none(),
            "configured {key} is unsupported for raw snapshots"
        );
    }
    for key in ["core.autocrlf", "core.safecrlf"] {
        if let Some(value) = config_string(config, key)? {
            ensure!(
                value.eq_ignore_ascii_case("false"),
                "configured {key} requires unsupported conversion"
            );
        }
    }
    for (key, allowed) in [
        ("core.sparsecheckout", false),
        ("core.filemode", true),
        ("core.symlinks", true),
        ("core.ignorecase", false),
    ] {
        if config_string(config, key)?.is_some() {
            ensure!(
                config.get_bool(key)? == allowed,
                "configured {key} is unsupported"
            );
        }
    }
    Ok(())
}
fn has_include_header(text: &str) -> bool {
    text.lines().any(|line| {
        let Some(header) = line
            .trim_start_matches('\u{feff}')
            .trim_start()
            .strip_prefix('[')
        else {
            return false;
        };
        let section = header
            .trim_start()
            .split(|ch: char| ch.is_whitespace() || matches!(ch, ']' | '.'))
            .next()
            .unwrap_or("");
        section.eq_ignore_ascii_case("include") || section.eq_ignore_ascii_case("includeif")
    })
}
pub(super) fn bounded_config(path: &Path, inputs: &mut super::ignore_rules::Inputs) -> Result<()> {
    let Some(bytes) = inputs.read(path)? else {
        return Ok(());
    };
    let text = std::str::from_utf8(&bytes).context("Git config must be UTF-8")?;
    ensure!(
        !has_include_header(text),
        "Git config includes are unsupported"
    );
    let scratch = tempfile::tempdir()?;
    let private = scratch.path().join("config");
    std::fs::write(&private, bytes)?;
    let config = Config::open(&private)?;
    check_config(&config)
}
enum GitEntryBinding {
    Directory(crate::candidate_inputs::DirectoryBinding),
    File(crate::candidate_inputs::BoundFile),
}
struct HostBinding {
    root: crate::candidate_inputs::DirectoryBinding,
    index: crate::candidate_inputs::BoundFile,
    git_entry: GitEntryBinding,
    git_dir: PathBuf,
    common_dir: PathBuf,
}
impl HostBinding {
    fn capture(source: &Source) -> Result<Self> {
        let root_binding =
            crate::candidate_inputs::DirectoryBinding::repository(source.root(), None)?;
        let root = open_absolute_directory(source.root())?;
        let meta = root.symlink_metadata(".git")?;
        let path = source.root().join(".git");
        let git_entry = if meta.is_dir() && !meta.is_symlink() {
            GitEntryBinding::Directory(crate::candidate_inputs::DirectoryBinding::repository(
                &path, None,
            )?)
        } else if meta.is_file() && !meta.is_symlink() {
            GitEntryBinding::File(crate::candidate_inputs::repository_live(&path, 65536)?.0)
        } else {
            anyhow::bail!("Git entry must be an ordinary file or directory");
        };
        let git_dir = source.repository().path().canonicalize()?;
        let index =
            crate::candidate_inputs::repository_live(&git_dir.join("index"), 32 * 1024 * 1024)?.0;
        let binding = Self {
            root: root_binding,
            index,
            git_entry,
            git_dir,
            common_dir: source.common_dir().to_owned(),
        };
        binding.verify(source)?;
        Ok(binding)
    }
    fn verify(&self, source: &Source) -> Result<()> {
        self.root.verify()?;
        self.index.verify()?;
        match &self.git_entry {
            GitEntryBinding::Directory(dir) => dir.verify()?,
            GitEntryBinding::File(file) => file.verify()?,
        }
        let fresh = Source::open(source.root())?;
        ensure!(
            fresh.repository().path().canonicalize()? == self.git_dir
                && fresh.common_dir() == self.common_dir,
            "named Git repository changed"
        );
        Ok(())
    }
}
fn reject_workspace_path_overrides() -> Result<()> {
    for name in [
        "CONDUCTOR_MUTATION_REGISTRY",
        "CONDUCTOR_MUTATION_RECEIPT_ROOT",
        "CONDUCTOR_NOTES_ROOT",
    ] {
        ensure!(
            std::env::var_os(name).is_none(),
            "workspace snapshot refuses ambient path override {name}"
        );
    }
    Ok(())
}
fn host(
    root: &Path,
    inputs: &mut super::ignore_rules::Inputs,
) -> Result<(Source, BTreeSet<String>, HostBinding)> {
    inputs.admit_git_environment()?;
    let source = Source::open(root)?;
    let repo = source.repository();
    let binding = HostBinding::capture(&source)?;
    if !inputs.config_disabled("GIT_CONFIG_NOSYSTEM")
        && !inputs.config_disabled("GIT_CONFIG_SYSTEM")
    {
        bounded_config(Path::new("/etc/gitconfig"), inputs)?;
        bounded_config(Path::new("/usr/etc/gitconfig"), inputs)?;
    }
    if !inputs.config_disabled("GIT_CONFIG_GLOBAL") {
        bounded_config(&inputs.default_git_dir()?.join("config"), inputs)?;
        bounded_config(&inputs.home()?.join(".gitconfig"), inputs)?;
    }
    bounded_config(&source.common_dir().join("config"), inputs)?;
    // A linked-worktree config is admitted only as bounded, separately held data.
    bounded_config(&binding.git_dir.join("config.worktree"), inputs)?;
    // The in-memory empty stack prevents ambient global/system config from
    // selecting filters or ignore sources; working-tree .gitignore remains data.
    repo.set_config(&Config::new()?)?;
    let index = repo.index()?;
    ensure!(
        !index.has_conflicts(),
        "conflicted host index is unsupported"
    );
    let mut indexed = BTreeSet::new();
    for entry in index.iter() {
        let extended = git2::IndexEntryExtendedFlag::from_bits_truncate(entry.flags_extended);
        ensure!(
            !extended.is_skip_worktree() && !extended.is_intent_to_add(),
            "sparse or intent-to-add index entries are unsupported"
        );
        let path = std::str::from_utf8(&entry.path).context("index path must be UTF-8")?;
        crate::candidate_source::safe_path(path.as_bytes())?;
        ensure!(
            !path
                .split('/')
                .any(|part| part.eq_ignore_ascii_case(".git")),
            "indexed Git metadata path is unsupported"
        );
        ensure!(
            Path::new(path)
                .file_name()
                .is_none_or(|name| name != ".gitattributes"),
            "indexed .gitattributes is unsupported"
        );
        ensure!(indexed.len() < MAX_FILES, "host index exceeds entry budget");
        indexed.insert(path.to_owned());
    }
    binding.verify(&source)?;
    Ok((source, indexed, binding))
}
fn permitted(path: &str, excluded: &[String], extra: &BTreeSet<String>) -> bool {
    if excluded.iter().any(|prefix| path.starts_with(prefix)) {
        return false;
    }
    let suffix = Path::new(path)
        .extension()
        .map(|x| format!(".{}", x.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    fixture_path(path) || SOURCE_SUFFIXES.contains(&suffix.as_str()) || extra.contains(&suffix)
}
type Stamp = (u64, u64, i64, i64, i64, i64);
type DirectoryStamps = Vec<(String, Stamp)>;
fn stamp(meta: &cap_std::fs::Metadata) -> Stamp {
    (
        meta.dev(),
        meta.ino(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}
fn open_absolute_directory(path: &Path) -> Result<Dir> {
    let mut dir = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    let mut parts = path.components();
    ensure!(
        matches!(parts.next(), Some(Component::RootDir)),
        "snapshot root must be absolute"
    );
    for part in parts {
        let Component::Normal(name) = part else {
            anyhow::bail!("snapshot root must be canonical");
        };
        dir = Dir::from_std_file(
            dir.open_with(
                name,
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW),
            )?
            .into_std(),
        );
    }
    Ok(dir)
}
fn open_directory(root: &Dir, path: &str) -> Result<Dir> {
    if path.is_empty() {
        return Ok(root.try_clone()?);
    }
    let (parent, name) = parent(root, path)?;
    Ok(Dir::from_std_file(
        parent
            .open_with(
                &name,
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW),
            )?
            .into_std(),
    ))
}
struct Scan<'a> {
    root: &'a Path,
    indexed: &'a BTreeSet<String>,
    excluded: &'a [String],
    extra: &'a BTreeSet<String>,
    inputs: &'a mut super::ignore_rules::Inputs,
    rules: Vec<ignore::gitignore::Gitignore>,
    dirs: Vec<(String, Stamp)>,
    paths: Vec<String>,
    count: usize,
}
impl Scan<'_> {
    fn walk(&mut self, dir: &Dir, prefix: &str, depth: usize) -> Result<()> {
        ensure!(depth <= 256, "worktree depth exceeds 256");
        let before = dir.dir_metadata()?;
        let named = self.root.join(prefix);
        self.inputs.require_absent(&named.join(".gitattributes"))?;
        let rules = self.inputs.read(&named.join(".gitignore"))?;
        self.rules
            .push(super::ignore_rules::matcher(&named, rules)?);
        for item in dir.entries()? {
            self.count += 1;
            ensure!(
                self.count <= MAX_FILES,
                "worktree scan exceeds 100000 entries"
            );
            let item = item?;
            let name = item.file_name();
            if name.as_encoded_bytes().eq_ignore_ascii_case(b".git") {
                ensure!(
                    prefix.is_empty() && name == ".git",
                    "nested or ambiguous Git metadata is unsupported"
                );
                continue;
            }
            let component = name.to_str().context("worktree path must be UTF-8")?;
            let path = if prefix.is_empty() {
                component.to_owned()
            } else {
                format!("{prefix}/{component}")
            };
            crate::candidate_source::safe_path(path.as_bytes())?;
            let meta = dir.symlink_metadata(&name)?;
            let is_dir = meta.is_dir() && !meta.is_symlink();
            if super::ignore_rules::ignored(&self.rules, &self.root.join(&path), is_dir) {
                continue;
            }
            if is_dir {
                let child = Dir::from_std_file(
                    dir.open_with(
                        &name,
                        OpenOptions::new()
                            .read(true)
                            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW),
                    )?
                    .into_std(),
                );
                let opened = child.dir_metadata()?;
                ensure!(
                    meta.dev() == opened.dev() && meta.ino() == opened.ino(),
                    "worktree directory replaced: {path}"
                );
                self.walk(&child, &path, depth + 1)?;
            } else if !self.indexed.contains(&path) && permitted(&path, self.excluded, self.extra) {
                self.paths.push(path);
            }
        }
        self.rules.pop();
        let after = dir.dir_metadata()?;
        ensure!(
            stamp(&before) == stamp(&after),
            "worktree directory changed while scanning: {prefix}"
        );
        self.dirs.push((prefix.to_owned(), stamp(&after)));
        Ok(())
    }
}
fn enumerate(
    source: &Source,
    indexed: &BTreeSet<String>,
    excluded: &[String],
    extra: &BTreeSet<String>,
    inputs: &mut super::ignore_rules::Inputs,
) -> Result<(Vec<String>, DirectoryStamps)> {
    let root = source.root();
    let default = inputs.default_git_dir()?;
    inputs.require_absent(&default.join("attributes"))?;
    inputs.require_absent(Path::new("/etc/gitattributes"))?;
    inputs.require_absent(Path::new("/usr/etc/gitattributes"))?;
    inputs.require_absent(&source.common_dir().join("info/attributes"))?;
    let global = inputs.read(&default.join("ignore"))?;
    let info = inputs.read(&source.common_dir().join("info/exclude"))?;
    let rules = vec![
        super::ignore_rules::matcher(root, global)?,
        super::ignore_rules::matcher(root, info)?,
    ];
    let root_dir = open_absolute_directory(root)?;
    let mut scan = Scan {
        root,
        indexed,
        excluded,
        extra,
        inputs,
        rules,
        dirs: Vec::new(),
        paths: Vec::new(),
        count: 0,
    };
    scan.walk(&root_dir, "", 0)?;
    scan.paths.sort();
    Ok((scan.paths, scan.dirs))
}
fn verify_directories(root: &Path, dirs: &[(String, Stamp)]) -> Result<()> {
    let root_dir = open_absolute_directory(root)?;
    for (path, expected) in dirs {
        let directory = open_directory(&root_dir, path)?;
        ensure!(
            stamp(&directory.dir_metadata()?) == *expected,
            "worktree directory changed: {path}"
        );
    }
    Ok(())
}
fn tracked_attributes(
    root: &Path,
    paths: &[String],
    inputs: &mut super::ignore_rules::Inputs,
) -> Result<()> {
    let mut dirs = BTreeSet::new();
    for path in paths {
        let mut dir = Path::new(path).parent();
        while let Some(current) = dir {
            dirs.insert(current.to_path_buf());
            dir = current.parent();
        }
    }
    for dir in dirs {
        inputs.require_absent(&root.join(dir).join(".gitattributes"))?;
    }
    Ok(())
}
pub(super) fn untracked(root: &Path) -> Result<Vec<String>, String> {
    (|| -> Result<Vec<String>> {
        let mut inputs = super::ignore_rules::Inputs::new();
        let (source, indexed, index_binding) = host(root, &mut inputs)?;
        let (paths, dirs) = enumerate(
            &source,
            &indexed,
            &excluded_prefixes(source.root()).map_err(anyhow::Error::msg)?,
            &configured_extra_suffixes(source.root()).map_err(anyhow::Error::msg)?,
            &mut inputs,
        )?;
        inputs.verify()?;
        verify_directories(source.root(), &dirs)?;
        index_binding.verify(&source)?;
        Ok(paths)
    })()
    .map_err(|error| format!("snapshot untracked inventory: {error:#}"))
}
fn parent(root: &Dir, path: &str) -> Result<(Dir, String)> {
    crate::candidate_source::safe_path(path.as_bytes())?;
    let mut dir = root.try_clone()?;
    let mut pieces = Path::new(path).components().peekable();
    while let Some(component) = pieces.next() {
        let Component::Normal(name) = component else {
            anyhow::bail!("unsafe snapshot path")
        };
        if pieces.peek().is_none() {
            return Ok((dir, name.to_str().context("non-UTF-8 filename")?.into()));
        }
        dir = Dir::from_std_file(
            dir.open_with(
                name,
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW),
            )?
            .into_std(),
        );
    }
    anyhow::bail!("empty snapshot path")
}
fn capture(root: &Dir, path: &str) -> Result<Option<(u32, Vec<u8>)>> {
    let (dir, name) = match parent(root, path) {
        Ok(value) => value,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    let before = match dir.symlink_metadata(&name) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let (mode, bytes) = if before.is_symlink() {
        (
            0o120000,
            dir.read_link_contents(&name)?
                .as_os_str()
                .as_encoded_bytes()
                .to_vec(),
        )
    } else if before.is_file() {
        ensure!(
            before.len() <= MAX_BLOB,
            "snapshot file exceeds 32 MiB: {path}"
        );
        let mut opened = dir.open_with(
            &name,
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK),
        )?;
        let actual = opened.metadata()?;
        ensure!(
            actual.is_file() && actual.dev() == before.dev() && actual.ino() == before.ino(),
            "snapshot input replaced: {path}"
        );
        let mut bytes = Vec::new();
        opened.by_ref().take(MAX_BLOB + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_BLOB,
            "snapshot file exceeds 32 MiB: {path}"
        );
        (
            if before.mode() & 0o111 != 0 {
                0o100755
            } else {
                0o100644
            },
            bytes,
        )
    } else {
        anyhow::bail!("snapshot input is not a regular file or symlink: {path}");
    };
    let after = dir.symlink_metadata(&name)?;
    ensure!(
        before.dev() == after.dev()
            && before.ino() == after.ino()
            && before.len() == after.len()
            && before.mtime() == after.mtime()
            && before.mtime_nsec() == after.mtime_nsec()
            && before.ctime() == after.ctime()
            && before.ctime_nsec() == after.ctime_nsec(),
        "snapshot input changed while reading: {path}"
    );
    Ok(Some((mode, bytes)))
}
fn add(repo: &Repository, index: &mut Index, path: &str, mode: u32, bytes: &[u8]) -> Result<()> {
    index.add(&IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode,
        uid: 0,
        gid: 0,
        file_size: bytes.len().try_into()?,
        id: repo.blob(bytes)?,
        flags: 0,
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    })?;
    Ok(())
}
fn prepare_private(root: &Path, source: &Source) -> Result<Repository> {
    let private = root.join("git-source");
    let mut options = RepositoryInitOptions::new();
    options.mkdir(true).external_template(false).no_reinit(true);
    let repo = Repository::init_opts(&private, &options)?;
    let objects = source.common_dir().join("objects").canonicalize()?;
    let text = objects.to_str().context("Git object path must be UTF-8")?;
    ensure!(
        !text.contains(['\n', '\r']),
        "Git object path contains a newline"
    );
    std::fs::write(
        private.join(".git/objects/info/alternates"),
        format!("{text}\n"),
    )?;
    repo.odb()?.add_disk_alternate(text)?;
    Ok(repo)
}
fn materialize(
    source: &Source,
    root: &Path,
    worktree: &Path,
    paths: &[String],
    untracked: &[String],
    parent_oid: Oid,
    selection: Option<&super::inputs::Selection>,
) -> Result<(
    String,
    Option<conductor_native::mutation_cargo_supporting_binding::SupportingInputs>,
)> {
    let repo = prepare_private(root, source)?;
    let parent = repo.find_commit(parent_oid)?;
    let root_dir = open_absolute_directory(source.root())?;
    if let Some(selection) = selection {
        validate_outputs(&root_dir, selection)?;
    }
    let mut index = Index::new()?;
    let mut total = 0u64;
    let mut selected: Vec<_> = paths.iter().chain(untracked).collect();
    selected.sort();
    selected.dedup();
    let mut identity = selection
        .map(|s| conductor_native::mutation_snapshot_inputs::Builder::new(s.excluded()))
        .transpose()
        .map_err(anyhow::Error::msg)?;
    for path in selected {
        if selection.is_some_and(|s| s.contains(path)) {
            ensure!(
                !super::inputs::forbidden_output_input(path),
                "workspace output namespace hides Cargo control or Rust source: {path}"
            );
            continue;
        }
        if let Some((mode, bytes)) = capture(&root_dir, path)? {
            if let Some(builder) = identity.as_mut() {
                builder
                    .add(path, mode, &bytes)
                    .map_err(anyhow::Error::msg)?;
            }
            total = total
                .checked_add(bytes.len() as u64)
                .context("snapshot byte count overflow")?;
            ensure!(total <= MAX_TOTAL, "snapshot exceeds 2 GiB");
            add(&repo, &mut index, path, mode, &bytes)?;
        }
    }
    let tree_oid = index.write_tree_to(&repo)?;
    let tree = repo.find_tree(tree_oid)?;
    let sig = Signature::now("Audit Orchestrator Snapshot", "orchestrator@localhost")?;
    let commit = repo.commit(None, &sig, &sig, "orchestrator snapshot", &tree, &[&parent])?;
    repo.set_head_detached(commit)?;
    let mut disk_index = repo.index()?;
    disk_index.read_tree(&tree)?;
    disk_index.write()?;
    let private_source = Source::open(&root.join("git-source"))?;
    let snapshot = candidate_snapshot::Snapshot::create(
        &private_source,
        &tree_oid.to_string(),
        worktree,
        candidate_snapshot::Limits::default(),
    )?;
    ensure!(
        snapshot.report().skipped_gitlinks == 0,
        "snapshot refuses gitlinks"
    );
    snapshot.publish()?;
    std::fs::rename(root.join("git-source/.git"), worktree.join(".git"))?;
    std::fs::remove_dir(root.join("git-source"))?;
    if let Some(selection) = selection {
        validate_outputs(&root_dir, selection)?;
    }
    Ok((
        commit.to_string(),
        identity
            .map(|builder| builder.finish())
            .transpose()
            .map_err(anyhow::Error::msg)?,
    ))
}
pub(super) fn populate(
    repo: &Path,
    root: &Path,
    worktree: &Path,
) -> Result<(String, Vec<String>), String> {
    let (commit, paths, _) = populate_inner(repo, root, worktree, None)?;
    Ok((commit, paths))
}

pub(super) fn populate_bound(
    repo: &Path,
    root: &Path,
    worktree: &Path,
    selection: &super::inputs::Selection,
) -> Result<
    (
        String,
        Vec<String>,
        conductor_native::mutation_cargo_supporting_binding::SupportingInputs,
    ),
    String,
> {
    let (commit, paths, identity) = populate_inner(repo, root, worktree, Some(selection))?;
    Ok((commit, paths, identity.ok_or("missing snapshot identity")?))
}

fn populate_inner(
    repo: &Path,
    root: &Path,
    worktree: &Path,
    selection: Option<&super::inputs::Selection>,
) -> Result<
    (
        String,
        Vec<String>,
        Option<conductor_native::mutation_cargo_supporting_binding::SupportingInputs>,
    ),
    String,
> {
    (|| -> Result<_> {
        if selection.is_some() {
            reject_workspace_path_overrides()?;
        }
        let mut inputs = super::ignore_rules::Inputs::new();
        let (source, indexed, index_binding) = host(repo, &mut inputs)?;
        let head = source.repository().head()?.peel_to_commit()?;
        let parent_oid = head.id();
        let entries = source.list_tree(&head.tree_id().to_string())?;
        let mut paths = Vec::new();
        for entry in entries {
            ensure!(
                !entry
                    .path
                    .split('/')
                    .any(|part| part.eq_ignore_ascii_case(".git")),
                "tracked Git metadata path is unsupported"
            );
            ensure!(
                Path::new(&entry.path)
                    .file_name()
                    .is_none_or(|name| name != ".gitattributes"),
                "tracked .gitattributes requires unsupported Git conversion analysis"
            );
            ensure!(
                entry.object_type == "blob"
                    && matches!(entry.mode.as_str(), "100644" | "100755" | "120000"),
                "snapshot refuses gitlinks or unsupported tracked modes: {}",
                entry.path
            );
            paths.push(entry.path);
        }
        if selection.is_some() {
            let mut union: BTreeSet<String> = paths.into_iter().collect();
            union.extend(indexed.iter().cloned());
            paths = union.into_iter().collect();
        }
        tracked_attributes(source.root(), &paths, &mut inputs)?;
        let (untracked, dirs) = enumerate(
            &source,
            &indexed,
            &excluded_prefixes(source.root()).map_err(anyhow::Error::msg)?,
            &configured_extra_suffixes(source.root()).map_err(anyhow::Error::msg)?,
            &mut inputs,
        )?;
        // A staged deletion can make a HEAD path appear untracked; add it only once.
        let tracked: BTreeSet<_> = paths.iter().map(String::as_str).collect();
        let added: Vec<_> = untracked
            .iter()
            .filter(|path| !tracked.contains(path.as_str()))
            .cloned()
            .collect();
        let (commit, identity) = materialize(
            &source, root, worktree, &paths, &added, parent_oid, selection,
        )?;
        inputs.verify()?;
        if selection.is_some() {
            reject_workspace_path_overrides()?;
        }
        verify_directories(source.root(), &dirs)?;
        index_binding.verify(&source)?;
        ensure!(
            source.repository().head()?.peel_to_commit()?.id() == parent_oid,
            "host HEAD changed during snapshot capture"
        );
        Ok((commit, untracked, identity))
    })()
    .map_err(|error| format!("snapshot preparation: {error:#}"))
}

/// Recompute the snapshot input identity from the same live tracked/untracked rules.
pub(super) fn live_identity(
    repo: &Path,
    selection: &super::inputs::Selection,
) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs> {
    reject_workspace_path_overrides()?;
    let mut inputs = super::ignore_rules::Inputs::new();
    let (source, indexed, binding) = host(repo, &mut inputs)?;
    let head = source.repository().head()?.peel_to_commit()?;
    let parent_oid = head.id();
    let entries = source.list_tree(&head.tree_id().to_string())?;
    let mut paths = BTreeSet::new();
    for entry in entries {
        ensure!(
            entry.object_type == "blob"
                && matches!(entry.mode.as_str(), "100644" | "100755" | "120000"),
            "unsupported tracked snapshot input"
        );
        ensure!(
            Path::new(&entry.path)
                .file_name()
                .is_none_or(|n| n != ".gitattributes"),
            "tracked attributes are unsupported"
        );
        paths.insert(entry.path);
    }
    paths.extend(indexed.iter().cloned());
    let tracked: Vec<_> = paths.iter().cloned().collect();
    tracked_attributes(source.root(), &tracked, &mut inputs)?;
    let (untracked, dirs) = enumerate(
        &source,
        &indexed,
        &excluded_prefixes(source.root()).map_err(anyhow::Error::msg)?,
        &configured_extra_suffixes(source.root()).map_err(anyhow::Error::msg)?,
        &mut inputs,
    )?;
    paths.extend(untracked);
    let root = open_absolute_directory(source.root())?;
    validate_outputs(&root, selection)?;
    let identity = hash_paths(&root, &paths, selection, None)?;
    validate_outputs(&root, selection)?;
    inputs.verify()?;
    reject_workspace_path_overrides()?;
    verify_directories(source.root(), &dirs)?;
    binding.verify(&source)?;
    ensure!(
        source.repository().head()?.peel_to_commit()?.id() == parent_oid,
        "host HEAD changed during input capture"
    );
    Ok(identity)
}

fn hash_paths(
    root: &Dir,
    paths: &BTreeSet<String>,
    selection: &super::inputs::Selection,
    replacement: Option<&super::inputs::Replacement>,
) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs> {
    if let Some(replacement) = replacement {
        ensure!(
            paths.contains(replacement.path()) && !selection.contains(replacement.path()),
            "attribution target is absent or excluded"
        );
    }
    let mut builder =
        conductor_native::mutation_snapshot_inputs::Builder::new(selection.excluded())
            .map_err(anyhow::Error::msg)?;
    let mut total = 0u64;
    for path in paths {
        if selection.contains(path) {
            continue;
        }
        let replaced = replacement.filter(|r| r.path() == path);
        if replaced.is_some() {
            regular_single_link(root, path)?;
        }
        if let Some((mode, bytes)) = capture(root, path)? {
            total = total
                .checked_add(bytes.len() as u64)
                .context("snapshot input size overflow")?;
            ensure!(total <= MAX_TOTAL, "snapshot inputs exceed 2 GiB");
            let bytes = match replaced {
                Some(replacement) => {
                    regular_single_link(root, path)?;
                    replacement.original_for(mode, &bytes)?
                }
                None => bytes.as_slice(),
            };
            builder.add(path, mode, bytes).map_err(anyhow::Error::msg)?;
        } else {
            ensure!(replaced.is_none(), "attribution target disappeared");
        }
    }
    builder.finish().map_err(anyhow::Error::msg)
}

/// Recompute from an immutable Git tree, using the same path/mode/content framing.
pub(super) fn git_identity(
    source: &Source,
    tree: &str,
    selection: &super::inputs::Selection,
) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs> {
    let mut entries = source.list_tree(tree)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    let mut builder =
        conductor_native::mutation_snapshot_inputs::Builder::new(selection.excluded())
            .map_err(anyhow::Error::msg)?;
    let mut total = 0u64;
    for entry in entries {
        ensure!(
            entry.object_type == "blob",
            "unsupported Git snapshot input type"
        );
        if selection.contains(&entry.path) {
            ensure!(
                !super::inputs::forbidden_output_input(&entry.path),
                "workspace output namespace hides Cargo control or Rust source: {}",
                entry.path
            );
            ensure!(
                entry.mode != "120000",
                "excluded Git output must not be a symlink"
            );
            ensure!(
                matches!(entry.mode.as_str(), "100644" | "100755"),
                "unsupported Git output mode"
            );
            continue;
        }
        let mode = u32::from_str_radix(&entry.mode, 8)?;
        ensure!(
            matches!(mode, 0o100644 | 0o100755 | 0o120000),
            "unsupported Git snapshot input mode"
        );
        let blob = source.repository().find_blob(Oid::from_str(&entry.oid)?)?;
        ensure!(
            blob.size() as u64 <= MAX_BLOB,
            "Git snapshot input exceeds 32 MiB"
        );
        total = total
            .checked_add(blob.size() as u64)
            .context("Git input size overflow")?;
        ensure!(total <= MAX_TOTAL, "Git snapshot inputs exceed 2 GiB");
        builder
            .add(&entry.path, mode, blob.content())
            .map_err(anyhow::Error::msg)?;
    }
    builder.finish().map_err(anyhow::Error::msg)
}

struct SnapshotScan<'a> {
    selection: &'a super::inputs::Selection,
    paths: BTreeSet<String>,
    dirs: DirectoryStamps,
    entries: usize,
}
impl SnapshotScan<'_> {
    fn walk(&mut self, dir: &Dir, prefix: &str, depth: usize) -> Result<()> {
        ensure!(depth <= 256, "snapshot depth exceeds 256");
        let before = dir.dir_metadata()?;
        for item in dir.entries()? {
            self.entries += 1;
            ensure!(self.entries <= MAX_FILES, "snapshot entries exceed bound");
            let item = item?;
            let name = item.file_name();
            if prefix.is_empty() && name == ".git" {
                continue;
            }
            let name = name.to_str().context("snapshot path must be UTF-8")?;
            let path = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            crate::candidate_source::safe_path(path.as_bytes())?;
            if self.selection.contains(&path) {
                continue;
            }
            let meta = dir.symlink_metadata(name)?;
            if meta.is_dir() && !meta.is_symlink() {
                let child = Dir::from_std_file(
                    dir.open_with(
                        name,
                        OpenOptions::new()
                            .read(true)
                            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW),
                    )?
                    .into_std(),
                );
                let opened = child.dir_metadata()?;
                ensure!(
                    meta.dev() == opened.dev() && meta.ino() == opened.ino(),
                    "snapshot directory replaced: {path}"
                );
                self.walk(&child, &path, depth + 1)?;
            } else {
                self.paths.insert(path);
            }
        }
        let after = dir.dir_metadata()?;
        ensure!(
            stamp(&before) == stamp(&after),
            "snapshot directory changed: {prefix}"
        );
        self.dirs.push((prefix.to_owned(), stamp(&after)));
        Ok(())
    }
}

/// Recompute complete materialized input identity after no-Git cleanup or execution.
pub(super) fn materialized_identity(
    root: &Path,
    selection: &super::inputs::Selection,
) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs> {
    materialized_identity_replaced(root, selection, None)
}

pub(super) fn materialized_identity_replaced(
    root: &Path,
    selection: &super::inputs::Selection,
    replacement: Option<&super::inputs::Replacement>,
) -> Result<conductor_native::mutation_cargo_supporting_binding::SupportingInputs> {
    let root = root.canonicalize()?;
    let dir = open_absolute_directory(&root)?;
    validate_outputs(&dir, selection)?;
    let mut scan = SnapshotScan {
        selection,
        paths: BTreeSet::new(),
        dirs: Vec::new(),
        entries: 0,
    };
    scan.walk(&dir, "", 0)?;
    let identity = hash_paths(&dir, &scan.paths, selection, replacement)?;
    verify_directories(&root, &scan.dirs)?;
    validate_outputs(&dir, selection)?;
    Ok(identity)
}

fn regular_single_link(root: &Dir, path: &str) -> Result<()> {
    let (dir, name) = parent(root, path)?;
    let meta = dir.symlink_metadata(name)?;
    ensure!(
        meta.is_file() && !meta.is_symlink() && meta.nlink() == 1,
        "attribution target must be a regular file without aliases"
    );
    Ok(())
}

pub(super) fn attribution_replacement(
    root: &Path,
    row: &serde_json::Value,
    pins: &std::collections::BTreeMap<String, String>,
) -> Result<super::inputs::Replacement> {
    let path = row["path"].as_str().context("missing engine mutant path")?;
    conductor_native::mutation_snapshot_inputs::validate_path(path).map_err(anyhow::Error::msg)?;
    let pin = pins
        .get(path)
        .context("engine mutant is outside the pinned source set")?;
    let dir = open_absolute_directory(root)?;
    regular_single_link(&dir, path)?;
    let (mode, bytes) = capture(&dir, path)?.context("engine mutation source is missing")?;
    regular_single_link(&dir, path)?;
    ensure!(
        crate::mutation_campaign::sha256_hex(&bytes) == *pin,
        "attribution original source differs from campaign pin"
    );
    super::inputs::Replacement::from_row(path.into(), mode, bytes, row)
}

/// Refuse physical aliases in existing coordinator-owned output roots.
/// Rechecked at capture and final verification; output publication must also use
/// the coordinator's confined, alias-safe writer.
fn validate_output_entry(
    dir: &Dir,
    name: &std::ffi::OsStr,
    depth: usize,
    count: &mut usize,
) -> Result<()> {
    ensure!(depth <= 256, "snapshot output depth exceeds bound");
    *count = count
        .checked_add(1)
        .context("snapshot output count overflow")?;
    ensure!(*count <= MAX_FILES, "snapshot output entries exceed bound");
    let meta = match dir.symlink_metadata(name) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let component = name
        .to_str()
        .context("snapshot output path must be UTF-8")?;
    ensure!(
        !super::inputs::forbidden_output_input(component),
        "workspace output namespace hides Cargo control or Rust source: {component}"
    );
    ensure!(!meta.is_symlink(), "snapshot output must not be a symlink");
    if meta.is_file() {
        ensure!(meta.nlink() == 1, "snapshot output must not be hardlinked");
    } else if meta.is_dir() {
        let child = Dir::from_std_file(
            dir.open_with(
                name,
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW),
            )?
            .into_std(),
        );
        let opened = child.dir_metadata()?;
        ensure!(
            meta.dev() == opened.dev() && meta.ino() == opened.ino(),
            "snapshot output directory replaced"
        );
        for item in child.entries()? {
            validate_output_entry(&child, &item?.file_name(), depth + 1, count)?;
        }
        ensure!(
            stamp(&opened) == stamp(&child.dir_metadata()?),
            "snapshot output directory changed"
        );
    } else {
        anyhow::bail!("snapshot output must be a regular file or directory");
    }
    Ok(())
}
fn validate_outputs(root: &Dir, selection: &super::inputs::Selection) -> Result<()> {
    let mut count = 0;
    for path in selection.excluded() {
        let (dir, name) = match parent(root, path) {
            Ok(pair) => pair,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        validate_output_entry(&dir, std::ffi::OsStr::new(&name), 0, &mut count)?;
    }
    Ok(())
}
