//! Held, initially empty read-only bytecode namespace for confined Fest guests.
//!
//! The directory is a sibling of the writable snapshot, never a child of it.
//! It is not removed on drop: the owner removes it only after proven cleanup.
use crate::{candidate_inputs::DirectoryBinding, mutation_execution_grants::Grants};
use anyhow::{ensure, Context, Result};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

pub(crate) struct ReadOnlyCache {
    path: PathBuf,
    held: DirectoryBinding,
}

impl ReadOnlyCache {
    pub(crate) fn create(grants: &Grants) -> Result<Self> {
        grants.verify()?;
        let path = grants
            .scratch_parent
            .join(crate::resource_id("fest-pycache")?);
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .context("create private Fest bytecode namespace")?;
        let admission: Result<Self> = (|| {
            grants.validate_private_root(&path)?;
            let held = DirectoryBinding::open(&path, None)?;
            let result = Self {
                path: path.clone(),
                held,
            };
            result.verify()?;
            Ok(result)
        })();
        if let Err(error) = admission {
            return match fs::remove_dir(&path) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(error.context(format!(
                    "prelaunch cache cleanup failed; retained {}: {cleanup}",
                    path.display()
                ))),
            };
        }
        admission
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn verify(&self) -> Result<()> {
        self.held.verify()?;
        ensure!(
            fs::read_dir(&self.path)?.next().is_none(),
            "Fest bytecode namespace is not empty"
        );
        self.held.verify()
    }

    pub(crate) fn verify_admission(&self) -> Result<()> {
        self.verify().with_context(|| {
            format!(
                "Fest cache admission failed; retained {}",
                self.path.display()
            )
        })
    }

    /// Caller must first prove all owned descendants and input checks complete.
    pub(crate) fn remove_verified(self) -> Result<()> {
        self.verify()?;
        fs::remove_dir(&self.path).context("remove empty Fest bytecode namespace")?;
        ensure!(
            matches!(fs::symlink_metadata(&self.path), Err(error)
            if error.kind() == std::io::ErrorKind::NotFound),
            "Fest bytecode namespace remains"
        );
        Ok(())
    }

    pub(crate) fn prelaunch_error(self, error: anyhow::Error) -> anyhow::Error {
        let path = self.path.clone();
        match self.remove_verified() {
            Ok(()) => error,
            Err(cleanup) => error.context(format!(
                "Fest prelaunch cache cleanup failed; retained {}: {cleanup:#}",
                path.display()
            )),
        }
    }
}

use crate::mutation_cargo_seed::{
    inventory_root, inventory_tree, verify_inventory_root, InventoryBudget, InventoryEntry,
};
use serde::Deserialize;

/// Operator-selected external manifest is authority; the guest never selects it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GuestConfig {
    pub(crate) root: PathBuf,
    pub(crate) manifest_path: PathBuf,
    pub(crate) manifest_sha256: String,
    pub(crate) external_files: std::collections::BTreeMap<PathBuf, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuestManifest {
    schema_version: u32,
    entries: Vec<InventoryEntry>,
}

pub(crate) struct HeldGuest {
    root: PathBuf,
    directory: cap_std::fs::Dir,
    manifest: crate::candidate_inputs::BoundFile,
    external_files: Vec<crate::candidate_inputs::BoundFile>,
    entries: Vec<InventoryEntry>,
}

fn validate_entries(entries: &[InventoryEntry]) -> Result<()> {
    ensure!(
        !entries.is_empty() && entries.len() <= 10_000,
        "invalid Fest guest entry count"
    );
    let mut total = 0u64;
    let mut previous: Option<&str> = None;
    for entry in entries {
        ensure!(
            previous.is_none_or(|p| p < entry.path.as_str()),
            "Fest guest entries must be uniquely sorted"
        );
        previous = Some(&entry.path);
        let name = Path::new(&entry.path)
            .file_name()
            .and_then(|n| n.to_str())
            .context("invalid Fest guest path")?;
        ensure!(
            !name.ends_with(".pth")
                && !name.ends_with(".pyc")
                && !name.ends_with(".pyo")
                && !matches!(
                    name,
                    "sitecustomize.py"
                        | "usercustomize.py"
                        | "pyvenv.cfg"
                        | "__pycache__"
                        | "seed-manifest.json"
                ),
            "Fest guest contains a startup hook or bytecode cache: {}",
            entry.path
        );
        ensure!(
            entry.bytes <= 32 * 1024 * 1024,
            "Fest guest file exceeds bound"
        );
        total = total
            .checked_add(entry.bytes)
            .context("Fest guest byte count overflow")?;
        ensure!(
            total <= 256 * 1024 * 1024,
            "Fest guest exceeds total byte bound"
        );
    }
    Ok(())
}

