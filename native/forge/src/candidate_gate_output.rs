//! Prepare an explicit review-artifact parent without following symlinks.
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, DirBuilder, DirBuilderExt, OpenOptions, OpenOptionsExt};
use std::{
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};
pub(super) fn receipt(root: &Path, requested: &Path) -> Result<PathBuf> {
    let output = if requested.is_absolute() {
        requested.to_owned()
    } else {
        root.join(requested)
    };
    ensure!(
        output.is_absolute() && output.as_os_str().as_bytes().len() <= 4096,
        "invalid review artifact path"
    );
    let parent = output.parent().context("review artifact has no parent")?;
    let mut dir = Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    for part in parent.components() {
        let name = match part {
            Component::RootDir => continue,
            Component::Normal(name) => name,
            _ => anyhow::bail!("review artifact path contains traversal"),
        };
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let opened = match dir.open_with(name, &options) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                dir.create_dir_with(name, DirBuilder::new().mode(0o700))?;
                dir.open_with(name, &options)?
            }
            Err(error) => return Err(error.into()),
        };
        dir = Dir::from_std_file(opened.into_std());
    }
    ensure!(
        output.file_name().is_some(),
        "review artifact must name a file"
    );
    Ok(output)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};
    #[test]
    fn output_parent_is_created_without_following_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let output = receipt(&root, Path::new("tasks/audit/gate.json")).unwrap();
        assert_eq!(output, root.join("tasks/audit/gate.json"));
        assert!(output.parent().unwrap().is_dir());
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.join("link")).unwrap();
        assert!(receipt(&root, Path::new("link/gate.json")).is_err());
        assert!(receipt(&root, Path::new("../outside.json")).is_err());
        assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
    }
}
