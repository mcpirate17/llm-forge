//! Explicit operator build context and captured Git input bindings.
use super::inputs::{self, BoundFile, DirectoryBinding};
use crate::candidate_source::{Candidate, Source, TreeEntry};
use anyhow::{ensure, Context as _, Result};
use conductor_native::{
    candidate_clang_args::{Mapping, MappingKind},
    candidate_clang_db::{self, Database},
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

const MAX_DB: usize = 32 * 1024 * 1024;
const MAX_CONFIG: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub(super) enum Artifact {
    Snapshot {
        path: String,
    },
    External {
        path: PathBuf,
        sha256: String,
        #[serde(default)]
        source_tree: Option<String>,
    },
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExternalMapping {
    original: PathBuf,
    admitted: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    schema_version: u32,
    source_root: PathBuf,
    database: Artifact,
    resource_dir: PathBuf,
    #[serde(default)]
    mappings: Vec<ExternalMapping>,
    #[serde(default)]
    header_units: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    config: Option<Artifact>,
}

pub(super) struct Context {
    pub database: Database,
    pub mappings: Vec<Mapping>,
    pub resource_dir: PathBuf,
    pub header_units: BTreeMap<String, Vec<String>>,
    pub config: Option<Artifact>,
    pub source_root: PathBuf,
    pub tree: BTreeMap<String, TreeEntry>,
    root: PathBuf,
    files: Vec<BoundFile>,
    directories: BTreeMap<PathBuf, DirectoryBinding>,
    captured: BTreeSet<String>,
    captured_bytes: usize,
}

fn logical_root(path: &Path) -> Result<()> {
    ensure!(
        path.is_absolute()
            && path != Path::new("/")
            && path.as_os_str().as_encoded_bytes().len() <= 4096
            && !path
                .as_os_str()
                .as_encoded_bytes()
                .iter()
                .any(u8::is_ascii_control)
            && path
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_))),
        "build context root must be a bounded normalized absolute path"
    );
    Ok(())
}

impl Context {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn load(source: &Source, candidate: &Candidate) -> Result<Self> {
        let path = PathBuf::from(
            std::env::var_os("FORGE_CLANG_TIDY_CONTEXT")
                .context("database context is required: FORGE_CLANG_TIDY_CONTEXT")?,
        );
        let digest = std::env::var("FORGE_CLANG_TIDY_CONTEXT_SHA256")
            .context("database context digest is required: FORGE_CLANG_TIDY_CONTEXT_SHA256")?;
        let (bound, raw) = inputs::external(&path, &digest, MAX_CONFIG, source.root())?;
        let value =
            crate::tool_schema::parse_document_with_limit(std::str::from_utf8(&raw)?, MAX_CONFIG)?;
        let settings: Settings =
            serde_json::from_value(value).context("invalid tidy context schema")?;
        ensure!(
            settings.schema_version == 1,
            "unsupported tidy context version"
        );
        logical_root(&settings.source_root)?;
        ensure!(
            settings.mappings.len() <= 127 && settings.header_units.len() <= 4096,
            "tidy context count limit exceeded"
        );
        let tree = source
            .list_tree(&candidate.tree_oid)?
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        let mut result = Self {
            database: Database {
                entries: Vec::new(),
            },
            mappings: vec![Mapping {
                original: settings.source_root.clone(),
                admitted: source.root().to_path_buf(),
                kind: MappingKind::Source,
            }],
            resource_dir: settings.resource_dir,
            header_units: settings.header_units,
            config: settings.config,
            source_root: settings.source_root,
            tree,
            root: source.root().to_path_buf(),
            files: vec![bound],
            directories: BTreeMap::new(),
            captured: BTreeSet::new(),
            captured_bytes: 0,
        };
        result.external_directory(&result.resource_dir.clone())?;
        for mapping in settings.mappings {
            logical_root(&mapping.original)?;
            result.external_directory(&mapping.admitted)?;
            result.mappings.push(Mapping {
                original: mapping.original,
                admitted: mapping.admitted,
                kind: MappingKind::External,
            });
        }
        result.validate_headers()?;
        if let Artifact::External { source_tree, .. } = &settings.database {
            ensure!(
                source_tree.as_deref().is_some_and(
                    |tree| tree == candidate.base_tree_oid || tree == candidate.tree_oid
                ),
                "external database source_tree must match candidate or base tree"
            );
        }
        let bytes = result
            .artifact(&settings.database, MAX_DB)
            .context("capture compilation database")?;
        result.database = candidate_clang_db::parse_database(&bytes).map_err(anyhow::Error::msg)?;
        result.verify()?;
        Ok(result)
    }

