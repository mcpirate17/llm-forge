//! Create private Git metadata; object reads use an explicitly granted source ODB.
use crate::candidate_source::Source;
use anyhow::{ensure, Context, Result};
use git2::{Index, ObjectType, Oid, Repository, RepositoryInitOptions, Signature, Time};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy)]
pub(super) enum Objects {
    Borrowed,
    Private,
}

/// Copy only preflighted tree and blob objects, not history or alternates.
fn copy_tree(source: &Source, destination: &Repository, tree: &str) -> Result<()> {
    crate::candidate_snapshot::validate_recovery_tree(
        source,
        tree,
        crate::candidate_snapshot::Limits::default(),
    )?;
    let input = source.repository().odb()?;
    let output = destination.odb()?;
    let mut pending = vec![(Oid::from_str(tree)?, ObjectType::Tree)];
    let mut seen = HashSet::new();
    let mut total = 0u64;
    while let Some((oid, kind)) = pending.pop() {
        if !seen.insert(oid) {
            continue;
        }
        ensure!(seen.len() <= 200_001, "private Git object count exceeded");
        let (size, actual) = input.read_header(oid)?;
        ensure!(
            actual == kind && size <= 64 * 1024 * 1024,
            "private Git object type or size invalid"
        );
        total = total
            .checked_add(size as u64)
            .context("private Git object size overflow")?;
        ensure!(
            total <= 2 * 1024 * 1024 * 1024 + 64 * 1024 * 1024,
            "private Git object budget exceeded"
        );
        let object = input.read(oid)?;
        ensure!(
            object.kind() == kind && object.data().len() == size,
            "private Git object changed"
        );
        ensure!(
            output.write(kind, object.data())? == oid,
            "private Git object identity mismatch"
        );
        if kind == ObjectType::Tree {
            for entry in source.repository().find_tree(oid)?.iter() {
                let child = entry.kind().context("private Git object type missing")?;
                ensure!(
                    matches!(child, ObjectType::Tree | ObjectType::Blob),
                    "private Git refuses gitlinks"
                );
                pending.push((entry.id(), child));
            }
        }
    }
    Ok(())
}

pub(super) fn prepare(
    source: &Source,
    tree: &str,
    runtime: &Path,
    workspace: &Path,
    objects: Objects,
) -> Result<(PathBuf, PathBuf)> {
    let git_dir = runtime.join("git");
    let mut options = RepositoryInitOptions::new();
    options.bare(true).external_template(false);
    let repo = Repository::init_opts(&git_dir, &options)?;
    // Explicit private metadata permissions must not depend on the caller's umask.
    fs::set_permissions(&git_dir, fs::Permissions::from_mode(0o700))?;
    if matches!(objects, Objects::Private) {
        copy_tree(source, &repo, tree)?;
    } else {
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
    }
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
    fs::set_permissions(git_dir.join("HEAD"), fs::Permissions::from_mode(0o600))?;
    let index = runtime.join("index");
    let mut staged = Index::open(&index)?;
    staged.read_tree(&tree)?;
    staged.write()?;
    fs::set_permissions(&index, fs::Permissions::from_mode(0o600))?;
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
