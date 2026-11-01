//! Immutable JSONL and mmap generations, selected by one atomic pointer.
use crate::{
    file_lock::{self, Lease},
    harness::state,
};
use anyhow::{ensure, Context, Result};
use conductor_native::memory_index_sidecar::Reader;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub generation: String,
    pub rows: usize,
    pub dimension: usize,
    pub fingerprint: String,
    pub source_sha256: String,
    pub sources: Vec<String>,
}

impl Manifest {
    fn validate(
        &self,
        id: &str,
        size: usize,
        metadata: &conductor_native::memory_index_sidecar::Metadata,
    ) -> Result<()> {
        ensure!(
            size <= 65536
                && self.schema_version == 1
                && self.generation == id
                && !self.sources.is_empty()
                && self.sources.len() <= 256,
            "invalid memory manifest"
        );
        let mut sources = std::collections::BTreeSet::new();
        for source in &self.sources {
            crate::task_store::validate_id(source)?;
            ensure!(sources.insert(source), "duplicate memory manifest source");
        }
        ensure!(
            self.rows == metadata.rows
                && self.dimension == metadata.dimension
                && self.fingerprint == metadata.fingerprint
                && conductor_native::sha256_hex::parse(&self.source_sha256)
                    .map_err(anyhow::Error::msg)?
                    == metadata.source_sha256,
            "memory manifest does not bind the complete snapshot"
        );
        Ok(())
    }
}

#[derive(Clone)]
pub struct Store {
    root: PathBuf,
}

pub struct Pinned {
    pub manifest: Manifest,
    pub source: PathBuf,
    pub reader: Reader,
    _lease: Lease,
}

pub(crate) struct Building {
    store: Store,
    pub path: PathBuf,
    pub id: String,
    _writer: Lease,
    published: bool,
}

fn valid_id(id: &str) -> Result<()> {
    let (pid, nonce) = id
        .strip_prefix("memory-")
        .and_then(|s| s.split_once('-'))
        .context("invalid memory generation")?;
    ensure!(
        pid.len() <= 10
            && pid.parse::<u32>().is_ok_and(|p| p > 0)
            && nonce.len() == 32
            && nonce
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "invalid memory generation"
    );
    Ok(())
}

fn immutable(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file()
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.nlink() == 1
            && meta.mode() & 0o222 == 0,
        "memory generation component must be owned, immutable and singly linked"
    );
    Ok(())
}

impl Store {
    pub fn create(root: &Path) -> Result<Self> {
        ensure!(root.is_absolute(), "memory store must be absolute");
        match fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => File::open(root.parent().context("memory root parent")?)?.sync_all()?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
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
        let data = state::read(&self.root.join("current"), 128)?;
        let id = std::str::from_utf8(&data)?.trim();
        valid_id(id)?;
        Ok(id.to_owned())
    }
    pub fn pin(&self) -> Result<Pinned> {
        let publication = file_lock::owned(&self.root.join("publication.lock"), false)?;
        let id = self.current()?;
        let directory = self.root.join(&id);
        state::owned_directory(&directory)?;
        let lease = file_lock::owned(&directory.join("readers.lock"), false)?;
        drop(publication);
        for name in ["manifest.json", "index.jsonl", "index.sidecar"] {
            immutable(&directory.join(name))?;
        }
        let manifest_bytes = state::read(&directory.join("manifest.json"), 65536)?;
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
        let source = directory.join("index.jsonl");
        let reader = Reader::open(&source, &directory.join("index.sidecar"))?;
        reader.verify_source_hash()?;
        let metadata = reader.metadata();
        manifest.validate(&id, manifest_bytes.len(), &metadata)?;
        Ok(Pinned {
            manifest,
            source,
            reader,
            _lease: lease,
        })
    }
    pub(crate) fn begin(&self) -> Result<Building> {
        let writer = file_lock::owned(&self.root.join("builder.lock"), true)?;
        let id = crate::resource_id("memory")?;
        valid_id(&id)?;
        let path = self.root.join(&id);
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Building {
            store: self.clone(),
            path,
            id,
            _writer: writer,
            published: false,
        })
    }
    fn collect(&self, current: &str, previous: Option<&str>) -> Result<()> {
        let entries = fs::read_dir(&self.root)?
            .take(4097)
            .collect::<std::io::Result<Vec<_>>>()?;
        ensure!(
            entries.len() <= 4096,
            "memory store generation inventory exceeds 4096"
        );
        let mut candidates = Vec::new();
        for entry in entries {
            let name = entry.file_name();
            let Some(id) = name.to_str() else {
                continue;
            };
            if valid_id(id).is_err() || id == current || Some(id) == previous {
                continue;
            }
            let path = entry.path();
            state::owned_directory(&path)?;
            let Some(_reader) = file_lock::try_owned(&path.join("readers.lock"), true)? else {
                continue;
            };
            collectible_files(&path)?;
            candidates.push(path);
        }
        for path in candidates {
            let Some(_reader) = file_lock::try_owned(&path.join("readers.lock"), true)? else {
                continue;
            };
            for file in collectible_files(&path)? {
                fs::remove_file(file.path())?;
            }
            fs::remove_dir(path)?;
        }
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
}

