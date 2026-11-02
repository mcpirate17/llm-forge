//! Directory-capability file access shared by inspection and patch transactions.
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, Metadata, MetadataExt, OpenOptions, OpenOptionsExt};
use conductor_native::syntax::{fingerprint, MAX_SOURCE_BYTES};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub struct SourceRoot {
    directory: Dir,
}

pub struct Snapshot {
    pub(crate) path: PathBuf,
    pub(crate) content: String,
    pub(crate) sha256: String,
    metadata: Metadata,
}

impl Snapshot {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn content(&self) -> &str {
        &self.content
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn into_content(self) -> String {
        self.content
    }
}

impl SourceRoot {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            directory: Dir::open_ambient_dir(path, cap_std::ambient_authority())
                .context("open source directory capability")?,
        })
    }

    pub(crate) fn identity(&self) -> Result<(u64, u64)> {
        let metadata = self.directory.dir_metadata()?;
        ensure!(metadata.is_dir(), "source root is not a directory");
        Ok((metadata.dev(), metadata.ino()))
    }

    pub fn read(&self, path: &Path) -> Result<Snapshot> {
        validate_relative(path)?;
        let mut file = self
            .directory
            .open_with(
                path,
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK),
            )
            .context("open source within selected root")?;
        let metadata = file.metadata()?;
        ensure!(metadata.is_file(), "source must be a regular file");
        ensure!(
            metadata.len() <= MAX_SOURCE_BYTES as u64,
            "source byte limit exceeded"
        );
        let mut content = String::new();
        (&mut file)
            .take(MAX_SOURCE_BYTES as u64 + 1)
            .read_to_string(&mut content)?;
        ensure!(
            content.len() <= MAX_SOURCE_BYTES && !content.contains('\0'),
            "source size/NUL limit exceeded"
        );
        let after = file.metadata()?;
        ensure!(
            metadata.len() == after.len()
                && metadata.mtime() == after.mtime()
                && metadata.mtime_nsec() == after.mtime_nsec()
                && metadata.ctime() == after.ctime()
                && metadata.ctime_nsec() == after.ctime_nsec(),
            "source changed while reading"
        );
        Ok(Snapshot {
            path: path.to_owned(),
            sha256: fingerprint(&content),
            content,
            metadata,
        })
    }

    /// Validation belongs to PatchPlan. Lock cooperating writers, recheck source
    /// bytes/identity, snapshot the exact original in Git, then atomically replace.
    pub(crate) fn replace(&self, original: &Snapshot, candidate: &str) -> Result<String> {
        ensure!(
            candidate.len() <= MAX_SOURCE_BYTES,
            "patched source byte limit exceeded"
        );
        ensure!(
            original.metadata.nlink() == 1,
            "refusing to replace a multiply-linked source file"
        );
        let _lock = DirectoryLock::new(&self.directory)?;
        self.check_current(original)?;
        let blob = self.git_snapshot(&original.content)?;
        let temporary = self.stage(original, candidate)?;
        self.check_current(original)?;
        let name = original
            .path
            .file_name()
            .context("source has no filename")?;
        temporary
            .directory
            .rename(&temporary.name, &temporary.directory, name)?;
        if let Err(error) = temporary.sync() {
            let current = self.read(&original.path)?;
            ensure!(
                current.sha256 == fingerprint(candidate),
                "commit sync failed ({error}); concurrent edit prevents rollback"
            );
            let rollback = self.stage(original, &original.content)?;
            rollback
                .directory
                .rename(&rollback.name, &rollback.directory, name)?;
            rollback
                .sync()
                .context("rollback restored original bytes but directory sync failed")?;
            return Err(error).context("commit durability failed; original source restored");
        }
        Ok(blob)
    }

    fn check_current(&self, original: &Snapshot) -> Result<()> {
        let current = self.read(&original.path)?;
        ensure!(
            current.sha256 == original.sha256
                && current.metadata.ino() == original.metadata.ino()
                && current.metadata.dev() == original.metadata.dev()
                && current.metadata.mode() == original.metadata.mode()
                && current.metadata.nlink() == original.metadata.nlink()
                && current.metadata.ctime() == original.metadata.ctime()
                && current.metadata.ctime_nsec() == original.metadata.ctime_nsec(),
            "source changed since patch preparation"
        );
        Ok(())
    }

    fn stage(&self, original: &Snapshot, candidate: &str) -> Result<StagedFile> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = original
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let directory = self.directory.open_dir(parent)?;
        let name = PathBuf::from(format!(
            ".forge-edit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = directory.open_with(
            &name,
            OpenOptions::new().write(true).create_new(true).mode(0o600),
        )?;
        let staged = StagedFile { directory, name };
        file.write_all(candidate.as_bytes())?;
        file.set_permissions(original.metadata.permissions())?;
        file.sync_all()?;
        Ok(staged)
    }

    fn git_snapshot(&self, source: &str) -> Result<String> {
        // Ordinary repositories only. Metadata is trusted by the root's owner;
        // reject external .git pointers rather than escape the selected root.
        let metadata = self
            .directory
            .open_dir(".git")
            .context("patch application requires a local .git directory")?;
        let path = format!("/proc/self/fd/{}", metadata.as_raw_fd());
        let repo = git2::Repository::open_bare(path).context("open native Git snapshot store")?;
        let blob = repo
            .blob(source.as_bytes())
            .context("write exact original Git blob")?;
        let reference = format!("refs/forge/undo/{blob}");
        let saved = match repo.reference(&reference, blob, false, "forge patch recovery") {
            Ok(reference) => reference,
            Err(error) if error.code() == git2::ErrorCode::Exists => {
                repo.find_reference(&reference)?
            }
            Err(error) => return Err(error).context("retain Git recovery blob"),
        };
        ensure!(
            saved.target() == Some(blob),
            "Git recovery reference has unexpected target"
        );
        Ok(blob.to_string())
    }

    pub(crate) fn recovery_blob(&self, id: &str) -> Result<String> {
        ensure!(
            id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit()),
            "expected complete Git SHA-1 blob id"
        );
        let metadata = self.directory.open_dir(".git")?;
        let repo = git2::Repository::open_bare(format!("/proc/self/fd/{}", metadata.as_raw_fd()))?;
        let oid = git2::Oid::from_str(id)?;
        ensure!(
            repo.find_reference(&format!("refs/forge/undo/{oid}"))?
                .target()
                == Some(oid),
            "blob is not a retained Forge recovery snapshot"
        );
        let blob = repo.find_blob(oid)?;
        ensure!(
            blob.size() <= MAX_SOURCE_BYTES && !blob.content().contains(&0),
            "recovery blob size/NUL limit exceeded"
        );
        Ok(std::str::from_utf8(blob.content())?.to_owned())
    }
}

