//! Exclusive draft publication under the pinned host capability.
//! Operator-controlled ancestors and same-UID mutation after the last identity
//! check remain trusted; no custom filesystem transaction is claimed.
use super::{
    directory_options, normalize_model_draft, same_inode, validate_value, Settings,
    MAX_DOCUMENT_BYTES,
};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, File, MetadataExt, OpenOptions, OpenOptionsExt};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

const BASE: &str = "research/tmp/local_clerk";
const MAX_PATH: usize = 4096;

/// Verify source provenance then exclusively publish a normalized, private draft.
pub fn write_document(settings: &Settings, path: &Path, document: &Value) -> Result<Value> {
    write_checked(settings, path, document, |_| Ok(()))
}

/// Inspect an output target without creating directories or reserving the name.
/// Publication must still repeat all checks because the filesystem can change.
pub fn preflight_output(settings: &Settings, path: &Path) -> Result<()> {
    let (_, relative) = output_path(settings, path)?;
    let root = &settings.roots[0];
    root.check().context("clerk output host changed")?;
    let parent = relative.parent().context("clerk output parent missing")?;
    let name = relative
        .file_name()
        .context("clerk output filename missing")?;
    let existing = existing_parent(&root.directory, parent)?;
    root.check().context("clerk output host changed")?;
    if let Some(parent) = existing {
        missing(&parent, name)?;
    }
    Ok(())
}

fn existing_parent(root: &Dir, relative: &Path) -> Result<Option<Dir>> {
    let mut directory = root.try_clone()?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            anyhow::bail!("unsafe clerk output parent");
        };
        match directory.open_with(name, &directory_options()) {
            Ok(file) => directory = Dir::from_std_file(file.into_std()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("inspect clerk output parent"),
        }
    }
    Ok(Some(directory))
}

fn output_path(settings: &Settings, path: &Path) -> Result<(PathBuf, PathBuf)> {
    ensure!(
        path.to_str().is_some() && path.as_os_str().len() <= MAX_PATH,
        "clerk output path must be UTF-8 and at most 4096 bytes"
    );
    ensure!(
        path.components().all(|part| matches!(
            part,
            Component::RootDir | Component::CurDir | Component::Normal(_)
        )),
        "clerk output path must not contain traversal"
    );
    let base = settings.host().join(BASE);
    let relative = if path.is_absolute() {
        path.strip_prefix(&base)
            .context("clerk output must be below the output root")?
            .to_owned()
    } else {
        path.to_owned()
    };
    let name = relative
        .file_name()
        .context("clerk output must name a JSON file")?;
    ensure!(
        relative
            .components()
            .any(|part| matches!(part, Component::Normal(_)))
            && name != ".current_work.md"
            && Path::new(name).extension().is_some_and(|ext| ext == "json"),
        "clerk output must be a JSON file and not .current_work.md"
    );
    let output = base.join(&relative);
    ensure!(
        output.to_str().is_some() && output.as_os_str().len() <= MAX_PATH,
        "absolute clerk output path exceeds limit"
    );
    Ok((output, Path::new(BASE).join(relative)))
}

fn document_bytes(
    settings: &Settings,
    output: &Path,
    document: &Value,
) -> Result<(Vec<u8>, usize)> {
    validate_value(settings, document)?;
    ensure!(
        normalize_model_draft(&document["draft"])? == document["draft"],
        "clerk draft must be normalized before publication"
    );
    let sources = document["sources"]
        .as_array()
        .context("clerk sources missing")?;
    for source in sources {
        let path = source["path"]
            .as_str()
            .context("clerk source path missing")?;
        ensure!(
            settings.source_path(Path::new(path))? != output,
            "clerk output collides with a source"
        );
    }
    let mut bytes = serde_json::to_vec(document)?;
    ensure!(
        bytes.len() < MAX_DOCUMENT_BYTES,
        "clerk document exceeds publication limit"
    );
    bytes.push(b'\n');
    Ok((bytes, sources.len()))
}

