//! Explicit private Git context for snapshot tools. Operator-owned parents must remain
//! stable; checks detect ordinary drift but do not attest against a same-UID racer.
use super::{resolve, Candidate, Kind, Source, EMPTY_TREE, MAX_OUTPUT};
use anyhow::{bail, ensure, Context, Result};
use git2::{ObjectType, Oid, Repository};
use sha2::{Digest, Sha256};
use std::{
    env,
    fs::{self, Metadata, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const MAX_COMMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    uid: u32,
    mode: u32,
}

fn identity(m: &Metadata) -> Identity {
    Identity {
        dev: m.dev(),
        ino: m.ino(),
        uid: m.uid(),
        mode: m.mode(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    identity: Identity,
    len: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}

fn stamp(m: &Metadata) -> Stamp {
    Stamp {
        identity: identity(m),
        len: m.len(),
        mtime: m.mtime(),
        mtime_ns: m.mtime_nsec(),
        ctime: m.ctime(),
        ctime_ns: m.ctime_nsec(),
    }
}

fn directory(path: &Path) -> Result<Identity> {
    ensure!(
        path.is_absolute() && path.canonicalize()? == path,
        "directory must be canonical and absolute: {}",
        path.display()
    );
    let m = fs::symlink_metadata(path)?;
    ensure!(
        m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o022 == 0,
        "unsafe private directory: {}",
        path.display()
    );
    Ok(identity(&m))
}

fn read_file(path: &Path, limit: usize) -> Result<(Stamp, Vec<u8>)> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("open private file {}", path.display()))?;
    let before = file.metadata()?;
    ensure!(
        before.is_file()
            && before.nlink() == 1
            && before.uid() == unsafe { libc::geteuid() }
            && before.mode() & 0o022 == 0
            && before.len() <= limit as u64,
        "unsafe private file: {}",
        path.display()
    );
    let mut bytes = Vec::with_capacity(before.len() as usize);
    (&mut file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= limit
            && stamp(&before) == stamp(&file.metadata()?)
            && stamp(&before) == stamp(&fs::symlink_metadata(path)?),
        "private file changed while reading: {}",
        path.display()
    );
    Ok((stamp(&before), bytes))
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn check_index(source: &Source, index: &git2::Index, tree: &str) -> Result<()> {
    let expected = source.list_tree(tree)?;
    ensure!(
        expected.len() == index.len(),
        "private index entry count differs from candidate tree"
    );
    for (entry, expected) in index.iter().zip(expected.iter()) {
        ensure!(
            entry.path == expected.path.as_bytes()
                && format!("{:06o}", entry.mode) == expected.mode
                && entry.id.to_string() == expected.oid,
            "private index differs from candidate tree"
        );
    }
    Ok(())
}

fn oid(value: &str) -> Result<Oid> {
    ensure!(
        value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "expected a full 40-digit Git object ID"
    );
    Ok(Oid::from_str(value)?)
}

fn header(repo: &Repository, oid: Oid) -> Result<ObjectType> {
    let (size, kind) = repo.odb()?.read_header(oid)?;
    ensure!(size <= MAX_OUTPUT, "Git object exceeds 64 MiB");
    if kind == ObjectType::Commit {
        ensure!(size <= MAX_COMMIT, "Git commit exceeds 1 MiB");
    }
    Ok(kind)
}

fn base_tree(repo: &Repository, base: &str) -> Result<(String, Option<String>)> {
    let id = oid(base)?;
    if id.to_string() == EMPTY_TREE {
        return Ok((EMPTY_TREE.into(), None));
    }
    match header(repo, id)? {
        ObjectType::Commit => Ok((
            repo.find_commit(id)?.tree_id().to_string(),
            Some(id.to_string()),
        )),
        ObjectType::Tree => Ok((id.to_string(), None)),
        _ => bail!("base must identify a commit or tree"),
    }
}

fn head(repo: &Repository) -> Result<(String, String)> {
    let id = repo
        .head()?
        .target()
        .context("private HEAD must resolve to a direct commit")?;
    ensure!(
        header(repo, id)? == ObjectType::Commit,
        "private HEAD must identify a commit"
    );
    Ok((id.to_string(), repo.find_commit(id)?.tree_id().to_string()))
}

#[derive(Clone, Debug)]
pub struct ToolBinding {
    root: PathBuf,
    runtime: PathBuf,
    git: PathBuf,
    index: PathBuf,
    root_id: Identity,
    runtime_id: Identity,
    git_id: Identity,
    index_stamp: Stamp,
    index_hash: [u8; 32],
    head_stamp: Stamp,
    head_hash: [u8; 32],
    head_oid: String,
    tree_oid: String,
}

impl ToolBinding {
    pub fn verify(&self, source: &Source) -> Result<()> {
        ensure!(
            source.root == self.root && source.common_dir == self.git,
            "private source identity changed"
        );
        ensure!(
            directory(&self.root)? == self.root_id
                && directory(&self.runtime)? == self.runtime_id
                && directory(&self.git)? == self.git_id,
            "private directory identity changed"
        );
        let (index_stamp, index) = read_file(&self.index, MAX_OUTPUT)?;
        let (head_stamp, raw_head) = read_file(&self.git.join("HEAD"), MAX_COMMIT)?;
        ensure!(
            index_stamp == self.index_stamp
                && digest(&index) == self.index_hash
                && head_stamp == self.head_stamp
                && digest(&raw_head) == self.head_hash,
            "private index or HEAD changed"
        );
        let (head_oid, tree_oid) = head(&source.repository)?;
        ensure!(
            head_oid == self.head_oid && tree_oid == self.tree_oid,
            "private HEAD reference changed"
        );
        Ok(())
    }
}

fn paths(
    root: &Path,
    git: Option<PathBuf>,
    work: Option<PathBuf>,
    index: Option<PathBuf>,
) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let git = git.context("GIT_DIR is required")?;
    let work = work.context("GIT_WORK_TREE is required")?;
    let index = index.context("GIT_INDEX_FILE is required")?;
    directory(root)?;
    ensure!(
        work == root && work.is_absolute() && work.canonicalize()? == work,
        "GIT_WORK_TREE must equal canonical snapshot root"
    );
    directory(&git)?;
    ensure!(
        index.is_absolute() && index.parent() == git.parent(),
        "private Git and index must share a runtime parent"
    );
    let parent = git.parent().context("private Git has no parent")?;
    directory(parent)?;
    ensure!(
        !parent.starts_with(root) && !root.starts_with(parent),
        "private runtime must be outside snapshot"
    );
    ensure!(index.file_name().is_some(), "invalid index path");
    Ok((git, work, index))
}

impl Source {
    pub fn tool_context(
        root: &Path,
        base: &str,
        tree: &str,
    ) -> Result<(Source, Candidate, ToolBinding)> {
        Self::tool_context_paths(
            root,
            base,
            tree,
            env::var_os("GIT_DIR").map(PathBuf::from),
            env::var_os("GIT_WORK_TREE").map(PathBuf::from),
            env::var_os("GIT_INDEX_FILE").map(PathBuf::from),
        )
    }

    fn tool_context_paths(
        root: &Path,
        base: &str,
        tree: &str,
        git: Option<PathBuf>,
        work: Option<PathBuf>,
        index: Option<PathBuf>,
    ) -> Result<(Source, Candidate, ToolBinding)> {
        let (git, _, index) = paths(root, git, work, index)?;
        let repository = Repository::open(&git).context("open explicit private Git directory")?;
        ensure!(
            !repository.is_worktree()
                && repository.path().canonicalize()? == git
                && repository.commondir().canonicalize()? == git,
            "linked or redirected Git metadata is unsupported"
        );
        let source = Source {
            repository,
            root: root.to_path_buf(),
            common_dir: git.clone(),
        };
        let tree = oid(tree)?.to_string();
        let (head_oid, head_tree) = head(&source.repository)?;
        ensure!(
            head_tree == tree,
            "private HEAD tree differs from expected candidate tree"
        );
        let (index_stamp, raw_index) = read_file(&index, MAX_OUTPUT)?;
        let parsed = resolve::parse_index(&raw_index)?;
        check_index(&source, &parsed, &tree)?;
        let (base_tree_oid, base_commit_oid) = base_tree(&source.repository, base)?;
        let changes = source.changes(&base_tree_oid, &tree)?;
        let (head_stamp, raw_head) = read_file(&git.join("HEAD"), MAX_COMMIT)?;
        let binding = ToolBinding {
            root: root.to_path_buf(),
            runtime: git.parent().unwrap().to_path_buf(),
            root_id: directory(root)?,
            runtime_id: directory(git.parent().unwrap())?,
            git_id: directory(&git)?,
            git,
            index,
            index_stamp,
            index_hash: digest(&raw_index),
            head_stamp,
            head_hash: digest(&raw_head),
            head_oid,
            tree_oid: tree.clone(),
        };
        binding.verify(&source)?;
        let candidate = Candidate {
            kind: Kind::Index,
            tree_oid: tree,
            base_tree_oid,
            base_commit_oid,
            commit_oid: None,
            target_ref: None,
            changes,
            integration_base_oid: None,
            integration_base_detail: "private tool context does not establish integration ancestry"
                .into(),
        };
        Ok((source, candidate, binding))
    }
}

#[cfg(test)]
#[path = "tool_context_tests.rs"]
mod tests;
