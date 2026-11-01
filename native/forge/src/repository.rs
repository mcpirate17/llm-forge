//! Bounded filesystem checkout discovery shared by hooks and workspace context.
use anyhow::{ensure, Context, Result};
use std::path::{Path, PathBuf};

fn marker(path: &Path) -> Result<String> {
    let bytes = crate::bounded_file::read(path, 4096)?;
    let text = std::str::from_utf8(&bytes)?.trim();
    ensure!(
        !text.is_empty() && !text.chars().any(char::is_control),
        "invalid Git directory marker: {}",
        path.display()
    );
    Ok(text.into())
}

fn common(root: &Path, entry: &Path) -> Result<PathBuf> {
    let text = marker(entry)?;
    let raw = text
        .strip_prefix("gitdir:")
        .context("invalid gitdir marker")?
        .trim();
    ensure!(!raw.is_empty(), "empty gitdir marker");
    let gitdir = root.join(raw).canonicalize().context("resolving gitdir")?;
    ensure!(gitdir.is_dir(), "gitdir must name a directory");
    let path = gitdir.join("commondir");
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(gitdir),
        Err(e) => Err(e.into()),
        Ok(_) => {
            let common = gitdir
                .join(marker(&path)?)
                .canonicalize()
                .context("resolving Git common directory")?;
            ensure!(
                common.is_dir(),
                "Git common directory must name a directory"
            );
            Ok(common)
        }
    }
}

/// Missing repositories are optional; malformed or unsafe Git metadata is an error.
pub fn checkout_of_checked(path: &Path) -> Result<Option<(PathBuf, PathBuf)>> {
    for dir in path.ancestors() {
        let entry = dir.join(".git");
        let metadata = match std::fs::symlink_metadata(&entry) {
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue
            }
            Err(e) => return Err(e.into()),
            Ok(metadata) => metadata,
        };
        let common = if metadata.is_dir() {
            entry.canonicalize()?
        } else {
            ensure!(
                metadata.is_file(),
                "Git marker must be a directory or regular file"
            );
            common(dir, &entry)?
        };
        return Ok(Some((dir.to_path_buf(), common)));
    }
    Ok(None)
}

/// Hook discovery keeps its optional interface, with explicit failure diagnostics.
/// Claim readers use the checked API and propagate metadata failures.
pub fn checkout_of(path: &Path) -> Option<(PathBuf, PathBuf)> {
    match checkout_of_checked(path) {
        Ok(checkout) => checkout,
        Err(error) => {
            eprintln!("forge: checkout discovery: {error:#}");
            None
        }
    }
}