fn collectible_files(path: &Path) -> Result<Vec<fs::DirEntry>> {
    let files = fs::read_dir(path)?
        .take(6)
        .collect::<std::io::Result<Vec<_>>>()?;
    ensure!(
        files.len() <= 5
            && files.iter().all(|entry| [
                "manifest.json",
                "index.jsonl",
                "index.sidecar",
                "index.sidecar.lock",
                "readers.lock"
            ]
            .iter()
            .any(|allowed| entry.file_name() == *allowed)),
        "unexpected memory generation component; collection refused"
    );
    Ok(files)
}

impl Building {
    pub fn publish(mut self, sources: Vec<String>) -> Result<Manifest> {
        let source = self.path.join("index.jsonl");
        // Seal before the sidecar captures ctime, and never rename the source inode.
        crate::state_publish::seal_file(&source)?;
        let sidecar = self.path.join("index.sidecar");
        conductor_native::memory_index_sidecar::build_sidecar(&source, &sidecar)
            .map_err(anyhow::Error::msg)?;
        let reader = Reader::open(&source, &sidecar)?;
        reader.verify_source_hash()?;
        let metadata = reader.metadata();
        let manifest = Manifest {
            schema_version: 1,
            generation: self.id.clone(),
            rows: metadata.rows,
            dimension: metadata.dimension,
            fingerprint: metadata.fingerprint.clone(),
            source_sha256: metadata
                .source_sha256
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            sources,
        };
        let manifest_bytes = serde_json::to_vec(&manifest)?;
        manifest.validate(&self.id, manifest_bytes.len(), &metadata)?;
        state::write_new(&self.path.join("manifest.json"), &manifest_bytes)?;
        crate::state_publish::seal_file(&self.path.join("manifest.json"))?;
        drop(file_lock::owned(&self.path.join("readers.lock"), true)?);
        File::open(&self.path)?.sync_all()?;
        let _publication = file_lock::owned(&self.store.root.join("publication.lock"), true)?;
        let previous = match self.store.current() {
            Ok(id) => Some(id),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        // Record visibility even if the following directory fsync reports an error.
        let pointer = self.store.root.join(crate::resource_id("memory-pointer")?);
        state::write_new(&pointer, self.id.as_bytes())?;
        if let Err(error) = fs::rename(&pointer, self.store.root.join("current")) {
            let _ = fs::remove_file(pointer);
            return Err(error.into());
        }
        self.published = true;
        File::open(&self.store.root)?
            .sync_all()
            .context("memory generation published but directory sync failed")?;
        self.store
            .collect(&self.id, previous.as_deref())
            .context("memory generation published but collection failed")?;
        Ok(manifest)
    }
}
impl Drop for Building {
    fn drop(&mut self) {
        if !self.published {
            if let Err(error) = fs::remove_dir_all(&self.path) {
                eprintln!("forge: unpublished memory cleanup: {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(crate::resource_id("memory-store-test").unwrap());
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn build(store: &Store, sources: Vec<String>) -> Result<Manifest> {
        let stage = store.begin()?;
        let row = serde_json::json!({"schema_version":3,
            "embedding":{"fingerprint":"sha256:test","dimension":2,"paid":true},
            "source_sha256":"0".repeat(64),"source":"notes","path":"note.md",
            "title":"test","text":"body","vector":[1.0,0.0]});
        fs::write(stage.path.join("index.jsonl"), serde_json::to_vec(&row)?)?;
        stage.publish(sources)
    }
    #[test]
    fn invalid_manifest_never_replaces_a_valid_current_generation() {
        let scratch = Scratch::new();
        let store = Store::create(&scratch.0.join("store")).unwrap();
        let original = build(&store, vec!["notes".into()]).unwrap().generation;
        for sources in [
            vec!["x".into(); 257],
            vec!["x".repeat(65537)],
            vec![],
            vec!["".into()],
            vec!["../notes".into()],
            vec!["notes".into(), "notes".into()],
        ] {
            assert!(build(&store, sources).is_err());
            assert_eq!(store.current().unwrap(), original);
            assert_eq!(store.pin().unwrap().manifest.generation, original);
        }
    }
    #[test]
    fn oversized_generation_inventory_does_not_remove_any_entries() {
        let scratch = Scratch::new();
        let store = Store::create(&scratch.0.join("store")).unwrap();
        let id = crate::resource_id("memory").unwrap();
        let old = store.root.join(id);
        fs::DirBuilder::new().mode(0o700).create(&old).unwrap();
        let valid = store.root.join(crate::resource_id("memory").unwrap());
        fs::DirBuilder::new().mode(0o700).create(&valid).unwrap();
        fs::write(valid.join("manifest.json"), b"preserve").unwrap();
        for i in 0..6 {
            fs::write(old.join(format!("sentinel-{i}")), b"preserve").unwrap();
        }
        assert!(store.collect("current", None).is_err());
        assert_eq!(fs::read(valid.join("manifest.json")).unwrap(), b"preserve");
        for i in 0..6 {
            assert_eq!(
                fs::read(old.join(format!("sentinel-{i}"))).unwrap(),
                b"preserve"
            );
        }
    }
}
