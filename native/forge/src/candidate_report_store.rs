//! Atomic report exports under an operator-controlled directory tree.
//! Cooperating publishers serialize with a parent-directory flock. This may
//! wait for another publisher; callers must not already hold that directory lock.
//! Parent components are pinned without following symlinks. Moving or replacing
//! a pinned parent, or swapping final entries between checks, by the same UID is
//! outside the trusted-parent contract; rename never opens or truncates the leaf.
use anyhow::{bail, ensure, Context, Result};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, File, MetadataExt, OpenOptions, OpenOptionsExt};
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_PATH: usize = 4096;

fn open_parent(path: &Path) -> Result<(Dir, OsString)> {
    let raw = path.as_os_str().as_bytes();
    ensure!(
        !raw.is_empty() && raw.len() <= MAX_PATH && !raw.contains(&0),
        "invalid report path"
    );
    ensure!(
        raw != b"." && !raw.ends_with(b"/") && !raw.ends_with(b"/."),
        "report path must name a file"
    );
    let name = path
        .file_name()
        .context("report path must name a file")?
        .to_os_string();
    ensure!(
        name.len() <= 255 && name != "." && name != "..",
        "invalid report filename"
    );
    let base = if path.is_absolute() {
        Path::new("/")
    } else {
        Path::new(".")
    };
    let mut directory = Dir::open_ambient_dir(base, ambient_authority())?;
    for part in path.parent().context("report parent missing")?.components() {
        match part {
            Component::RootDir | Component::CurDir => (),
            Component::Normal(name) => {
                ensure!(
                    name.len() <= 255,
                    "report parent component exceeds 255 bytes"
                );
                let mut opts = OpenOptions::new();
                opts.read(true).custom_flags(
                    libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                );
                directory = Dir::from_std_file(directory.open_with(name, &opts)?.into_std());
            }
            _ => bail!("report path must not contain traversal"),
        }
    }
    Ok((directory, name))
}

fn check_destination(parent: &Dir, name: &OsStr) -> Result<()> {
    match parent.symlink_metadata(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(meta) => {
            ensure!(
                meta.is_file() && meta.uid() == unsafe { libc::geteuid() } && meta.nlink() == 1,
                "existing report must be an owned singly linked regular file (mode={:o}, uid={}, links={})",
                meta.mode(), meta.uid(), meta.nlink()
            );
            Ok(())
        }
    }
}

fn same_stage(parent: &Dir, name: &OsStr, held: &File) -> Result<()> {
    let current = parent.symlink_metadata(name)?;
    let original = held.metadata()?;
    ensure!(
        current.is_file()
            && original.is_file()
            && current.nlink() == 1
            && original.nlink() == 1
            && current.uid() == unsafe { libc::geteuid() }
            && (current.dev(), current.ino()) == (original.dev(), original.ino()),
        "report staging inode changed"
    );
    Ok(())
}

fn stage(parent: &Dir, bytes: &[u8]) -> Result<(OsString, File)> {
    let name = OsString::from(format!(".{}", crate::resource_id("report-pending")?));
    let mut file = parent.open_with(
        &name,
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
    )?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        return match cleanup(parent, &name, &file) {
            Ok(()) => Err(error).context("write report staging file"),
            Err(cleanup) => Err(anyhow::anyhow!(
                "report staging write failed: {error}; cleanup failed: {cleanup:#}"
            )),
        };
    }
    Ok((name, file))
}

fn cleanup(parent: &Dir, name: &OsStr, file: &File) -> Result<()> {
    same_stage(parent, name, file)?;
    parent
        .remove_file(name)
        .context("remove owned report staging file")
}

/// Atomically replace an owned report with at most 16 MiB of complete bytes.
pub fn publish(path: &Path, bytes: &[u8]) -> Result<()> {
    publish_checked(path, bytes, || Ok(()))
}

/// Revalidate caller-held inputs after staging and immediately before replacement.
/// The parent lock serializes cooperating publishers, not arbitrary same-UID writes.
pub(crate) fn publish_checked(
    path: &Path,
    bytes: &[u8],
    before_rename: impl FnOnce() -> Result<()>,
) -> Result<()> {
    publish_locked(path, bytes, |_| Ok(()), before_rename)
}

struct PairOutput<'a> {
    path: &'a Path,
    parent: Dir,
    name: OsString,
    staged: Option<(OsString, File)>,
    renamed: bool,
    durable: bool,
}

impl<'a> PairOutput<'a> {
    fn new(path: &'a Path) -> Result<Self> {
        let (parent, name) = open_parent(path)?;
        Ok(Self {
            path,
            parent,
            name,
            staged: None,
            renamed: false,
            durable: false,
        })
    }

    fn parent_identity(&self) -> Result<(u64, u64)> {
        let meta = self.parent.dir_metadata()?;
        Ok((meta.dev(), meta.ino()))
    }

    fn verify_staged(&self) -> Result<()> {
        let (name, held) = self
            .staged
            .as_ref()
            .context("paired staging file missing")?;
        same_stage(&self.parent, name, held)?;
        check_destination(&self.parent, &self.name)
    }