pub fn validate_relative(path: &Path) -> Result<()> {
    ensure!(
        !path.as_os_str().is_empty()
            && !path.is_absolute()
            && path
                .components()
                .all(|c| matches!(c, Component::Normal(part) if part != ".git")),
        "source path must be relative, without parent traversal or Git metadata"
    );
    Ok(())
}

struct StagedFile {
    directory: Dir,
    name: PathBuf,
}

impl StagedFile {
    fn sync(&self) -> Result<()> {
        self.directory
            .open_with(
                ".",
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY),
            )?
            .sync_all()
            .context("sync source parent directory")
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if let Err(error) = self.directory.remove_file(&self.name) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("forge: temporary patch cleanup failed: {error}");
            }
        }
    }
}

struct DirectoryLock(cap_std::fs::File);

impl DirectoryLock {
    fn new(directory: &Dir) -> Result<Self> {
        // A new open description also serializes threads sharing one SourceRoot.
        let directory = directory.open_with(
            ".",
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY),
        )?;
        // SAFETY: flock borrows a live directory fd, retained by this guard.
        if unsafe { libc::flock(directory.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("another patch holds the root lock");
        }
        Ok(Self(directory))
    }
}

impl Drop for DirectoryLock {
    fn drop(&mut self) {
        // SAFETY: the borrowed Dir outlives this lock guard.
        if unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) } != 0 {
            eprintln!(
                "forge: patch unlock failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}
