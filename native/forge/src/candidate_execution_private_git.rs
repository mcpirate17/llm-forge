//! Create private Git metadata; object reads use an explicitly granted source ODB.
use crate::candidate_source::Source;
use anyhow::{ensure, Context, Result};
use git2::{Index, Oid, Repository, RepositoryInitOptions, Signature, Time};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub(super) fn prepare(
    source: &Source,
    tree: &str,
    runtime: &Path,
    workspace: &Path,
) -> Result<(PathBuf, PathBuf)> {
    let git_dir = runtime.join("git");
    let mut options = RepositoryInitOptions::new();
    options.bare(true).external_template(false);
    let repo = Repository::init_opts(&git_dir, &options)?;
    let object_path = source.common_dir().join("objects");
    let object_path = object_path.canonicalize()?;
    let object_path = object_path
        .to_str()
        .context("Git object directory must be UTF-8")?;
    ensure!(
        !object_path.contains(['\n', '\r', '\0']),
        "invalid Git alternate path"
    );
    let info = git_dir.join("objects/info");
    fs::create_dir_all(&info)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(info.join("alternates"))?;
    writeln!(file, "{object_path}")?;
    file.sync_all()?;
    repo.odb()?.add_disk_alternate(object_path)?;
    let tree = repo.find_tree(Oid::from_str(tree)?)?;
    let signature = Signature::new("Forge Candidate", "candidate@invalid", &Time::new(0, 0))?;
    let commit = repo.commit(
        None,
        &signature,
        &signature,
        "candidate review snapshot",
        &tree,
        &[],
    )?;
    repo.set_head_detached(commit)?;
    let index = runtime.join("index");
    let mut staged = Index::open(&index)?;
    staged.read_tree(&tree)?;
    staged.write()?;
    let mut config = repo.config()?;
    config.set_bool("core.bare", false)?;
    config.set_str(
        "core.worktree",
        workspace.to_str().context("workspace path is not UTF-8")?,
    )?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(runtime.join("gitconfig"))?
        .sync_all()?;
    Ok((git_dir, index))
}
