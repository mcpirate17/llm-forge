//! Capture source bytes and hot-path configuration before any scanner executes.
use super::{config, mode::Mode};
use crate::{
    candidate_inputs::{self, BoundFile},
    candidate_source::{
        lines::{RangePlan, RangeScope},
        Candidate, Source,
    },
};
use anyhow::{ensure, Context, Result};
use cap_std::fs::Dir;
use conductor_native::candidate_cargo_lint::read_optional;
use git2::{ErrorCode, ObjectType, Oid};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

const MAX_AST_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;

#[derive(Serialize)]
pub struct Excluded {
    path: String,
    reason: &'static str,
}

pub struct Plan {
    root: PathBuf,
    directory: Dir,
    held: Vec<BoundFile>,
    absent: Vec<String>,
    pub ranges: RangePlan,
    pub excluded: Vec<Excluded>,
    pub hot_paths: Vec<String>,
    pub config_blob_oid: Option<String>,
    pub source_bytes: usize,
    pub sources: Vec<(usize, String)>,
}

impl Plan {
    pub fn new(
        source: &Source,
        candidate: &Candidate,
        mode: Mode,
        files: &[String],
    ) -> Result<Self> {
        ensure!(
            !files.is_empty() && files.len() <= 4096,
            "scan requires 1..4096 selected paths"
        );
        let mut seen = BTreeSet::new();
        let captured: BTreeSet<_> = candidate.changes.iter().map(|c| c.path.as_str()).collect();
        for file in files {
            crate::candidate_source::safe_path(file.as_bytes())?;
            ensure!(
                !file.chars().any(char::is_control) && file.split('/').count() <= 128,
                "invalid scan path"
            );
            ensure!(seen.insert(file.as_str()), "duplicate scan path");
            ensure!(
                captured.contains(file.as_str()),
                "selected path has no captured change: {file}"
            );
        }
        let root = source.root().to_path_buf();
        let directory = Dir::open_ambient_dir(&root, cap_std::ambient_authority())?;
        let mut plan = Self {
            root,
            directory,
            held: Vec::new(),
            absent: Vec::new(),
            ranges: RangePlan {
                base_tree_oid: candidate.base_tree_oid.clone(),
                candidate_tree_oid: candidate.tree_oid.clone(),
                files: Vec::new(),
            },
            excluded: Vec::new(),
            hot_paths: Vec::new(),
            config_blob_oid: None,
            source_bytes: 0,
            sources: Vec::new(),
        };
        if mode == Mode::HotLoopScan {
            plan.capture_config(source, &candidate.tree_oid)?;
        }
        let mut selected = Vec::new();
        for file in seen {
            if let Some(reason) = mode.exclusion(file, &plan.hot_paths) {
                plan.excluded.push(Excluded {
                    path: file.into(),
                    reason,
                });
            } else {
                selected.push(file);
            }
        }
        plan.ranges = source.changed_ranges(candidate, &selected)?;
        plan.capture_sources()?;
        plan.verify()?;
        Ok(plan)
    }

    fn capture_config(&mut self, source: &Source, tree: &str) -> Result<()> {
        let repo = source.repository();
        let tree = Oid::from_str(tree)?;
        let (size, kind) = repo.odb()?.read_header(tree)?;
        ensure!(
            kind == ObjectType::Tree && size <= 8 * 1024 * 1024,
            "invalid/oversized configuration tree"
        );
        let tree = repo.find_tree(tree)?;
        let entry = match tree.get_path(Path::new("pyproject.toml")) {
            Ok(entry) => Some(entry),
            Err(error) if error.code() == ErrorCode::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if let Some(entry) = entry {
            ensure!(
                entry.kind() == Some(ObjectType::Blob)
                    && matches!(entry.filemode(), 0o100644 | 0o100755),
                "pyproject must be a regular captured file"
            );
            let (size, kind) = repo.odb()?.read_header(entry.id())?;
            ensure!(
                kind == ObjectType::Blob && size <= MAX_AST_BYTES,
                "invalid/oversized captured pyproject"
            );
            let oid = entry.id().to_string();
            let (binding, raw) =
                candidate_inputs::snapshot(&self.root, "pyproject.toml", &oid, MAX_AST_BYTES)?;
            self.hot_paths = config::hot_paths(Some(&raw))?;
            self.config_blob_oid = Some(oid);
            self.held.push(binding);
        } else {
            self.absent.push("pyproject.toml".into());
        }
        Ok(())
    }

    fn capture_sources(&mut self) -> Result<()> {
        for (index, file) in self.ranges.files.iter().enumerate() {
            if file.scope == RangeScope::Deleted {
                self.absent.push(file.path.clone());
                self.excluded.push(Excluded {
                    path: file.path.clone(),
                    reason: "deleted file",
                });
                continue;
            }
            let maximum = if file.scope == RangeScope::Changed {
                MAX_AST_BYTES
            } else {
                8 * 1024 * 1024
            };
            let (held, raw) = candidate_inputs::snapshot(
                &self.root,
                &file.path,
                file.blob_oid.as_deref().context("missing source blob")?,
                maximum,
            )?;
            self.source_bytes = self
                .source_bytes
                .checked_add(raw.len())
                .context("source size overflow")?;
            ensure!(
                self.source_bytes <= MAX_TOTAL_BYTES,
                "scan sources exceed 32 MiB"
            );
            self.held.push(held);
            if file.scope == RangeScope::Changed {
                self.sources.push((
                    index,
                    String::from_utf8(raw).context("scan source must be UTF-8")?,
                ));
            } else {
                self.excluded.push(Excluded {
                    path: file.path.clone(),
                    reason: "no added or modified lines",
                });
            }
        }
        Ok(())
    }

    pub fn verify(&self) -> Result<()> {
        for file in &self.held {
            file.verify()?;
        }
        for path in &self.absent {
            ensure!(
                read_optional(&self.directory, path, MAX_AST_BYTES)
                    .map_err(anyhow::Error::msg)?
                    .is_none(),
                "unexpected file in snapshot: {path}"
            );
        }
        Ok(())
    }
}
