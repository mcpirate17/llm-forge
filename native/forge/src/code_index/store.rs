use super::{file_hash, immutable_file, validate_hash};
use crate::harness::state;
use anyhow::{ensure, Context, Result};
use conductor_native::vector_index::{Config, Space};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions, Permissions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub generation: String,
    pub source_root: PathBuf,
    pub rows: usize,
    #[serde(with = "conductor_native::sha256_hex")]
    pub source_set_sha256: [u8; 32],
    pub space: Space,
    pub config: Config,
    pub database_sha256: String,
    pub vectors_sha256: String,
}
impl Manifest {
    pub(crate) fn validate(&self, generation: &str) -> Result<()> {
        ensure!(
            self.version == 1
                && self.generation == generation
                && self.source_root.is_absolute()
                && self.rows <= conductor_native::vector_index::MAX_ROWS
                && self.source_set_sha256 != [0; 32],
            "invalid index manifest"
        );
        self.space.validate()?;
        self.config.validate()?;
        validate_hash(&self.database_sha256)?;
        validate_hash(&self.vectors_sha256)?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct Store {
    pub(crate) root: PathBuf,
}
impl Store {
    /// Indexes live in an explicit private state directory, never in the source tree.
    pub fn create(root: &Path) -> Result<Self> {
        ensure!(root.is_absolute(), "index directory must be absolute");
        match fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        Self::open(root)
    }
    pub fn open(root: &Path) -> Result<Self> {
        Ok(Self {
            root: state::owned_directory(root)?,
        })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn current(&self) -> Result<String> {
        let bytes = state::read(&self.root.join("current"), 128)?;
        let id = std::str::from_utf8(&bytes)?.trim();
        validate_id(id)?;
        Ok(id.to_owned())
    }
    pub(crate) fn begin(&self) -> Result<Staging> {
        let lock = lock(&self.root.join("builder.lock"), true)?;
        let id = crate::native_id::new("index")?;
        validate_id(&id)?;
        let path = self.root.join(&id);
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Staging {
            store: self.clone(),
            path,
            id,
            _builder: lock,
            published: false,
        })
    }
    pub(crate) fn pin(&self, limits: super::ReadLimits) -> Result<Pinned> {
        // Prevent collection between reading the pointer and acquiring the lease.
        let _publication = lock(&self.root.join("publication.lock"), false)?;
        let id = self.current()?;
        let path = self.root.join(&id);
        state::owned_directory(&path)?;
        let lease = lock(&path.join("readers.lock"), false)?;
        drop(_publication);
        let manifest: Manifest =
            serde_json::from_slice(&state::read(&path.join("manifest.json"), 16384)?)?;
        manifest.validate(&id)?;
        ensure!(
            manifest.rows <= limits.max_rows
                && immutable_file(&path.join("vectors.bin"))?.metadata()?.len()
                    <= limits.max_mapped_bytes,
            "index generation exceeds operator reader limits"
        );
        ensure!(
            file_hash(&path.join("chunks.sqlite"))? == manifest.database_sha256,
            "code database checksum mismatch"
        );
        ensure!(
            file_hash(&path.join("vectors.bin"))? == manifest.vectors_sha256,
            "vector file checksum mismatch"
        );
        Ok(Pinned {
            manifest,
            path,
            _lease: lease,
        })
    }
    /// Keep the current generation and one previous generation. Active readers
    /// retain leases; their generations are collected by a later publication.
    fn collect(&self, current: &str, previous: Option<&str>) -> Result<()> {
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(id) = name.to_str() else {
                continue;
            };
            if validate_id(id).is_err() || id == current || Some(id) == previous {
                continue;
            }
            let path = entry.path();
            state::owned_directory(&path)?;
            let Some(_lease) = try_lock(&path.join("readers.lock"), true)? else {
                continue;
            };
            // Only exact generated component names are eligible. Never recursively
            // remove unrelated files that appeared in an index directory.
            for file in fs::read_dir(&path)? {
                let name = file?.file_name();
                ensure!(
                    [
                        "chunks.sqlite",
                        "vectors.bin",
                        "manifest.json",
                        "readers.lock"
                    ]
                    .iter()
                    .any(|allowed| name == *allowed),
                    "unexpected file in retired generation; collection refused"
                );
            }
            for file in fs::read_dir(&path)? {
                fs::remove_file(file?.path())?;
            }
            fs::remove_dir(&path)?;
        }
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
}

pub(crate) struct Pinned {
    pub manifest: Manifest,
    pub path: PathBuf,
    _lease: File,
}
pub(crate) struct Staging {
    store: Store,
    pub path: PathBuf,
    pub id: String,
    _builder: File,
    published: bool,
}
impl Staging {
    pub fn publish(mut self, manifest: &Manifest) -> Result<Manifest> {
        manifest.validate(&self.id)?;
        state::write_new(
            &self.path.join("manifest.json"),
            &serde_json::to_vec_pretty(manifest)?,
        )?;
        seal(&self.path.join("manifest.json"))?;
        drop(lock(&self.path.join("readers.lock"), true)?);
        File::open(&self.path)?.sync_all()?;
        let _publication = lock(&self.store.root.join("publication.lock"), true)?;
        let previous = match self.store.current() {
            Ok(id) => Some(id),
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                None
            }
            Err(e) => return Err(e),
        };
        let pointer = self.store.root.join(crate::native_id::new("pointer")?);
        state::write_new(&pointer, self.id.as_bytes())?;
        fs::rename(&pointer, self.store.root.join("current"))?;
        // Once published, never delete this generation on a durability/GC error.
        self.published = true;
        File::open(&self.store.root)?.sync_all().with_context(|| {
            format!(
                "index {} was published but directory durability is uncertain",
                self.id
            )
        })?;
        self.store
            .collect(&self.id, previous.as_deref())
            .with_context(|| {
                format!(
                    "index {} was published but retired-generation collection failed",
                    self.id
                )
            })?;
        Ok(manifest.clone())
    }
}
impl Drop for Staging {
    fn drop(&mut self) {
        if !self.published {
            if let Err(error) = fs::remove_dir_all(&self.path) {
                eprintln!(
                    "forge: failed to remove unpublished index {}: {error}",
                    self.path.display()
                );
            }
        }
    }
}
fn validate_id(id: &str) -> Result<()> {
    let (pid, nonce) = id
        .strip_prefix("index-")
        .and_then(|rest| rest.split_once('-'))
        .context("invalid index generation id")?;
    ensure!(
        pid.len() <= 10
            && pid.parse::<u32>().is_ok_and(|p| p > 0)
            && nonce.len() == 32
            && nonce
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "invalid index generation id"
    );
    Ok(())
}
fn try_lock(path: &Path, exclusive: bool) -> Result<Option<File>> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.nlink() == 1
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.mode() & 0o077 == 0,
        "invalid index lock inode"
    );
    if unsafe {
        libc::flock(
            file.as_raw_fd(),
            libc::LOCK_NB
                | if exclusive {
                    libc::LOCK_EX
                } else {
                    libc::LOCK_SH
                },
        )
    } != 0
    {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(None);
        }
        return Err(error.into());
    }
    Ok(Some(file))
}
fn lock(path: &Path, exclusive: bool) -> Result<File> {
    try_lock(path, exclusive)?.context("index publication, builder, or generation is busy; retry")
}

pub(crate) fn seal(path: &Path) -> Result<()> {
    fs::set_permissions(path, Permissions::from_mode(0o400))?;
    immutable_file(path)?.sync_all()?;
    Ok(())
}
