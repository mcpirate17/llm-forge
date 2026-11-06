//! Explicit offline crates.io seed provisioning. No ambient Cargo home or network.
//! Cache index bytes are opaque, pinned to an operator-selected Cargo executable.
//! Revalidation observes persistent drift, not hostile same-UID transient changes.
#[path = "mutation_cargo_seed_archive.rs"]
mod archive;
#[path = "mutation_cargo_seed_io.rs"]
mod io;
#[cfg(test)]
#[path = "mutation_cargo_seed_tests.rs"]
mod tests;
use crate::candidate_inputs::{self, BoundFile, DirectoryBinding};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";
const INDEX_PROVENANCE: &str = "opaque-public-cache-observation-not-authenticated-by-cargo-lock";
const MANIFEST: &str = "seed-manifest.json";
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SeedConfig {
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub expected_sha256: String,
}
pub struct ProvisionRequest<'a> {
    pub cache_root: &'a Path,
    pub lock_path: &'a Path,
    pub expected_lock_sha256: &'a str,
    /// Opaque cache namespace selected explicitly; never inferred from receipts.
    pub registry_directory: &'a str,
    pub cargo_sha256: &'a str,
    pub destination: &'a Path,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SeedIdentity {
    pub schema_version: u32,
    pub manifest_sha256: String,
    pub lock_sha256: String,
    pub cargo_sha256: String,
    pub registry_directory: String,
    pub files: usize,
    pub bytes: u64,
    pub source_inventory_sha256: String,
    pub expanded_files: usize,
    pub expanded_bytes: u64,
    pub index_provenance: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct Provisioned {
    pub config: SeedConfig,
    pub identity: SeedIdentity,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Package {
    name: String,
    version: String,
    checksum: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    registry_source: String,
    index_provenance: String,
    registry_directory: String,
    cargo_sha256: String,
    lock_sha256: String,
    packages: Vec<Package>,
    entries: Vec<io::Entry>,
    expanded: Vec<archive::Expanded>,
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn hash(value: &str) -> Result<()> {
    ensure!(
        value.len() == 64
            && value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "seed requires lowercase SHA-256 hex"
    );
    Ok(())
}
fn namespace(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 128
            && value != "."
            && value != ".."
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid explicit registry namespace"
    );
    Ok(())
}
fn packages(bytes: &[u8]) -> Result<Vec<Package>> {
    ensure!(bytes.len() <= 1024 * 1024, "seed lock exceeds 1MiB");
    let lock: toml::Value = std::str::from_utf8(bytes)?.parse()?;
    ensure!(
        matches!(
            lock.get("version").and_then(toml::Value::as_integer),
            Some(3 | 4)
        ),
        "unsupported seed lock version"
    );
    let entries = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .context("seed lock packages missing")?;
    ensure!(entries.len() <= 4096, "seed lock exceeds 4096 packages");
    let mut result = BTreeMap::new();
    for entry in entries {
        let Some(source) = entry.get("source") else {
            continue;
        };
        ensure!(
            source.as_str() == Some(REGISTRY),
            "seed refuses Git and non-crates.io registry sources"
        );
        let name = entry
            .get("name")
            .and_then(toml::Value::as_str)
            .context("locked package name missing")?;
        let version = entry
            .get("version")
            .and_then(toml::Value::as_str)
            .context("locked package version missing")?;
        ensure!(
            !name.is_empty()
                && name.len() <= 128
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
            "invalid locked crate name"
        );
        ensure!(
            !version.is_empty()
                && version.len() <= 128
                && version
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b)),
            "invalid locked crate version"
        );
        let checksum = entry
            .get("checksum")
            .and_then(toml::Value::as_str)
            .context("locked registry checksum missing")?;
        hash(checksum)?;
        ensure!(
            result
                .insert(
                    (name.to_string(), version.to_string()),
                    Package {
                        name: name.into(),
                        version: version.into(),
                        checksum: checksum.into()
                    }
                )
                .is_none(),
            "duplicate locked registry package"
        );
    }
    ensure!(!result.is_empty(), "seed lock has no registry packages");
    let packages: Vec<_> = result.into_values().collect();
    archive_paths(&packages, "validation")?;
    Ok(packages)
}
fn archive_paths<'a>(
    packages: &'a [Package],
    namespace: &str,
) -> Result<BTreeMap<String, &'a str>> {
    let mut paths = BTreeMap::new();
    for package in packages {
        let path = format!(
            "{}/{}-{}.crate",
            prefix(namespace, "cache"),
            package.name,
            package.version
        );
        ensure!(
            paths.insert(path, package.checksum.as_str()).is_none(),
            "locked package archive paths collide"
        );
    }
    Ok(paths)
}
// Public Cargo registry index pathname layout; cached entry bytes stay opaque.
// Names reach this helper only after package validation against held lock bytes.
fn index_path(name: &str) -> String {
    let name = name.to_ascii_lowercase();
    match name.len() {
        1 | 2 => format!("{}/{name}", name.len()),
        3 => format!("3/{}/{name}", &name[..1]),
        _ => format!("{}/{}/{name}", &name[..2], &name[2..4]),
    }
}
fn index_paths(packages: &[Package], namespace: &str) -> BTreeSet<String> {
    let root = prefix(namespace, "index");
    let mut paths = BTreeSet::from([format!("{root}/config.json")]);
    paths.extend(
        packages
            .iter()
            .map(|p| format!("{root}/.cache/{}", index_path(&p.name))),
    );
    paths
}
fn input_directories(index_files: &BTreeSet<String>, namespace: &str) -> BTreeSet<String> {
    let mut directories = BTreeSet::from([
        "registry".into(),
        "registry/cache".into(),
        prefix(namespace, "cache"),
    ]);
    for file in index_files {
        directories.extend(
            Path::new(file)
                .ancestors()
                .skip(1)
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_str().unwrap().into()),
        );
    }
    directories
}
fn prefix(namespace: &str, group: &str) -> String {
    format!("registry/{group}/{namespace}")
}
fn validate_manifest(manifest: &Manifest, lock: &[u8], cargo: &str) -> Result<()> {
    hash(cargo)?;
    namespace(&manifest.registry_directory)?;
    ensure!(
        manifest.schema_version == 1
            && manifest.registry_source == REGISTRY
            && manifest.index_provenance == INDEX_PROVENANCE
            && manifest.lock_sha256 == sha(lock)
            && manifest.cargo_sha256 == cargo
            && manifest.packages == packages(lock)?,
        "seed manifest lock/Cargo/package identity mismatch"
    );
    ensure!(
        manifest
            .entries
            .len()
            .checked_add(
                manifest
                    .expanded
                    .iter()
                    .map(|p| p.entries.len())
                    .sum::<usize>()
            )
            .is_some_and(|n| n <= io::MAX_ENTRIES),
        "seed manifest entry limit exceeded"
    );
    let archives = archive_paths(&manifest.packages, &manifest.registry_directory)?;
    let index_files = index_paths(&manifest.packages, &manifest.registry_directory);
    let directories = input_directories(&index_files, &manifest.registry_directory);
    let mut seen = BTreeMap::new();
    let mut actual = BTreeSet::new();
    let mut total = 0u64;
    let mut previous = "";
    let index = prefix(&manifest.registry_directory, "index");
    let cache = prefix(&manifest.registry_directory, "cache");
    for entry in &manifest.entries {
        io::relative(&entry.path)?;
        ensure!(
            !entry.executable,
            "cache/index input must not be executable"
        );
        ensure!(
            previous < entry.path.as_str(),
            "seed manifest entries must be unique and sorted"
        );
        previous = &entry.path;
        ensure!(entry.bytes <= io::MAX_FILE, "seed file bound exceeded");
        total = total
            .checked_add(entry.bytes)
            .context("seed total overflow")?;
        ensure!(total <= io::MAX_TOTAL, "seed total bound exceeded");
        if validate_entry(entry, &archives, &index_files, &directories)? {
            actual.insert(entry.path.clone());
        }
        seen.insert(entry.path.as_str(), entry.directory);
    }
    ensure!(
        actual.len() == archives.len(),
        "seed missing locked archive"
    );
    for required in [
        "registry",
        "registry/cache",
        "registry/index",
        &cache,
        &index,
        &format!("{index}/.cache"),
        &format!("{index}/config.json"),
    ] {
        ensure!(
            seen.contains_key(required),
            "required seed structure missing: {required}"
        );
    }
    for package in &manifest.packages {
        let path = format!("{index}/.cache/{}", index_path(&package.name));
        ensure!(
            seen.get(path.as_str()) == Some(&false),
            "locked package sparse index entry missing: {}",
            package.name
        );
    }
    Ok(())
}
fn validate_entry(
    entry: &io::Entry,
    archives: &BTreeMap<String, &str>,
    index_files: &BTreeSet<String>,
    directories: &BTreeSet<String>,
) -> Result<bool> {
    if entry.directory {
        ensure!(
            entry.bytes == 0 && entry.sha256.is_none(),
            "invalid seed directory entry"
        );
        ensure!(
            directories.contains(&entry.path),
            "unexpected seed directory"
        );
    } else {
        hash(
            entry
                .sha256
                .as_deref()
                .context("seed file digest missing")?,
        )?;
        if let Some(checksum) = archives.get(&entry.path) {
            ensure!(
                entry.sha256.as_deref() == Some(*checksum),
                "seed archive differs from lock checksum"
            );
            return Ok(true);
        }
        ensure!(index_files.contains(&entry.path), "unexpected seed file");
    }
    Ok(false)
}

