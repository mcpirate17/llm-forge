//! Private standalone Git-object snapshots. No Git child, checkout or worktree.
mod paths;
#[cfg(test)]
mod tests;

use crate::candidate_source::{Source, TreeEntry};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{
    Dir, DirBuilder, DirBuilderExt, MetadataExt, OpenOptions, OpenOptionsExt, Permissions,
    PermissionsExt,
};
use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::{CString, OsString};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_blob_bytes: u64,
    pub max_tree_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_blob_bytes: 64 * 1024 * 1024,
            max_tree_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}
impl Limits {
    fn validate(self) -> Result<()> {
        let maximum = Self::default();
        ensure!(
            self.max_blob_bytes <= maximum.max_blob_bytes
                && self.max_tree_bytes <= maximum.max_tree_bytes,
            "candidate snapshot limits exceed 64 MiB per blob or 2 GiB per tree"
        );
        Ok(())
    }
}

struct Plan {
    entries: Vec<TreeEntry>,
    links: BTreeMap<String, String>,
    bytes: u64,
    files: u64,
    skipped: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub tree_oid: String,
    pub output: PathBuf,
    pub entries: usize,
    pub materialized_files: u64,
    pub materialized_bytes: u64,
    pub skipped_gitlinks: u64,
    pub published: bool,
}

struct Destination {
    parent: Dir,
    parent_path: PathBuf,
    name: OsString,
}
impl Destination {
    fn open(output: &Path) -> Result<Self> {
        let name = output
            .file_name()
            .context("candidate output needs a new directory name")?
            .to_owned();
        ensure!(
            matches!(
                Path::new(&name).components().next(),
                Some(Component::Normal(_))
            ) && !name.as_bytes().eq_ignore_ascii_case(b".git"),
            "invalid candidate output directory name"
        );
        let parent_path = output
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .canonicalize()?;
        let parent = Dir::open_ambient_dir(&parent_path, cap_std::ambient_authority())?;
        ensure!(
            parent.metadata(".")?.is_dir(),
            "candidate output parent is not a directory"
        );
        match parent.symlink_metadata(&name) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
            Ok(_) => anyhow::bail!("candidate output already exists"),
        }
        Ok(Self {
            parent,
            parent_path,
            name,
        })
    }
    fn output(&self) -> PathBuf {
        self.parent_path.join(&self.name)
    }
    fn verify_parent(&self) -> Result<()> {
        let held = self.parent.metadata(".")?;
        let current = std::fs::metadata(&self.parent_path)?;
        use std::os::unix::fs::MetadataExt as _;
        ensure!(
            held.dev() == current.dev() && held.ino() == current.ino(),
            "candidate output parent changed"
        );
        Ok(())
    }
}

pub struct Snapshot {
    destination: Destination,
    staging: OsString,
    directory: Dir,
    identity: (u64, u64),
    report: Report,
    keep: bool,
}

pub fn preflight(source: &Source, tree: &str, output: &Path, limits: Limits) -> Result<Report> {
    let destination = Destination::open(output)?;
    let plan = paths::prepare(source, tree, limits)?;
    Ok(report(tree, &destination, &plan))
}

fn report(tree: &str, destination: &Destination, plan: &Plan) -> Report {
    Report {
        tree_oid: tree.to_ascii_lowercase(),
        output: destination.output(),
        entries: plan.entries.len(),
        materialized_files: plan.files,
        materialized_bytes: plan.bytes,
        skipped_gitlinks: plan.skipped,
        published: false,
    }
}

