//! Shared native check for exact regular candidate files without symlink aliases.
//! Callers pass a canonical root and retain their existing diagnostic context.
use anyhow::{bail, Context, Result};
use std::{fs, path::Path};

pub fn require_regular_file(root: &Path, relative: &str) -> Result<()> {
    let file = root.join(relative);
    let metadata = fs::symlink_metadata(&file)
        .with_context(|| format!("mapped contract file missing: {}", file.display()))?;
    if !metadata.file_type().is_file() {
        bail!("mapped contract file is not regular: {}", file.display());
    }
    let canonical = file
        .canonicalize()
        .with_context(|| format!("cannot resolve mapped contract file: {}", file.display()))?;
    if canonical != file {
        bail!("mapped contract file escapes its crate: {}", file.display());
    }
    Ok(())
}