fn walk(root: &Dir, relative: &Path, create: bool) -> Result<Dir> {
    let mut directory = root.try_clone()?;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            anyhow::bail!("unsafe clerk output parent");
        };
        if create {
            match directory.create_dir_with(name, DirBuilder::new().mode(0o700)) {
                Ok(()) => directory.open(".")?.sync_all()?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error.into()),
            }
        }
        directory = Dir::from_std_file(directory.open_with(name, &directory_options())?.into_std());
    }
    Ok(directory)
}
fn missing(parent: &Dir, name: &std::ffi::OsStr) -> Result<()> {
    match parent.symlink_metadata(name) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => anyhow::bail!("clerk output already exists"),
    }
}
fn same_stage(parent: &Dir, name: &std::ffi::OsStr, held: &File) -> Result<()> {
    let current = parent.symlink_metadata(name)?;
    let original = held.metadata()?;
    ensure!(
        current.is_file() && original.is_file() && same_inode(&current, &original),
        "clerk staging file changed"
    );
    Ok(())
}
fn recheck(
    settings: &Settings,
    relative_parent: &Path,
    parent: &Dir,
    name: &std::ffi::OsStr,
    stage: &std::ffi::OsStr,
    held: &File,
) -> Result<()> {
    let root = &settings.roots[0];
    root.check().context("clerk output host changed")?;
    let fresh = walk(&root.directory, relative_parent, false)?;
    ensure!(
        same_inode(&parent.dir_metadata()?, &fresh.dir_metadata()?),
        "clerk output parent changed"
    );
    same_stage(parent, stage, held)?;
    ensure!(
        held.metadata()?.nlink() == 1,
        "clerk staging file has unexpected hard links"
    );
    missing(parent, name)
}

fn stage(parent: &Dir, bytes: &[u8]) -> Result<(OsString, File)> {
    let name = OsString::from(format!(".{}", crate::resource_id("clerk-pending")?));
    let mut file = parent.open_with(
        &name,
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .mode(0o600),
    )?;
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(error) = result {
        let cleanup = same_stage(parent, &name, &file)
            .and_then(|()| parent.remove_file(&name).map_err(Into::into));
        return match cleanup {
            Ok(()) => Err(error.into()),
            Err(cleanup) => Err(anyhow::anyhow!(
                "clerk staging write failed: {error}; cleanup failed: {cleanup:#}"
            )),
        };
    }
    Ok((name, file))
}
fn remove_stage(parent: &Dir, name: &std::ffi::OsStr, held: &File) -> Result<()> {
    same_stage(parent, name, held)?;
    parent
        .remove_file(name)
        .context("remove owned clerk staging file")
}
fn write_checked(
    settings: &Settings,
    path: &Path,
    document: &Value,
    boundary: impl FnOnce(&Path) -> Result<()>,
) -> Result<Value> {
    let (output, relative) = output_path(settings, path)?;
    let (bytes, sources) = document_bytes(settings, &output, document)?;
    let root = &settings.roots[0];
    root.check().context("clerk output host changed")?;
    let parent_path = relative.parent().context("clerk output parent missing")?;
    let name = relative
        .file_name()
        .context("clerk output filename missing")?;
    let parent = walk(&root.directory, parent_path, true)?;
    missing(&parent, name)?;
    let (stage_name, held) = stage(&parent, &bytes)?;
    let result = (|| -> Result<()> {
        boundary(&output)?;
        recheck(settings, parent_path, &parent, name, &stage_name, &held)?;
        parent
            .hard_link(&stage_name, &parent, name)
            .context("publish clerk draft without overwrite")?;
        Ok(())
    })();
    let cleanup = remove_stage(&parent, &stage_name, &held);
    match (result, cleanup) {
        (Err(error), Err(cleanup)) => anyhow::bail!(
            "clerk publication failed: {error:#}; staging cleanup failed: {cleanup:#}"
        ),
        (Err(error), Ok(())) => return Err(error),
        (Ok(()), Err(cleanup)) => {
            anyhow::bail!("clerk draft published but staging cleanup failed: {cleanup:#}")
        }
        (Ok(()), Ok(())) => (),
    }
    parent
        .open(".")
        .and_then(|directory| directory.sync_all())
        .context("clerk draft published but parent sync failed")?;
    Ok(
        json!({"written":output.to_str().context("clerk output path must be UTF-8")?,"sources":sources}),
    )
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
