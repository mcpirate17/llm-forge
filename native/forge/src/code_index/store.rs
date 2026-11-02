use super::{file_hash, immutable_file, validate_hash};
use crate::file_lock::{owned as lock, try_owned as try_lock, Lease};
use crate::harness::state;
use anyhow::{ensure, Context, Result};
use conductor_native::vector_index::{Config, Space};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Native-refresh lineage, local evidence rather than authentication against the owner.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshOrigin {
    pub version: u32,
    pub generation: String,
    pub source_root: PathBuf,
    pub space: Space,
    #[serde(with = "conductor_native::sha256_hex")]
    pub source_set_sha256: [u8; 32],
}
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_origin: Option<RefreshOrigin>,
}
impl Manifest {
    pub(crate) fn validate(&self, generation: &str) -> Result<()> {
        ensure!(
            ((self.version == 1 && self.refresh_origin.is_none())
                || (self.version == 2 && self.refresh_origin.is_some()))
                && self.generation == generation
                && self.source_root.is_absolute()
                && self.rows <= conductor_native::vector_index::MAX_ROWS
                && self.source_set_sha256 != [0; 32],
            "invalid index manifest"
        );
        self.space.validate()?;
        self.config.validate()?;
        if let Some(origin) = &self.refresh_origin {
            ensure!(
                origin.version == 1
                    && origin.generation == self.generation
                    && origin.source_root == self.source_root
                    && origin.space == self.space
                    && origin.source_set_sha256 == self.source_set_sha256,
                "native refresh origin binding mismatch"
            );
        }
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
            Ok(()) => File::open(root.parent().context("index root parent")?)?.sync_all()?,
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
    _lease: Lease,
}
pub(crate) struct Staging {
    store: Store,
    pub path: PathBuf,
    pub id: String,
    _builder: Lease,
    published: bool,
}
struct PendingPointer {
    path: PathBuf,
    armed: bool,
}
impl Drop for PendingPointer {
    fn drop(&mut self) {
        if self.armed {
            if let Err(error) = fs::remove_file(&self.path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "forge: failed to remove unpublished pointer {}: {error}",
                        self.path.display()
                    );
                }
            }
        }
    }
}
fn write_pointer(root: &Path, id: &str) -> Result<PendingPointer> {
    let path = root.join(crate::native_id::new("pointer")?);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    let pointer = PendingPointer { path, armed: true };
    file.write_all(id.as_bytes())?;
    file.sync_all()?;
    File::open(root)?.sync_all()?;
    Ok(pointer)
}
struct PublicationChecks<'a> {
    checkpoint: &'a mut dyn FnMut() -> Result<()>,
    final_check: &'a mut dyn FnMut() -> Result<()>,
}
impl Staging {
    pub fn publish(mut self, manifest: &Manifest) -> Result<Manifest> {
        self.publish_inner(manifest, None)
    }
    pub fn publish_checked(
        mut self,
        manifest: &Manifest,
        checkpoint: &mut dyn FnMut() -> Result<()>,
        final_check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Manifest> {
        self.publish_inner(
            manifest,
            Some(PublicationChecks {
                checkpoint,
                final_check,
            }),
        )
    }
    fn publish_inner(
        &mut self,
        manifest: &Manifest,
        checks: Option<PublicationChecks<'_>>,
    ) -> Result<Manifest> {
        manifest.validate(&self.id)?;
        state::write_new(
            &self.path.join("manifest.json"),
            &serde_json::to_vec_pretty(manifest)?,
        )?;
        seal(&self.path.join("manifest.json"))?;
        drop(lock(&self.path.join("readers.lock"), true)?);
        File::open(&self.path)?.sync_all()?;
        // This existing private-state lock is deliberately nonblocking.
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
        let mut pointer = write_pointer(&self.store.root, &self.id)?;
        if let Some(checks) = checks {
            (checks.checkpoint)()?;
            (checks.final_check)()?;
            (checks.checkpoint)()?;
        }
        fs::rename(&pointer.path, self.store.root.join("current"))?;
        pointer.armed = false;
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
pub(crate) fn seal(path: &Path) -> Result<()> {
    crate::state_publish::seal_file(path)?;
    Ok(())
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod publication_tests;