impl Snapshot {
    pub fn create(source: &Source, tree: &str, output: &Path, limits: Limits) -> Result<Self> {
        let destination = Destination::open(output)?;
        let plan = paths::prepare(source, tree, limits)?;
        let staging = OsString::from(format!(".{}", crate::native_id::new("forge-candidate")?));
        destination
            .parent
            .create_dir_with(&staging, DirBuilder::new().mode(0o700))?;
        let opened = destination.parent.open_with(
            &staging,
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW),
        );
        let directory = match opened {
            Ok(directory) => Dir::from_std_file(directory.into_std()),
            Err(error) => {
                if let Err(cleanup) = destination.parent.remove_dir(&staging) {
                    eprintln!("forge candidate staging cleanup: {cleanup}");
                }
                return Err(error.into());
            }
        };
        let identity = staging_identity(&destination.parent, &staging, directory.metadata("."))?;
        let mut snapshot = Self {
            report: report(tree, &destination, &plan),
            destination,
            staging,
            directory,
            identity,
            keep: false,
        };
        snapshot.populate(source, &plan)?;
        Ok(snapshot)
    }
    /// Private staging directory, removed on drop unless explicitly published.
    pub fn path(&self) -> PathBuf {
        self.destination.parent_path.join(&self.staging)
    }
    pub fn report(&self) -> &Report {
        &self.report
    }
    pub(crate) fn directory(&self) -> Result<Dir> {
        self.verify_staging()?;
        Ok(self.directory.try_clone()?)
    }

    /// Explicit cleanup lets callers refuse success when a private snapshot remains.
    pub fn discard(self) -> Result<()> {
        self.discard_using(|parent, name| parent.remove_dir_all(name))
    }

    fn discard_using(
        mut self,
        remove: impl FnOnce(&Dir, &std::ffi::OsStr) -> std::io::Result<()>,
    ) -> Result<()> {
        // Explicit discard reports failures without a second destructive Drop
        // attempt; retain any remaining staging state for recovery.
        self.keep = true;
        self.verify_staging()?;
        remove(&self.destination.parent, &self.staging)?;
        Ok(())
    }

    /// Preserve an unpublished private snapshot after incomplete process cleanup.
    /// The retained path is recovery evidence; it is never a successful result.
    pub(crate) fn retain_for_recovery(self) -> Result<Report> {
        self.retain_owned()
    }

    /// Preserve a completed task's owned workspace. The tree OID describes
    /// only its preimage; callers must separately bind selected output bytes.
    pub(crate) fn retain_task_output(self) -> Result<Report> {
        self.retain_owned()
    }

    fn retain_owned(mut self) -> Result<Report> {
        self.keep = true;
        self.verify_staging()?;
        self.report.output = self.path();
        Ok(self.report.clone())
    }

    fn populate(&mut self, source: &Source, plan: &Plan) -> Result<()> {
        for entry in &plan.entries {
            if entry.mode == "160000" {
                continue;
            }
            let parent = self.parent_for(&entry.path)?;
            let name = Path::new(&entry.path)
                .file_name()
                .context("candidate filename missing")?;
            if entry.mode == "120000" {
                continue;
            }
            let blob = source
                .repository()
                .find_blob(git2::Oid::from_str(&entry.oid)?)?;
            ensure!(
                Some(blob.size() as u64) == entry.size,
                "candidate blob size mismatch"
            );
            let mut file = parent.open_with(
                name,
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .mode(0o600),
            )?;
            file.write_all(blob.content())?;
            file.set_permissions(Permissions::from_mode(if entry.mode == "100755" {
                0o755
            } else {
                0o644
            }))?;
            file.sync_all()?;
            parent.open(".")?.sync_all()?;
        }
        // All ordinary paths exist before any source-controlled symlink does.
        for (path, target) in &plan.links {
            let parent = self.parent_for(path)?;
            parent.symlink_contents(
                target,
                Path::new(path)
                    .file_name()
                    .context("symlink name missing")?,
            )?;
            parent.open(".")?.sync_all()?;
        }
        self.directory.open(".")?.sync_all()?;
        Ok(())
    }

    fn parent_for(&self, path: &str) -> Result<Dir> {
        let mut directory = self.directory.try_clone()?;
        let parent = Path::new(path)
            .parent()
            .context("candidate file has no parent")?;
        for component in parent.components() {
            let Component::Normal(name) = component else {
                anyhow::bail!("unsafe candidate parent");
            };
            match directory.create_dir_with(name, DirBuilder::new().mode(0o755)) {
                Ok(()) => directory.open(".")?.sync_all()?,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error.into()),
            }
            let opened = directory.open_with(
                name,
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW),
            )?;
            directory = Dir::from_std_file(opened.into_std());
        }
        Ok(directory)
    }

    pub fn publish(mut self) -> Result<Report> {
        self.destination.verify_parent()?;
        self.verify_staging()?;
        let from = CString::new(self.staging.as_bytes())?;
        let to = CString::new(self.destination.name.as_bytes())?;
        let fd = self.destination.parent.as_raw_fd();
        ensure!(
            unsafe { libc::renameat2(fd, from.as_ptr(), fd, to.as_ptr(), libc::RENAME_NOREPLACE) }
                == 0,
            "publish candidate snapshot: {}",
            std::io::Error::last_os_error()
        );
        self.keep = true;
        self.report.published = true;
        self.destination
            .parent
            .open(".")?
            .sync_all()
            .context("candidate snapshot published but parent sync failed")?;
        Ok(self.report.clone())
    }

    fn verify_staging(&self) -> Result<()> {
        let meta = self.destination.parent.symlink_metadata(&self.staging)?;
        ensure!(
            meta.is_dir() && (meta.dev(), meta.ino()) == self.identity,
            "candidate staging directory changed"
        );
        Ok(())
    }
}

fn staging_identity(
    parent: &Dir,
    staging: &std::ffi::OsStr,
    metadata: std::io::Result<cap_std::fs::Metadata>,
) -> Result<(u64, u64)> {
    match metadata {
        Ok(meta) => Ok((meta.dev(), meta.ino())),
        Err(error) => {
            if let Err(cleanup) = parent.remove_dir(staging) {
                eprintln!("forge candidate staging cleanup: {cleanup}");
            }
            Err(error.into())
        }
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let result = self.verify_staging().and_then(|()| {
            self.destination
                .parent
                .remove_dir_all(&self.staging)
                .map_err(Into::into)
        });
        if let Err(error) = result {
            eprintln!("forge candidate snapshot cleanup: {error:#}");
        }
    }
}