fn capture_external_files(
    config: &GuestConfig,
    grants: &Grants,
) -> Result<Vec<crate::candidate_inputs::BoundFile>> {
    ensure!(
        config.external_files.len() <= 128,
        "too many Fest external files"
    );
    let granted: std::collections::BTreeSet<_> = grants
        .read_only
        .iter()
        .filter(|path| *path != &config.root)
        .collect();
    let pinned: std::collections::BTreeSet<_> = config.external_files.keys().collect();
    ensure!(
        granted == pinned,
        "Fest external file pins must match read grants exactly"
    );
    let mut total = 0usize;
    let mut held = Vec::with_capacity(pinned.len());
    for (path, hash) in &config.external_files {
        let (file, bytes) = crate::candidate_inputs::external_with_tmp(
            path,
            hash,
            32 * 1024 * 1024,
            &grants.source_root,
            Some(&grants.scratch_parent),
        )?;
        total = total
            .checked_add(bytes.len())
            .context("Fest external file size overflow")?;
        ensure!(
            total <= 128 * 1024 * 1024,
            "Fest external files exceed total byte bound"
        );
        held.push(file);
    }
    Ok(held)
}

impl HeldGuest {
    pub(crate) fn capture(config: &GuestConfig, grants: &Grants) -> Result<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let root_metadata = fs::symlink_metadata(&config.root)?;
        ensure!(
            root_metadata.is_dir()
                && !root_metadata.file_type().is_symlink()
                && root_metadata.uid() == unsafe { libc::geteuid() }
                && root_metadata.mode() & 0o7777 == 0o700,
            "Fest guest root must be an owned private directory"
        );
        ensure!(
            grants.read_only.iter().any(|path| path == &config.root),
            "Fest guest root requires its exact independent read grant"
        );
        let external_files = capture_external_files(config, grants)?;
        ensure!(
            grants.program("python")? == config.root.join("bin/python")
                && grants.program("fest")? == config.root.join("bin/fest"),
            "Fest guest executables must be pinned aliases inside the guest prefix"
        );
        ensure!(
            !config.manifest_path.starts_with(&config.root)
                && !config.root.starts_with(&config.manifest_path),
            "Fest manifest aliases guest root"
        );
        let (manifest, bytes) = crate::candidate_inputs::external_with_tmp(
            &config.manifest_path,
            &config.manifest_sha256,
            8 * 1024 * 1024,
            &grants.source_root,
            Some(&grants.scratch_parent),
        )?;
        let text = std::str::from_utf8(&bytes)?;
        let value = crate::tool_schema::parse_document_with_limit(text, 8 * 1024 * 1024)?;
        let document: GuestManifest = serde_json::from_value(value)?;
        ensure!(
            document.schema_version == 1,
            "unsupported Fest guest manifest"
        );
        validate_entries(&document.entries)?;
        let directory = inventory_root(&config.root)?;
        let result = Self {
            root: config.root.clone(),
            directory,
            manifest,
            external_files,
            entries: document.entries,
        };
        result.verify()?;
        Ok(result)
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn verify(&self) -> Result<()> {
        self.manifest.verify()?;
        for file in &self.external_files {
            file.verify()?;
        }
        verify_inventory_root(&self.root, &self.directory)?;
        // The shared walker reserves this name for Cargo's own manifest; it must
        // never be silently skipped in the Python guest inventory.
        ensure!(
            matches!(fs::symlink_metadata(self.root.join("seed-manifest.json")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound),
            "Fest guest contains reserved seed manifest path"
        );
        let actual = inventory_tree(&self.directory, None, "", &mut InventoryBudget::guest())?;
        validate_entries(&actual)?;
        ensure!(actual == self.entries, "Fest guest inventory changed");
        verify_inventory_root(&self.root, &self.directory)?;
        for file in &self.external_files {
            file.verify()?;
        }
        self.manifest.verify()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn empty_cache_detects_new_entries_and_requires_empty_removal() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("cache");
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let cache = ReadOnlyCache {
            held: DirectoryBinding::open(&path, None).unwrap(),
            path: path.clone(),
        };
        assert!(cache.verify().is_ok());
        fs::write(path.join("unexpected.pyc"), b"not empty").unwrap();
        assert!(cache.verify().is_err());
        assert!(
            format!("{:#}", cache.verify_admission().unwrap_err()).contains(path.to_str().unwrap())
        );
        fs::remove_file(path.join("unexpected.pyc")).unwrap();
        cache.remove_verified().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn guest_budget_rejects_oversized_file_before_inventory() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let file = fs::File::create(root.path().join("oversized")).unwrap();
        file.set_len(32 * 1024 * 1024 + 1).unwrap();
        let directory = inventory_root(root.path()).unwrap();
        assert!(inventory_tree(&directory, None, "", &mut InventoryBudget::guest()).is_err());
    }

    #[test]
    fn prelaunch_failure_reports_the_retained_cache_path() {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("cache");
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let cache = ReadOnlyCache {
            held: DirectoryBinding::open(&path, None).unwrap(),
            path: path.clone(),
        };
        fs::write(path.join("unexpected"), b"retained").unwrap();
        let error = cache.prelaunch_error(anyhow::anyhow!("setup failed"));
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("setup failed"));
        assert!(diagnostic.contains(path.to_str().unwrap()));
        assert!(path.join("unexpected").is_file());
    }
}

#[cfg(test)]
#[path = "mutation_fest_guest_tests.rs"]
mod admission_tests;