fn identity(manifest: &Manifest, digest: String) -> Result<SeedIdentity> {
    Ok(SeedIdentity {
        schema_version: 1,
        manifest_sha256: digest,
        lock_sha256: manifest.lock_sha256.clone(),
        cargo_sha256: manifest.cargo_sha256.clone(),
        registry_directory: manifest.registry_directory.clone(),
        files: manifest.entries.iter().filter(|e| !e.directory).count(),
        bytes: manifest.entries.iter().map(|e| e.bytes).sum(),
        index_provenance: INDEX_PROVENANCE.into(),
        source_inventory_sha256: sha(&serde_json::to_vec(&manifest.expanded)?),
        expanded_files: manifest
            .expanded
            .iter()
            .flat_map(|p| &p.entries)
            .filter(|e| !e.directory)
            .count(),
        expanded_bytes: manifest
            .expanded
            .iter()
            .flat_map(|p| &p.entries)
            .map(|e| e.bytes)
            .sum(),
    })
}
/// Imports only world-readable public source files; group-write is untrusted,
/// never permission to execute or read credentials/configuration.
/// Copies only selected .crate archives and opaque sparse-index metadata. Registry
/// config.json is metadata; Cargo config.toml and credentials are never copied.
/// On failure any newly created destination is retained for explicit inspection.
pub fn provision(request: &ProvisionRequest<'_>) -> Result<Provisioned> {
    namespace(request.registry_directory)?;
    hash(request.cargo_sha256)?;
    hash(request.expected_lock_sha256)?;
    ensure!(
        request.lock_path.is_absolute() && request.lock_path.canonicalize()? == request.lock_path,
        "seed lock path must be canonical"
    );
    let (lock, bytes) = candidate_inputs::repository_live(request.lock_path, 1024 * 1024)?;
    ensure!(
        sha(&bytes) == request.expected_lock_sha256,
        "seed lock digest mismatch"
    );
    let packages = packages(&bytes)?;
    let source = io::PublicCache::open(request.cache_root)?;
    let target = io::new_root(
        request.destination,
        &[request.cache_root, request.lock_path],
    )?;
    let operation = || -> Result<Provisioned> {
        let mut budget = io::Budget::new();
        for package in &packages {
            let path = format!(
                "{}/{}-{}.crate",
                prefix(request.registry_directory, "cache"),
                package.name,
                package.version
            );
            let copied = source
                .copy_file(&path, &target, &mut budget)
                .with_context(|| format!("locked archive input missing/unsafe: {path}"))?;
            ensure!(
                copied.sha256.as_deref() == Some(package.checksum.as_str()),
                "registry archive checksum mismatch: {path}"
            );
        }
        // Select before opening: opaque bytes do not authorize recursive reads.
        for path in index_paths(&packages, request.registry_directory) {
            source
                .copy_file(&path, &target, &mut budget)
                .with_context(|| format!("required index metadata input missing/unsafe: {path}"))?;
        }
        source.verify()?;
        let entries = io::tree(&target, None, "", &mut io::Budget::new())?;
        let expanded = archive::derive(&target, request.registry_directory, &packages)?;
        let manifest = Manifest {
            schema_version: 1,
            registry_source: REGISTRY.into(),
            index_provenance: INDEX_PROVENANCE.into(),
            registry_directory: request.registry_directory.into(),
            cargo_sha256: request.cargo_sha256.into(),
            lock_sha256: sha(&bytes),
            packages: packages.clone(),
            entries,
            expanded,
        };
        validate_manifest(&manifest, &bytes, request.cargo_sha256)?;
        let encoded = serde_json::to_vec(&manifest)?;
        ensure!(
            encoded.len() <= io::MAX_DOCUMENT,
            "seed manifest exceeds 16MiB"
        );
        let path = request.destination.join(MANIFEST);
        io::verify_root(request.destination, &target)?;
        io::manifest(&target, &encoded)?;
        lock.verify()?;
        let config = SeedConfig {
            root: request.destination.into(),
            manifest_path: path,
            expected_sha256: sha(&encoded),
        };
        let held = HeldSeed::capture(&config, &bytes, request.cargo_sha256)?;
        io::verify_root(request.destination, &target)?;
        source.verify()?;
        Ok(Provisioned {
            config,
            identity: held.identity(),
        })
    };
    operation().with_context(|| {
        format!(
            "Cargo seed provisioning failed; owned destination retained: {}",
            request.destination.display()
        )
    })
}
/// Holds only the root and manifest. Per-file descriptors are opened, hashed and
/// closed sequentially, with bounded fresh enumeration on every verify/stage.
pub struct HeldSeed {
    config: SeedConfig,
    root: DirectoryBinding,
    manifest_file: BoundFile,
    manifest: Manifest,
    identity: SeedIdentity,
}
impl HeldSeed {
    pub fn capture(
        config: &SeedConfig,
        lock_bytes: &[u8],
        expected_cargo_sha256: &str,
    ) -> Result<Self> {
        hash(&config.expected_sha256)?;
        io::root(&config.root)?;
        ensure!(
            config.manifest_path == config.root.join(MANIFEST),
            "seed manifest must be named inside its exact root"
        );
        let meta = fs::symlink_metadata(&config.manifest_path)?;
        ensure!(
            meta.is_file()
                && meta.nlink() == 1
                && meta.mode() & 0o022 == 0
                && (meta.uid() == 0 || meta.uid() == unsafe { libc::geteuid() }),
            "unsafe seed manifest identity/mode"
        );
        let root = DirectoryBinding::open(&config.root, None)?;
        let (manifest_file, bytes) =
            candidate_inputs::repository_live(&config.manifest_path, io::MAX_DOCUMENT)?;
        ensure!(
            sha(&bytes) == config.expected_sha256,
            "seed manifest digest mismatch"
        );
        let value = crate::tool_schema::parse_document_with_limit(
            std::str::from_utf8(&bytes)?,
            io::MAX_DOCUMENT,
        )?;
        let manifest: Manifest = serde_json::from_value(value)?;
        validate_manifest(&manifest, lock_bytes, expected_cargo_sha256)?;
        let derived = archive::derive(
            &io::root(&config.root)?,
            &manifest.registry_directory,
            &manifest.packages,
        )?;
        ensure!(
            derived == manifest.expanded,
            "seed expanded source inventory differs from locked archives"
        );
        let identity = identity(&manifest, config.expected_sha256.clone())?;
        let held = Self {
            config: config.clone(),
            root,
            manifest_file,
            manifest,
            identity,
        };
        held.verify()?;
        Ok(held)
    }
    pub fn identity(&self) -> SeedIdentity {
        self.identity.clone()
    }
    pub fn verify(&self) -> Result<()> {
        self.root.verify()?;
        self.manifest_file.verify()?;
        let entries = io::tree(
            &io::root(&self.config.root)?,
            None,
            "",
            &mut io::Budget::new(),
        )?;
        ensure!(
            entries == self.manifest.entries,
            "Cargo seed contents or exact entry set changed"
        );
        self.manifest_file.verify()?;
        self.root.verify()
    }
    pub fn stage(&self, destination: &Path) -> Result<StagedSeed> {
        self.verify()?;
        let target = io::new_root(
            destination,
            &[&self.config.root, &self.config.manifest_path],
        )?;
        let operation = || -> Result<StagedSeed> {
            let entries = io::tree(
                &io::root(&self.config.root)?,
                Some(&target),
                "",
                &mut io::Budget::new(),
            )?;
            ensure!(
                entries == self.manifest.entries,
                "seed changed during stage"
            );
            archive::materialize(
                &io::root(&self.config.root)?,
                &target,
                &self.manifest.registry_directory,
                &self.manifest.packages,
                &self.manifest.expanded,
            )?;
            self.verify()?;
            io::verify_root(destination, &target)?;
            let staged = StagedSeed {
                root: destination.into(),
                directory: DirectoryBinding::open(destination, None)?,
                manifest: self.manifest.clone(),
                identity: self.identity.clone(),
            };
            staged.verify()?;
            Ok(staged)
        };
        operation().with_context(|| {
            format!(
                "Cargo seed staging failed; owned destination retained: {}",
                destination.display()
            )
        })
    }
}
pub struct StagedSeed {
    root: PathBuf,
    directory: DirectoryBinding,
    manifest: Manifest,
    identity: SeedIdentity,
}
impl StagedSeed {
    pub fn identity(&self) -> SeedIdentity {
        self.identity.clone()
    }
    pub fn verify(&self) -> Result<()> {
        self.directory.verify()?;
        validate_runtime_layout(&self.root)?;
        let root = io::root(&self.root)?;
        let mut entries = Vec::new();
        let mut budget = io::Budget::new();
        for group in ["cache", "index"] {
            entries.extend(io::tree(
                &root,
                None,
                &format!("registry/{group}"),
                &mut budget,
            )?);
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let expected: Vec<_> = self
            .manifest
            .entries
            .iter()
            .filter(|e| e.path != "registry")
            .cloned()
            .collect();
        ensure!(
            entries == expected,
            "staged Cargo seed inputs or exact entry set changed"
        );
        archive::verify(
            &root,
            &self.manifest.registry_directory,
            &self.manifest.expanded,
        )?;
        self.directory.verify()
    }
}

// These are writable Cargo-generated outputs, not admitted input identities.
// In particular, configuration/auth/executable injection never becomes an
// allowed cache output just because it is outside the pinned subtrees.
fn validate_runtime_layout(root: &Path) -> Result<()> {
    let allowed = [
        "registry",
        ".package-cache",
        ".package-cache-mutate",
        ".global-cache",
        ".global-cache-journal",
        ".global-cache-wal",
        ".global-cache-shm",
    ];
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().context("runtime Cargo home name not UTF8")?;
        ensure!(
            allowed.contains(&name),
            "unexpected private Cargo home output: {name}"
        );
        let meta = fs::symlink_metadata(entry.path())?;
        ensure!(
            !meta.file_type().is_symlink()
                && if name == "registry" {
                    meta.is_dir()
                } else {
                    meta.is_file() && meta.nlink() == 1 && meta.len() <= io::MAX_FILE
                },
            "unsafe generated Cargo home output"
        );
    }
    for entry in fs::read_dir(root.join("registry"))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().context("runtime registry name not UTF8")?;
        ensure!(
            ["cache", "index", "src", "CACHEDIR.TAG"].contains(&name),
            "unexpected generated registry output"
        );
        let m = fs::symlink_metadata(entry.path())?;
        ensure!(
            !m.file_type().is_symlink()
                && if name == "CACHEDIR.TAG" {
                    m.is_file() && m.nlink() == 1 && m.len() <= 4096
                } else {
                    m.is_dir()
                },
            "unsafe generated registry output"
        );
    }
    Ok(())
}
