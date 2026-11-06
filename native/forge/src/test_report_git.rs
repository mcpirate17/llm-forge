//! Held read-only Git inputs for test reports. No Git process or external driver.
use crate::{
    candidate_inputs::{self, BoundFile, DirectoryBinding},
    candidate_source::Source,
};
use anyhow::{ensure, Context, Result};
use git2::{
    Delta, Diff, DiffFindOptions, DiffOptions, ErrorCode, ObjectType, Oid, Repository,
    RepositoryOpenFlags, Sort, Status, StatusOptions, StatusShow,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

const MAX_PATHS: usize = 100_000;
const MAX_COMMITS: usize = 100_000;
const MAX_EVENTS: usize = 1_000_000;
const MAX_METADATA: usize = 128 * 1024 * 1024;
const MAX_RENAME_CANDIDATES: usize = 1000;
const MAX_RENAME_PAIRS: usize = 65_536;

struct FileInput {
    path: PathBuf,
    held: Option<BoundFile>,
    bytes: Vec<u8>,
}
impl FileInput {
    fn capture(path: PathBuf, maximum: usize) -> Result<Self> {
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                let (held, bytes) = candidate_inputs::repository_live(&path, maximum)?;
                Ok(Self {
                    path,
                    held: Some(held),
                    bytes,
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                path,
                held: None,
                bytes: Vec::new(),
            }),
            Err(error) => Err(error).context("inspect Git report input"),
        }
    }
    fn verify(&self) -> Result<()> {
        if let Some(held) = &self.held {
            held.verify()?;
        } else {
            ensure!(
                matches!(fs::symlink_metadata(&self.path), Err(error)
                if error.kind() == std::io::ErrorKind::NotFound),
                "absent Git input appeared"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Settings {
    renames: bool,
    rename_limit: usize,
    show_root: bool,
}
fn bool_setting(config: &git2::Config, key: &str, default: bool) -> Result<bool> {
    match config.get_bool(key) {
        Ok(value) => Ok(value),
        Err(error) if error.code() == ErrorCode::NotFound => Ok(default),
        Err(error) => Err(error).with_context(|| format!("unsupported Git setting {key}")),
    }
}
fn settings(repo: &Repository) -> Result<Settings> {
    let config = repo.config()?.snapshot()?;
    let rename_limit = match config.get_i64("diff.renamelimit") {
        Ok(value) => {
            ensure!(
                value > 0 && value <= MAX_RENAME_CANDIDATES as i64,
                "unsupported or unbounded diff.renameLimit"
            );
            value as usize
        }
        Err(error) if error.code() == ErrorCode::NotFound => MAX_RENAME_CANDIDATES,
        Err(error) => return Err(error).context("invalid diff.renameLimit"),
    };
    match config.get_string("diff.algorithm") {
        Ok(value) => ensure!(
            matches!(value.as_str(), "myers" | "default"),
            "unsupported history diff.algorithm"
        ),
        Err(error) if error.code() == ErrorCode::NotFound => {}
        Err(error) => return Err(error).context("invalid history diff.algorithm"),
    }
    Ok(Settings {
        renames: bool_setting(&config, "diff.renames", true)?,
        rename_limit,
        show_root: bool_setting(&config, "log.showroot", true)?,
    })
}

// FROM_ENV is necessary for GIT_CONFIG_GLOBAL/SYSTEM/NOSYSTEM. Always open the
// already selected Git directory, and refuse environment-based repository changes.
fn repository(source: &Source) -> Result<Repository> {
    no_replacements(source)?;
    if std::env::var_os("GIT_CONFIG_GLOBAL").is_some() {
        match git2::Config::find_xdg() {
            Ok(_) => {
                anyhow::bail!("explicit GIT_CONFIG_GLOBAL with existing XDG Git config unsupported")
            }
            Err(error) if error.code() == ErrorCode::NotFound => {}
            Err(error) => return Err(error).context("inspect XDG Git configuration"),
        }
    }
    let repo = Repository::open_ext(
        source.repository().path(),
        RepositoryOpenFlags::FROM_ENV,
        &[] as &[&std::ffi::OsStr],
    )?;
    ensure!(
        repo.path().canonicalize()? == source.repository().path().canonicalize()?
            && repo.commondir().canonicalize()? == source.common_dir()
            && repo
                .workdir()
                .context("report repository must have a workdir")?
                .canonicalize()?
                == source.root(),
        "environment-aware report repository identities differ from selected source"
    );
    Ok(repo)
}

fn index_path(source: &Source) -> Result<PathBuf> {
    let path = match std::env::var_os("GIT_INDEX_FILE") {
        Some(value) => {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                path
            } else {
                source.root().join(path)
            }
        }
        None => source.repository().path().join("index"),
    };
    ensure!(
        path.as_os_str().len() <= 4096,
        "Git index path exceeds bound"
    );
    Ok(path)
}
fn head(repo: &Repository) -> Result<(String, Oid)> {
    let reference = repo
        .head()
        .context("test report history requires an existing HEAD commit")?;
    let id = reference
        .target()
        .context("HEAD does not resolve to a direct commit")?;
    let (size, kind) = repo.odb()?.read_header(id)?;
    ensure!(
        kind == ObjectType::Commit && size <= 1024 * 1024,
        "HEAD commit exceeds history bounds"
    );
    Ok((std::str::from_utf8(reference.name_bytes())?.to_owned(), id))
}
fn path(raw: &[u8]) -> Result<String> {
    let value = crate::candidate_source::safe_path(raw)?;
    ensure!(value.len() <= 4096, "Git path exceeds report bound");
    Ok(value.to_owned())
}
fn untracked(source: &Source, index: &FileInput) -> Result<Vec<String>> {
    let repo = repository(source)?;
    let mut parsed = if index.held.is_some() {
        crate::candidate_source::parse_index(&index.bytes)?
    } else {
        git2::Index::new()?
    };
    repo.set_index(&mut parsed)?;
    let mut options = StatusOptions::new();
    options
        .show(StatusShow::Workdir)
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .update_index(false);
    let statuses = repo.statuses(Some(&mut options))?;
    ensure!(
        statuses.len() <= MAX_PATHS,
        "untracked Git inventory exceeds bound"
    );
    let mut result = BTreeSet::new();
    for entry in statuses
        .iter()
        .filter(|entry| entry.status().contains(Status::WT_NEW))
    {
        let value = path(entry.path_bytes())?;
        if conductor_native::dead_tests::report::is_test_file(&value) {
            result.insert(value);
        }
    }
    Ok(result.into_iter().collect())
}

fn shallow_roots(input: &FileInput) -> Result<BTreeSet<Oid>> {
    let text = std::str::from_utf8(&input.bytes)?;
    let mut result = BTreeSet::new();
    for line in text.lines() {
        ensure!(
            line.len() == 40 && result.insert(Oid::from_str(line)?),
            "invalid or duplicate shallow boundary"
        );
        ensure!(result.len() <= 4096, "shallow boundary count exceeds bound");
    }
    Ok(result)
}
fn date(commit: &git2::Commit<'_>) -> Result<String> {
    let time = commit.author().when();
    let seconds = time
        .seconds()
        .checked_add(i64::from(time.offset_minutes()) * 60)
        .context("Git author time overflows")?;
    let days = seconds.div_euclid(86_400);
    ensure!(
        (-719_528..=2_932_896).contains(&days),
        "Git author date outside four-digit years"
    );
    let (year, month, day) = crate::civil::civil_from_days(days);
    ensure!(
        (0..=9999).contains(&year),
        "Git author year outside report range"
    );
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

struct Budget {
    started: Instant,
    metadata: usize,
    tree_entries: usize,
    trees: BTreeSet<Oid>,
    events: usize,
    path_bytes: usize,
}
impl Budget {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            metadata: 0,
            tree_entries: 0,
            trees: BTreeSet::new(),
            events: 0,
            path_bytes: 0,
        }
    }
    fn poll(&self) -> Result<()> {
        ensure!(
            self.started.elapsed() <= Duration::from_secs(30),
            "Git history cooperative deadline exceeded"
        );
        Ok(())
    }
    fn header(
        &mut self,
        repo: &Repository,
        id: Oid,
        kind: ObjectType,
        maximum: usize,
    ) -> Result<()> {
        self.poll()?;
        let (size, actual) = repo.odb()?.read_header(id)?;
        ensure!(
            actual == kind && size <= maximum,
            "Git history object type or size exceeds bound"
        );
        self.metadata = self
            .metadata
            .checked_add(size)
            .context("Git metadata size overflow")?;
        ensure!(
            self.metadata <= MAX_METADATA,
            "Git history metadata exceeds aggregate bound"
        );
        Ok(())
    }
    fn tree(&mut self, repo: &Repository, id: Oid) -> Result<()> {
        let mut pending = vec![id];
        while let Some(id) = pending.pop() {
            if !self.trees.insert(id) {
                continue;
            }
            self.header(repo, id, ObjectType::Tree, 16 * 1024 * 1024)?;
            let tree = repo.find_tree(id)?;
            self.tree_entries += tree.len();
            ensure!(
                self.tree_entries <= MAX_EVENTS,
                "Git history tree entries exceed bound"
            );
            for entry in tree.iter() {
                path(entry.name_bytes())?;
                if entry.kind() == Some(ObjectType::Tree) {
                    pending.push(entry.id());
                }
            }
        }
        Ok(())
    }
}

fn preflight_history(
    repo: &Repository,
    start: Oid,
    shallow: &BTreeSet<Oid>,
    budget: &mut Budget,
) -> Result<()> {
    let mut pending = vec![start];
    let mut queued = BTreeSet::from([start]);
    while let Some(id) = pending.pop() {
        budget.header(repo, id, ObjectType::Commit, 1024 * 1024)?;
        let commit = repo.find_commit(id)?;
        if shallow.contains(&id) {
            continue;
        }
        for parent in commit.parent_ids() {
            if queued.insert(parent) {
                ensure!(
                    queued.len() <= MAX_COMMITS,
                    "Git history commit count exceeds bound"
                );
                pending.push(parent);
            }
        }
    }
    Ok(())
}

fn rename(
    diff: &mut Diff<'_>,
    repo: &Repository,
    settings: &Settings,
    budget: &Budget,
) -> Result<()> {
    let deleted = diff
        .deltas()
        .filter(|delta| delta.status() == Delta::Deleted)
        .count();
    let added = diff
        .deltas()
        .filter(|delta| delta.status() == Delta::Added)
        .count();
    if !settings.renames || deleted == 0 || added == 0 {
        return Ok(());
    }
    ensure!(
        deleted <= settings.rename_limit
            && added <= settings.rename_limit
            && deleted
                .checked_mul(added)
                .is_some_and(|pairs| pairs <= MAX_RENAME_PAIRS),
        "rename candidate work exceeds bound; matching was not truncated"
    );
    let mut bytes = 0usize;
    for delta in diff
        .deltas()
        .filter(|delta| matches!(delta.status(), Delta::Added | Delta::Deleted))
    {
        let id = if delta.status() == Delta::Deleted {
            delta.old_file().id()
        } else {
            delta.new_file().id()
        };
        let (size, kind) = repo.odb()?.read_header(id)?;
        ensure!(
            kind == ObjectType::Blob && size <= 8 * 1024 * 1024,
            "rename blob exceeds bound"
        );
        bytes = bytes
            .checked_add(size)
            .context("rename byte count overflow")?;
        ensure!(bytes <= 64 * 1024 * 1024, "rename blob bytes exceed bound");
    }
    budget.poll()?;
    diff.find_similar(Some(
        DiffFindOptions::new()
            .renames(true)
            .rename_limit(settings.rename_limit),
    ))?;
    budget.poll()
}

fn history(
    source: &Source,
    start: Oid,
    shallow: &BTreeSet<Oid>,
    settings: &Settings,
) -> Result<BTreeMap<String, String>> {
    let owned = repository(source)?;
    let repo = &owned;
    let mut budget = Budget::new();
    // Bound the reachable graph before libgit2 prepares its default walk.
    preflight_history(repo, start, shallow, &mut budget)?;
    let mut walk = repo.revwalk()?;
    // libgit2 documents NONE as Git's default reverse-chronological walk.
    walk.set_sorting(Sort::NONE)?;
    walk.push(start)?;
    let mut dates = BTreeMap::new();
    for (index, id) in walk.enumerate() {
        ensure!(
            index < MAX_COMMITS,
            "Git history commit count exceeds bound"
        );
        let id = id?;
        budget.header(repo, id, ObjectType::Commit, 1024 * 1024)?;
        let commit = repo.find_commit(id)?;
        let parents = if shallow.contains(&id) {
            0
        } else {
            commit.parent_count()
        };
        if parents > 1 || parents == 0 && !settings.show_root {
            continue;
        }
        budget.tree(repo, commit.tree_id())?;
        let tree = commit.tree()?;
        let parent = if parents == 1 {
            let id = commit.parent_id(0)?;
            budget.header(repo, id, ObjectType::Commit, 1024 * 1024)?;
            let parent = repo.find_commit(id)?;
            budget.tree(repo, parent.tree_id())?;
            Some(parent.tree()?)
        } else {
            None
        };
        let mut options = DiffOptions::new();
        options.include_typechange(true).skip_binary_check(true);
        let mut diff = repo.diff_tree_to_tree(parent.as_ref(), Some(&tree), Some(&mut options))?;
        ensure!(
            diff.deltas().len() <= MAX_PATHS,
            "Git history diff path count exceeds bound"
        );
        rename(&mut diff, repo, settings, &budget)?;
        let day = date(&commit)?;
        for delta in diff.deltas() {
            let file = if delta.status() == Delta::Deleted {
                delta.old_file()
            } else {
                delta.new_file()
            };
            let name = path(file.path_bytes().context("history delta path missing")?)?;
            budget.events += 1;
            ensure!(
                budget.events <= MAX_EVENTS,
                "Git history path events exceed bound"
            );
            // Raw Unicode paths intentionally fix the legacy Git-text quoting bug.
            if !dates.contains_key(&name) {
                budget.path_bytes += name.len();
                ensure!(
                    dates.len() < MAX_PATHS && budget.path_bytes <= 32 * 1024 * 1024,
                    "Git history path inventory exceeds bound"
                );
                dates.insert(name, day.clone());
            }
        }
    }
    Ok(dates)
}

pub(crate) struct Inventory {
    pub tracked: Vec<String>,
    pub untracked_tests: Vec<String>,
    pub last_commit_dates: BTreeMap<String, String>,
    pub head: String,
    pub shallow: bool,
    root: PathBuf,
    git_dir: PathBuf,
    common: PathBuf,
    directories: Vec<DirectoryBinding>,
    head_name: String,
    index: FileInput,
    boundaries: FileInput,
    grafts: FileInput,
    settings: Settings,
}
fn no_replacements(source: &Source) -> Result<()> {
    for name in [
        "GIT_REPLACE_REF_BASE",
        "GIT_SHALLOW_FILE",
        "GIT_GRAFT_FILE",
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
    ] {
        ensure!(
            std::env::var_os(name).is_none(),
            "custom Git history environment unsupported: {name}"
        );
    }
    let mut refs = source.repository().references_glob("refs/replace/*")?;
    if let Some(reference) = refs.next() {
        reference?;
        anyhow::bail!("Git replacement history unsupported");
    }
    Ok(())
}
impl Inventory {
    pub(crate) fn capture(source: &Source) -> Result<Self> {
        no_replacements(source)?;
        let repo = repository(source)?;
        let (head_name, head) = head(&repo)?;
        let root = source.root().to_owned();
        let git_dir = source.repository().path().canonicalize()?;
        let common = source.common_dir().to_owned();
        let directories = [&root, &git_dir, &common]
            .into_iter()
            .map(|path| DirectoryBinding::repository(path, None))
            .collect::<Result<Vec<_>>>()?;
        let index = FileInput::capture(index_path(source)?, 32 * 1024 * 1024)?;
        let boundaries = FileInput::capture(common.join("shallow"), 1024 * 1024)?;
        let grafts = FileInput::capture(common.join("info/grafts"), 1024 * 1024)?;
        ensure!(grafts.bytes.is_empty(), "Git graft history unsupported");
        let roots = shallow_roots(&boundaries)?;
        let settings = settings(&repo)?;
        let tracked = source.tracked_index_paths()?;
        let untracked_tests = untracked(source, &index)?;
        let last_commit_dates = history(source, head, &roots, &settings)?;
        let result = Self {
            tracked,
            untracked_tests,
            last_commit_dates,
            head: head.to_string(),
            shallow: !roots.is_empty(),
            root,
            git_dir,
            common,
            directories,
            head_name,
            index,
            boundaries,
            grafts,
            settings,
        };
        result.verify(source)?;
        Ok(result)
    }
    pub(crate) fn verify(&self, source: &Source) -> Result<()> {
        ensure!(
            source.root() == self.root,
            "test report source root changed"
        );
        for directory in &self.directories {
            directory.verify()?;
        }
        let fresh = Source::open(&self.root)?;
        ensure!(
            fresh.repository().path().canonicalize()? == self.git_dir
                && fresh.common_dir() == self.common,
            "test report Git identities changed"
        );
        no_replacements(&fresh)?;
        let repo = repository(&fresh)?;
        let (name, id) = head(&repo)?;
        ensure!(
            name == self.head_name && id.to_string() == self.head,
            "test report HEAD changed"
        );
        ensure!(
            index_path(&fresh)? == self.index.path,
            "test report index selection changed"
        );
        self.index.verify()?;
        self.boundaries.verify()?;
        self.grafts.verify()?;
        ensure!(
            settings(&repo)? == self.settings,
            "test report history configuration changed"
        );
        ensure!(
            fresh.tracked_index_paths()? == self.tracked
                && untracked(&fresh, &self.index)? == self.untracked_tests,
            "test report Git path inventory changed"
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "test_report_git_tests.rs"]
mod tests;