    fn validate_headers(&self) -> Result<()> {
        let mut total = 0usize;
        for (header, units) in &self.header_units {
            crate::candidate_source::safe_path(header.as_bytes())?;
            ensure!(
                !units.is_empty(),
                "header mapping requires translation units"
            );
            let mut unique = BTreeSet::new();
            for unit in units {
                crate::candidate_source::safe_path(unit.as_bytes())?;
                ensure!(unique.insert(unit), "duplicate mapped translation unit");
                total = total.checked_add(1).context("header mapping overflow")?;
                ensure!(total <= 4096, "header mappings exceed 4096 units");
            }
        }
        Ok(())
    }

    pub fn external_directory(&mut self, path: &Path) -> Result<()> {
        ensure!(
            !path.starts_with(&self.source_root),
            "external inputs must be outside original source root"
        );
        if let Some(tmp) = std::env::var_os("TMPDIR") {
            ensure!(
                !path.starts_with(Path::new(&tmp)),
                "external inputs must be outside stage scratch"
            );
        }
        self.directory(path, true)
    }

    pub fn directory(&mut self, path: &Path, external: bool) -> Result<()> {
        if self.directories.contains_key(path) {
            return Ok(());
        }
        ensure!(
            self.directories.len() < 100_000,
            "too many compiler input directories"
        );
        if !external {
            ensure!(
                path.starts_with(&self.root),
                "source directory escaped snapshot"
            );
        }
        let bound = DirectoryBinding::open(path, if external { Some(&self.root) } else { None })?;
        self.directories.insert(path.to_path_buf(), bound);
        Ok(())
    }

    pub fn artifact(&mut self, artifact: &Artifact, maximum: usize) -> Result<Vec<u8>> {
        let (binding, bytes) = match artifact {
            Artifact::Snapshot { path } => {
                let entry = self.regular(path)?;
                inputs::snapshot(&self.root, path, &entry.oid, maximum)?
            }
            Artifact::External { path, sha256, .. } => {
                ensure!(
                    !path.starts_with(&self.source_root),
                    "external artifact must be outside original source root"
                );
                inputs::external(path, sha256, maximum, &self.root)?
            }
        };
        ensure!(self.files.len() < 8192, "too many captured compiler inputs");
        self.files.push(binding);
        Ok(bytes)
    }

    pub fn regular(&self, path: &str) -> Result<&TreeEntry> {
        crate::candidate_source::safe_path(path.as_bytes())?;
        let entry = self
            .tree
            .get(path)
            .context("compiler input is not captured in candidate tree")?;
        ensure!(
            matches!(entry.mode.as_str(), "100644" | "100755") && entry.object_type == "blob",
            "compiler input must be a regular Git blob"
        );
        Ok(entry)
    }

    pub fn source_file(&mut self, path: &str) -> Result<()> {
        if self.captured.contains(path) {
            return Ok(());
        }
        let bytes = self.artifact(&Artifact::Snapshot { path: path.into() }, 8 * 1024 * 1024)?;
        self.captured_bytes = self
            .captured_bytes
            .checked_add(bytes.len())
            .context("compiler source byte overflow")?;
        ensure!(
            self.captured_bytes <= MAX_DB,
            "compiler source inputs exceed 32 MiB"
        );
        self.captured.insert(path.into());
        Ok(())
    }

    pub fn verify(&self) -> Result<()> {
        for directory in self.directories.values() {
            directory.verify()?;
        }
        for file in &self.files {
            file.verify()?;
        }
        Ok(())
    }
}
