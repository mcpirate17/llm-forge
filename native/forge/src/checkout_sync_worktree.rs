//! Cooperative, journalled worktree leaf transitions; not a complete actor.
//!
//! All affected-directory writers must hold SyncLease. Arbitrary editors that
//! ignore it can race the final identity check and rename; Linux has no
//! inode-conditional rename. Retained slot inodes and WAL permit detection and
//! conditional recovery but do not make the operation atomic to such editors.
use crate::{
    candidate_review_lock::ReviewLock,
    checkout_sync_journal::{Event, Identity, Journal},
};
use anyhow::{bail, ensure, Context, Result};
use cap_std::fs::{
    Dir, DirBuilder, DirBuilderExt, MetadataExt, OpenOptions, OpenOptionsExt, Permissions,
    PermissionsExt,
};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CString, OsStr},
    io::{Read, Write},
    os::{fd::AsRawFd, unix::ffi::OsStrExt},
    path::{Component, Path, PathBuf},
    time::Duration,
};

const MAX_FILE: usize = 64 * 1024 * 1024;

pub(crate) struct SyncLease(ReviewLock);
impl SyncLease {
    pub(crate) fn acquire(repo: &Path, cancel: &mut dyn FnMut() -> Result<()>) -> Result<Self> {
        Ok(Self(ReviewLock::acquire_exclusive_cancellable(
            repo,
            Duration::from_secs(10),
            cancel,
        )?))
    }
    pub(crate) fn verify(&self) -> Result<()> {
        self.0.verify()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Content {
    File {
        bytes: Vec<u8>,
        mode: u32,
    },
    Symlink(Vec<u8>),
    /// Directories are leaf transitions only when empty. The orchestrator must
    /// schedule child removal/creation in the right order and journal each one.
    EmptyDirectory {
        mode: u32,
    },
}
#[derive(Clone, Debug)]
pub(crate) struct Observed {
    pub identity: Identity,
    pub bytes: Vec<u8>,
}

pub(crate) struct Root {
    path: PathBuf,
    dir: Dir,
    identity: (u64, u64),
}
impl Root {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        let dir = Dir::open_ambient_dir(&path, cap_std::ambient_authority())?;
        let m = dir.dir_metadata()?;
        ensure!(m.is_dir(), "worktree root is not a directory");
        Ok(Self {
            path,
            dir,
            identity: (m.dev(), m.ino()),
        })
    }
    fn verify(&self) -> Result<()> {
        use std::os::unix::fs::MetadataExt as _;
        let held = self.dir.dir_metadata()?;
        let current = std::fs::symlink_metadata(&self.path)?;
        ensure!(
            held.is_dir()
                && current.is_dir()
                && (held.dev(), held.ino()) == self.identity
                && (current.dev(), current.ino()) == self.identity,
            "worktree root changed"
        );
        Ok(())
    }
    fn parent(&self, relative: &str) -> Result<(Dir, String)> {
        crate::candidate_source::safe_path(relative.as_bytes())?;
        ensure!(
            !relative
                .split('/')
                .any(|part| part.eq_ignore_ascii_case(".git")),
            "worktree transitions cannot target Git metadata"
        );
        self.verify()?;
        let path = Path::new(relative);
        let name = path.file_name().context("path has no name")?;
        let name = name.to_str().context("path name is not UTF-8")?.to_owned();
        let mut dir = self.dir.try_clone()?;
        for component in path.parent().context("path parent missing")?.components() {
            let Component::Normal(name) = component else {
                bail!("unsafe parent component")
            };
            let before = dir.symlink_metadata(name)?;
            ensure!(before.is_dir(), "worktree parent is not a real directory");
            let next = dir.open_dir(name)?;
            let after = next.dir_metadata()?;
            ensure!(
                (before.dev(), before.ino()) == (after.dev(), after.ino()),
                "worktree parent changed"
            );
            dir = next;
        }
        Ok((dir, name))
    }
    pub(crate) fn observe(&self, relative: &str, lease: &SyncLease) -> Result<Option<Observed>> {
        lease.verify()?;
        let (parent, name) = self.parent(relative)?;
        let result = inspect(&parent, &name)?;
        self.verify_parent(relative, &parent)?;
        lease.verify()?;
        Ok(result)
    }
    /// Observe a populated directory after its planned children were published.
    /// Its contents are checked through their own leaf records, not an empty
    /// directory precondition used by the rename primitive.
    pub(crate) fn observe_directory_identity(
        &self,
        relative: &str,
        lease: &SyncLease,
    ) -> Result<Identity> {
        lease.verify()?;
        let (parent, name) = self.parent(relative)?;
        let before = parent.symlink_metadata(&name)?;
        ensure!(
            before.is_dir(),
            "published directory is no longer a directory"
        );
        let child = parent.open_dir(&name)?;
        let held = child.dir_metadata()?;
        let after = parent.symlink_metadata(&name)?;
        ensure!(
            held.is_dir()
                && after.is_dir()
                && (before.dev(), before.ino(), before.mode())
                    == (held.dev(), held.ino(), held.mode())
                && (held.dev(), held.ino(), held.mode())
                    == (after.dev(), after.ino(), after.mode()),
            "published directory changed during observation"
        );
        self.verify_parent(relative, &parent)?;
        lease.verify()?;
        Ok(Identity {
            device: held.dev(),
            inode: held.ino(),
            mode: held.mode(),
            length: 0,
            sha256: format!("{:x}", Sha256::digest([])),
        })
    }
    fn verify_parent(&self, relative: &str, held: &Dir) -> Result<()> {
        let (current, _) = self.parent(relative)?;
        let a = held.dir_metadata()?;
        let b = current.dir_metadata()?;
        ensure!(
            (a.dev(), a.ino()) == (b.dev(), b.ino()),
            "worktree parent directory changed"
        );
        Ok(())
    }
}

/// Same-filesystem staging depot outside every target subtree. It is never
/// removed automatically; successful completion or recovery reports its name.
pub(crate) struct Depot {
    name: String,
    dir: Dir,
    identity: (u64, u64),
}
impl Depot {
    pub(crate) fn create(root: &Root, lease: &SyncLease, journal: &mut Journal) -> Result<Self> {
        lease.verify()?;
        root.verify()?;
        let name = crate::resource_id("checkout-sync-depot")?;
        ensure!(
            inspect(&root.dir, &name)?.is_none(),
            "recovery depot already exists"
        );
        journal.append(Event::BeforeDepot { name: name.clone() })?;
        lease.verify()?;
        root.verify()?;
        root.dir
            .create_dir_with(&name, DirBuilder::new().mode(0o700))?;
        let dir = root.dir.open_dir(&name)?;
        dir.set_permissions(".", Permissions::from_mode(0o700))?;
        sync_dir(&dir)?;
        sync_dir(&root.dir)?;
        let m = dir.dir_metadata()?;
        ensure!(
            m.is_dir() && m.uid() == unsafe { libc::geteuid() } && m.mode() & 0o777 == 0o700,
            "unsafe recovery depot"
        );
        let depot = Self {
            name: name.clone(),
            dir,
            identity: (m.dev(), m.ino()),
        };
        depot.verify(root, lease)?;
        journal.append(Event::AfterDepot {
            name,
            device: m.dev(),
            inode: m.ino(),
        })?;
        Ok(depot)
    }
    pub(crate) fn open(
        root: &Root,
        lease: &SyncLease,
        name: &str,
        identity: (u64, u64),
    ) -> Result<Self> {
        ensure!(
            name.starts_with("checkout-sync-depot-")
                && name.len() <= 255
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
            "invalid depot name"
        );
        let before = root.dir.symlink_metadata(name)?;
        ensure!(before.is_dir(), "recovery depot is not a directory");
        let dir = root.dir.open_dir(name)?;
        let depot = Self {
            name: name.into(),
            dir,
            identity,
        };
        depot.verify(root, lease)?;
        Ok(depot)
    }
    fn verify(&self, root: &Root, lease: &SyncLease) -> Result<()> {
        lease.verify()?;
        root.verify()?;
        let held = self.dir.dir_metadata()?;
        let named = root.dir.symlink_metadata(&self.name)?;
        ensure!(
            held.is_dir()
                && named.is_dir()
                && held.uid() == unsafe { libc::geteuid() }
                && held.mode() & 0o777 == 0o700
                && (held.dev(), held.ino()) == self.identity
                && (named.dev(), named.ino()) == self.identity,
            "recovery depot changed"
        );
        Ok(())
    }
    fn duplicate(&self) -> Result<Self> {
        Ok(Self {
            name: self.name.clone(),
            dir: self.dir.try_clone()?,
            identity: self.identity,
        })
    }
}

fn inspect(dir: &Dir, name: &str) -> Result<Option<Observed>> {
    let before = match dir.symlink_metadata(name) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        before.nlink() == 1 || before.is_dir(),
        "hard-linked worktree entry refused"
    );
    let bytes = if before.is_file() {
        ensure!(
            before.len() <= MAX_FILE as u64,
            "worktree file exceeds limit"
        );
        let mut file = dir.open_with(
            name,
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC),
        )?;
        let opened = file.metadata()?;
        ensure!(
            (before.dev(), before.ino()) == (opened.dev(), opened.ino()),
            "worktree file changed before read"
        );
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_FILE as u64 + 1)
            .read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        ensure!(
            bytes.len() <= MAX_FILE && stamp(&before) == stamp(&after),
            "worktree file changed while reading"
        );
        bytes
    } else if before.file_type().is_symlink() {
        let target = dir.read_link_contents(name)?;
        let bytes = target.as_os_str().as_bytes().to_vec();
        ensure!(
            bytes.len() <= 4096 && !bytes.contains(&0),
            "invalid symlink target"
        );
        bytes
    } else if before.is_dir() {
        let child = dir.open_dir(name)?;
        let m = child.dir_metadata()?;
        ensure!(
            (m.dev(), m.ino()) == (before.dev(), before.ino()),
            "directory changed before inspection"
        );
        ensure!(
            child.entries()?.next().is_none(),
            "directory transition requires an empty directory"
        );
        Vec::new()
    } else {
        bail!("unsupported worktree entry type")
    };
    let after = dir.symlink_metadata(name)?;
    ensure!(
        stamp(&before) == stamp(&after),
        "worktree entry changed while observing"
    );
    Ok(Some(Observed {
        identity: Identity {
            device: before.dev(),
            inode: before.ino(),
            mode: before.mode(),
            // Directory allocation size may change after children are removed;
            // identity still binds its inode/mode and an enforced empty listing.
            length: if before.is_dir() { 0 } else { before.len() },
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        },
        bytes,
    }))
}
fn stamp(m: &cap_std::fs::Metadata) -> (u64, u64, u32, u64, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.mode(),
        m.nlink(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
fn sync_dir(dir: &Dir) -> Result<()> {
    // cap-std may hold an O_PATH directory capability, which cannot be fsynced.
    // Open a readable descriptor relative to that capability, preserving the
    // bound directory rather than resolving an ambient pathname again.
    dir.open_with(
        ".",
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC),
    )?
    .sync_all()?;
    Ok(())
}
fn rename(from_dir: &Dir, from: &str, to_dir: &Dir, to: &str, flags: u32) -> Result<()> {
    let from = CString::new(from)?;
    let to = CString::new(to)?;
    let result = unsafe {
        libc::renameat2(
            from_dir.as_raw_fd(),
            from.as_ptr(),
            to_dir.as_raw_fd(),
            to.as_ptr(),
            flags,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("atomic worktree rename");
    }
    Ok(())
}
fn same(observed: &Option<Observed>, identity: &Option<Identity>) -> bool {
    observed.as_ref().map(|v| &v.identity) == identity.as_ref()
}

pub(crate) struct Prepared {
    path: String,
    parent: Dir,
    parent_identity: (u64, u64),
    depot: Depot,
    name: String,
    slot: String,
    original: Option<Observed>,
    staged: Option<Observed>,
    original_blob: Option<String>,
}
impl Prepared {
    /// Stage one leaf in its own directory. No live target is changed here.
    /// The original's bytes are journalled privately before an intent is emitted.
    pub(crate) fn prepare(
        root: &Root,
        depot: &Depot,
        lease: &SyncLease,
        journal: &mut Journal,
        path: &str,
        expected: Option<Identity>,
        content: Option<Content>,
    ) -> Result<Self> {
        lease.verify()?;
        let (parent, name) = root.parent(path)?;
        let m = parent.dir_metadata()?;
        depot.verify(root, lease)?;
        ensure!(
            m.dev() == depot.identity.0,
            "worktree target crosses recovery depot filesystem"
        );
        ensure!(
            path != depot.name && !path.starts_with(&format!("{}/", depot.name)),
            "target overlaps recovery depot"
        );
        let original = inspect(&parent, &name)?;
        ensure!(
            same(&original, &expected),
            "worktree leaf changed since planning"
        );
        ensure!(
            original.is_some() || content.is_some(),
            "empty worktree transition"
        );
        let slot = crate::resource_id("checkout-sync-slot")?;
        ensure!(
            inspect(&depot.dir, &slot)?.is_none(),
            "worktree staging slot exists"
        );
        let original_blob = match &original {
            Some(value) if !is_directory(&value.identity) => {
                journal.save_blob(&slot, &value.bytes)?;
                Some(slot.clone())
            }
            _ => None,
        };
        journal.append(Event::BeforeStage {
            path: path.into(),
            parent_device: m.dev(),
            parent_inode: m.ino(),
            slot: slot.clone(),
            original: original.as_ref().map(|v| v.identity.clone()),
            original_blob: original_blob.clone(),
            has_staged: content.is_some(),
        })?;
        lease.verify()?;
        root.verify_parent(path, &parent)?;
        ensure!(
            same(
                &inspect(&parent, &name)?,
                &original.as_ref().map(|v| v.identity.clone())
            ),
            "worktree entry changed after stage intent"
        );
        ensure!(
            inspect(&depot.dir, &slot)?.is_none(),
            "worktree slot appeared after stage intent"
        );
        if let Some(content) = content {
            stage(&depot.dir, &slot, content)
                .with_context(|| format!("staging slot retained: {path} parent slot {slot}"))?;
        }
        sync_dir(&depot.dir)?;
        let staged = inspect(&depot.dir, &slot)?;
        root.verify_parent(path, &parent)?;
        depot.verify(root, lease)?;
        journal.append(Event::AfterStage {
            path: path.into(),
            staged: staged.as_ref().map(|v| v.identity.clone()),
        })?;
        Ok(Self {
            path: path.into(),
            parent,
            parent_identity: (m.dev(), m.ino()),
            depot: depot.duplicate()?,
            name,
            slot,
            original,
            staged,
            original_blob,
        })
    }
    /// WAL intent is durable before the first rename. On any error after intent,
    /// keep this slot and journal for recovery; this method never cleans it up.
    pub(crate) fn publish(
        &self,
        root: &Root,
        lease: &SyncLease,
        journal: &mut Journal,
    ) -> Result<()> {
        self.publish_inner(root, lease, journal, |_| Ok(()))
    }
    fn publish_inner<F>(
        &self,
        root: &Root,
        lease: &SyncLease,
        journal: &mut Journal,
        mut hook: F,
    ) -> Result<()>
    where
        F: FnMut(&str) -> Result<()>,
    {
        lease.verify()?;
        root.verify_parent(&self.path, &self.parent)?;
        ensure!(
            same(
                &inspect(&self.parent, &self.name)?,
                &self.original.as_ref().map(|x| x.identity.clone())
            ),
            "worktree target changed before intent"
        );
        ensure!(
            same(
                &inspect(&self.depot.dir, &self.slot)?,
                &self.staged.as_ref().map(|x| x.identity.clone())
            ),
            "worktree staging slot changed before intent"
        );
        self.depot.verify(root, lease)?;
        journal.append(Event::BeforePath {
            path: self.path.clone(),
            parent_device: self.parent_identity.0,
            parent_inode: self.parent_identity.1,
            slot: self.slot.clone(),
            original: self.original.as_ref().map(|v| v.identity.clone()),
            original_blob: self.original_blob.clone(),
            staged: self.staged.as_ref().map(|v| v.identity.clone()),
        })?;
        lease.verify()?;
        root.verify_parent(&self.path, &self.parent)?;
        ensure!(
            same(
                &inspect(&self.parent, &self.name)?,
                &self.original.as_ref().map(|x| x.identity.clone())
            ),
            "worktree target changed after intent"
        );
        ensure!(
            same(
                &inspect(&self.depot.dir, &self.slot)?,
                &self.staged.as_ref().map(|x| x.identity.clone())
            ),
            "worktree staging changed after intent"
        );
        hook("before_rename")?;
        self.depot.verify(root, lease)?;
        match (&self.original, &self.staged) {
            (Some(_), Some(_)) => rename(
                &self.depot.dir,
                &self.slot,
                &self.parent,
                &self.name,
                libc::RENAME_EXCHANGE,
            )?,
            (None, Some(_)) => rename(
                &self.depot.dir,
                &self.slot,
                &self.parent,
                &self.name,
                libc::RENAME_NOREPLACE,
            )?,
            (Some(_), None) => rename(
                &self.parent,
                &self.name,
                &self.depot.dir,
                &self.slot,
                libc::RENAME_NOREPLACE,
            )?,
            (None, None) => bail!("empty transition"),
        }
        hook("after_rename")?;
        sync_dir(&self.parent)?;
        sync_dir(&self.depot.dir)?;
        let current = inspect(&self.parent, &self.name)?;
        let displaced = inspect(&self.depot.dir, &self.slot)?;
        ensure!(
            same(&current, &self.staged.as_ref().map(|v| v.identity.clone()))
                && same(
                    &displaced,
                    &self.original.as_ref().map(|v| v.identity.clone())
                ),
            "worktree post-publication identity differs; retain recovery"
        );
        root.verify_parent(&self.path, &self.parent)?;
        self.depot.verify(root, lease)?;
        journal.append(Event::AfterPath {
            path: self.path.clone(),
            published: current.map(|v| v.identity),
        })?;
        Ok(())
    }
}
fn is_directory(identity: &Identity) -> bool {
    identity.mode & libc::S_IFMT == libc::S_IFDIR
}
fn stage(parent: &Dir, slot: &str, content: Content) -> Result<()> {
    match content {
        Content::File { bytes, mode } => {
            ensure!(
                bytes.len() <= MAX_FILE && mode & !0o7777 == 0,
                "invalid staged file"
            );
            let mut f = parent.open_with(
                slot,
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC),
            )?;
            f.write_all(&bytes)?;
            f.set_permissions(Permissions::from_mode(mode))?;
            f.sync_all()?;
        }
        Content::Symlink(target) => {
            ensure!(
                !target.is_empty() && target.len() <= 4096 && !target.contains(&0),
                "invalid symlink target"
            );
            parent.symlink_contents(Path::new(OsStr::from_bytes(&target)), slot)?;
        }
        Content::EmptyDirectory { mode } => {
            ensure!(mode & !0o7777 == 0, "invalid staged directory mode");
            parent.create_dir_with(slot, DirBuilder::new().mode(mode))?;
            let child = parent.open_dir(slot)?;
            child.set_permissions(".", Permissions::from_mode(mode))?;
            sync_dir(&child)?;
        }
    }
    Ok(())
}

/// Resolve a durable intent by observing both the live name and displaced slot.
/// No check-then-rename guarantee is made against non-cooperating writers.
pub(crate) struct RecoveryLeaf<'a> {
    pub path: &'a str,
    pub parent_identity: (u64, u64),
    pub slot: &'a str,
    pub original: Option<Identity>,
    pub staged: Option<Identity>,
}
pub(crate) fn rollback_intent(
    root: &Root,
    depot: &Depot,
    lease: &SyncLease,
    journal: &mut Journal,
    leaf: RecoveryLeaf<'_>,
) -> Result<()> {
    let RecoveryLeaf {
        path,
        parent_identity,
        slot,
        original,
        staged,
    } = leaf;
    ensure!(
        slot.starts_with("checkout-sync-slot-")
            && slot.len() <= 255
            && slot
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b)),
        "invalid recovery slot"
    );
    lease.verify()?;
    let (parent, name) = root.parent(path)?;
    let m = parent.dir_metadata()?;
    ensure!(
        (m.dev(), m.ino()) == parent_identity,
        "worktree recovery parent changed"
    );
    let current = inspect(&parent, &name)?;
    depot.verify(root, lease)?;
    let backup = inspect(&depot.dir, slot)?;
    if same(&current, &original) && same(&backup, &staged) {
        journal.append(Event::BeforeRollback {
            subject: format!("path:{path}"),
        })?;
        journal.append(Event::Rollback {
            subject: format!("path:{path}"),
            restored: true,
            detail: "intent had not changed the live name".into(),
        })?;
        return Ok(());
    }
    ensure!(
        same(&current, &staged) && same(&backup, &original),
        "unexpected live/slot state; preserve both"
    );
    lease.verify()?;
    root.verify_parent(path, &parent)?;
    journal.append(Event::BeforeRollback {
        subject: format!("path:{path}"),
    })?;
    lease.verify()?;
    root.verify_parent(path, &parent)?;
    depot.verify(root, lease)?;
    ensure!(
        same(&inspect(&parent, &name)?, &staged) && same(&inspect(&depot.dir, slot)?, &original),
        "worktree changed after rollback intent; preserve both"
    );
    match (&original, &staged) {
        (Some(_), Some(_)) => rename(&depot.dir, slot, &parent, &name, libc::RENAME_EXCHANGE)?,
        (None, Some(_)) => rename(&parent, &name, &depot.dir, slot, libc::RENAME_NOREPLACE)?,
        (Some(_), None) => rename(&depot.dir, slot, &parent, &name, libc::RENAME_NOREPLACE)?,
        (None, None) => bail!("empty recovery transition"),
    }
    sync_dir(&parent)?;
    sync_dir(&depot.dir)?;
    ensure!(
        same(&inspect(&parent, &name)?, &original) && same(&inspect(&depot.dir, slot)?, &staged),
        "rollback postcondition differs; retain recovery"
    );
    root.verify_parent(path, &parent)?;
    depot.verify(root, lease)?;
    journal.append(Event::Rollback {
        subject: format!("path:{path}"),
        restored: true,
        detail: "conditional worktree restore".into(),
    })?;
    Ok(())
}

#[cfg(test)]
#[path = "checkout_sync_worktree_tests.rs"]
mod tests;