    fn replace(
        &mut self,
        rename: &mut impl FnMut(&Dir, &OsStr, &OsStr) -> Result<()>,
    ) -> Result<()> {
        let (name, _) = self
            .staged
            .as_ref()
            .context("paired staging file missing")?;
        rename(&self.parent, name, &self.name)?;
        self.renamed = true;
        self.staged = None;
        self.parent
            .open(".")?
            .sync_all()
            .context("paired output replaced but directory sync failed")?;
        self.durable = true;
        Ok(())
    }

    fn cleanup_staged(&mut self) -> Result<()> {
        if let Some((name, held)) = self.staged.take() {
            cleanup(&self.parent, &name, &held).with_context(|| {
                format!("remove paired staging file {name:?} for {:?}", self.path)
            })?;
        }
        Ok(())
    }

    fn disposition(&self) -> String {
        format!(
            "path={:?} renamed={} durable={}",
            self.path, self.renamed, self.durable
        )
    }
}

fn pair_leases(
    first: &PairOutput<'_>,
    second: &PairOutput<'_>,
) -> Result<Vec<crate::file_lock::Exclusive>> {
    let first_id = first.parent_identity()?;
    let second_id = second.parent_identity()?;
    ensure!(
        first_id != second_id || first.name != second.name,
        "paired outputs must name distinct files"
    );
    let mut parents = vec![(first_id, &first.parent), (second_id, &second.parent)];
    parents.sort_by_key(|(identity, _)| *identity);
    parents.dedup_by_key(|(identity, _)| *identity);
    parents
        .into_iter()
        .map(|(_, parent)| {
            crate::file_lock::Exclusive::blocking(
                parent.open(".")?.into_std(),
                "paired report parent directory",
            )
        })
        .collect()
}

/// Stage both outputs, then replace first followed by second under ordered parent locks.
/// Two renames are not transactional. Errors identify each output's replacement and
/// durability state; callers must not consume inputs unless this returns success.
pub(crate) fn publish_pair_checked(
    first: (&Path, &[u8]),
    second: (&Path, &[u8]),
    before_rename: impl FnOnce() -> Result<()>,
) -> Result<()> {
    publish_pair_using(first, second, before_rename, stage, |parent, from, to| {
        parent
            .rename(from, parent, to)
            .context("rename paired report staging file")
    })
}

fn publish_pair_using(
    first: (&Path, &[u8]),
    second: (&Path, &[u8]),
    before_rename: impl FnOnce() -> Result<()>,
    mut stage_file: impl FnMut(&Dir, &[u8]) -> Result<(OsString, File)>,
    mut rename: impl FnMut(&Dir, &OsStr, &OsStr) -> Result<()>,
) -> Result<()> {
    ensure!(
        first.1.len() <= MAX_BYTES && second.1.len() <= MAX_BYTES,
        "paired report exceeds 16 MiB per output; neither output replaced"
    );
    let mut first_output = PairOutput::new(first.0)?;
    let mut second_output = PairOutput::new(second.0)?;
    let _leases = pair_leases(&first_output, &second_output)?;
    let result = (|| -> Result<()> {
        check_destination(&first_output.parent, &first_output.name)?;
        check_destination(&second_output.parent, &second_output.name)?;
        first_output.staged = Some(stage_file(&first_output.parent, first.1)?);
        second_output.staged = Some(stage_file(&second_output.parent, second.1)?);
        first_output.verify_staged()?;
        second_output.verify_staged()?;
        before_rename()?;
        first_output.replace(&mut rename)?;
        second_output.verify_staged()?;
        second_output.replace(&mut rename)
    })();
    if let Err(error) = result {
        let cleanup = [
            first_output.cleanup_staged(),
            second_output.cleanup_staged(),
        ]
        .into_iter()
        .filter_map(|result| result.err().map(|error| format!("{error:#}")))
        .collect::<Vec<_>>();
        bail!(
            "paired publication failed: {error:#}; first {}; second {}; staging cleanup errors={cleanup:?}",
            first_output.disposition(),
            second_output.disposition()
        );
    }
    Ok(())
}

fn publish_locked(
    path: &Path,
    bytes: &[u8],
    after_lock: impl FnOnce(&Dir) -> Result<()>,
    before_rename: impl FnOnce() -> Result<()>,
) -> Result<()> {
    ensure!(bytes.len() <= MAX_BYTES, "report exceeds 16 MiB");
    let (parent, name) = open_parent(path)?;
    // Reopening gives this publisher its own open-file description; cloning a
    // descriptor shared with a peer would not serialize their flock ownership.
    let _lease = crate::file_lock::Exclusive::blocking(
        parent.open(".")?.into_std(),
        "report parent directory",
    )?;
    after_lock(&parent)?;
    check_destination(&parent, &name)?;
    let (stage_name, held) = stage(&parent, bytes)?;
    let result = (|| -> Result<()> {
        same_stage(&parent, &stage_name, &held)?;
        check_destination(&parent, &name)?;
        before_rename()?;
        parent
            .rename(&stage_name, &parent, &name)
            .context("rename report staging file")?;
        Ok(())
    })();
    if let Err(error) = result {
        return match cleanup(&parent, &stage_name, &held) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(anyhow::anyhow!(
                "report publish failed: {error:#}; cleanup failed: {cleanup:#}"
            )),
        };
    }
    parent
        .open(".")
        .and_then(|directory| directory.sync_all())
        .context("report published but directory sync failed")?;
    Ok(())
}

#[cfg(test)]
#[path = "candidate_report_store_tests.rs"]
mod tests;
