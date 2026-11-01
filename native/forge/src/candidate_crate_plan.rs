//! Bound current manifests/build inputs and immutable integration-base blobs.
use crate::{
    candidate_inputs::{self, BoundFile},
    candidate_source::{Candidate, Source, TreeEntry},
};
use anyhow::{ensure, Context, Result};
use cap_std::fs::Dir;
use conductor_native::{
    candidate_cargo_lint::read_optional, candidate_crate_version::normalize_distribution_name,
};
use git2::{ObjectType, Oid};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const MAX_BYTES: usize = 1024 * 1024;
const MAX_TOTAL: usize = 32 * 1024 * 1024;

pub struct Plan {
    directory: Dir,
    held: Vec<BoundFile>,
    absent: Vec<String>,
    bytes: usize,
    pub payload: Value,
    pub selected: Vec<String>,
    pub manifests: Vec<String>,
    pub wanted: BTreeSet<String>,
}

fn build_input(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| {
            ["rs", "c", "cc", "cpp", "cu", "h", "hpp"]
                .iter()
                .any(|ext| s.eq_ignore_ascii_case(ext))
        })
}

fn source_paths(candidate: &Candidate, selected: &[String]) -> Result<BTreeSet<String>> {
    let mut paths: BTreeSet<_> = selected.iter().cloned().collect();
    for change in &candidate.changes {
        if change.status.starts_with('R') && selected.binary_search(&change.path).is_ok() {
            let old = change.old_path.as_ref().context("rename has no old path")?;
            crate::candidate_source::safe_path(old.as_bytes())?;
            ensure!(
                !old.chars().any(char::is_control) && old.split('/').count() <= 128,
                "invalid old crate-version path"
            );
            paths.insert(old.clone());
            ensure!(paths.len() <= 4096, "crate source endpoints exceed 4096");
        }
    }
    Ok(paths)
}

impl Plan {
    pub fn new(source: &Source, candidate: &Candidate, files: &[String]) -> Result<Self> {
        ensure!(
            !files.is_empty() && files.len() <= 4096,
            "crate-version requires 1..4096 selected changes"
        );
        let captured: BTreeSet<_> = candidate.changes.iter().map(|c| c.path.as_str()).collect();
        let mut selected = BTreeSet::new();
        for file in files {
            crate::candidate_source::safe_path(file.as_bytes())?;
            ensure!(
                !file.chars().any(char::is_control) && file.split('/').count() <= 128,
                "invalid crate-version path"
            );
            ensure!(
                selected.insert(file.clone()),
                "duplicate selected crate-version path"
            );
            ensure!(
                captured.contains(file.as_str()),
                "selected path has no captured change: {file}"
            );
        }
        let current = source.list_tree(&candidate.tree_oid)?;
        let base = source.list_tree(&candidate.base_tree_oid)?;
        let current: BTreeMap<_, _> = current
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        let base: BTreeMap<_, _> = base
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        let mut plan = Self {
            directory: Dir::open_ambient_dir(source.root(), cap_std::ambient_authority())?,
            held: Vec::new(),
            absent: Vec::new(),
            bytes: 0,
            payload: Value::Null,
            selected: selected.into_iter().collect(),
            manifests: Vec::new(),
            wanted: BTreeSet::new(),
        };
        let mut manifests = Vec::new();
        for (path, entry) in &current {
            if Path::new(path).file_name().and_then(|n| n.to_str()) != Some("Cargo.toml") {
                continue;
            }
            ensure!(
                manifests.len() < 4096,
                "candidate manifest count exceeds 4096"
            );
            let text = plan.current(source.root(), entry)?;
            if let Ok(value) = toml::from_str::<toml::Value>(&text) {
                if let Some(name) = value
                    .get("package")
                    .and_then(|p| p.get("name"))
                    .and_then(|n| n.as_str())
                {
                    plan.wanted
                        .insert(normalize_distribution_name(name).map_err(anyhow::Error::msg)?);
                }
            }
            let before = plan.base(source, base.get(path))?;
            manifests.push(json!({"path":path,"current":text,"base":before}));
            plan.manifests.push(path.clone());
        }
        let mut sources = Vec::new();
        for path in source_paths(candidate, &plan.selected)? {
            if !build_input(&path) {
                continue;
            }
            let text = match current.get(&path) {
                Some(entry) => Some(plan.current(source.root(), entry)?),
                None => {
                    plan.absent.push(path.clone());
                    None
                }
            };
            let before = plan.base(source, base.get(&path))?;
            sources.push(json!({"path":path,"current":text,"base":before}));
        }
        plan.payload = json!({"manifests":manifests,"sources":sources,"installed":[]});
        crate::json_canon::encoded_size(&plan.payload, MAX_TOTAL)?;
        plan.verify()?;
        Ok(plan)
    }

    fn add(&mut self, bytes: usize) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .context("crate input byte overflow")?;
        ensure!(
            self.bytes <= MAX_TOTAL,
            "crate source/manifest inputs exceed 32 MiB"
        );
        Ok(())
    }

    fn current(&mut self, root: &Path, entry: &TreeEntry) -> Result<String> {
        regular(entry)?;
        let (held, bytes) = candidate_inputs::snapshot(root, &entry.path, &entry.oid, MAX_BYTES)?;
        self.add(bytes.len())?;
        self.held.push(held);
        String::from_utf8(bytes).context("current crate input must be UTF-8")
    }

    fn base(&mut self, source: &Source, entry: Option<&TreeEntry>) -> Result<Option<String>> {
        let Some(entry) = entry else {
            return Ok(None);
        };
        regular(entry)?;
        let oid = Oid::from_str(&entry.oid)?;
        let (size, kind) = source.repository().odb()?.read_header(oid)?;
        ensure!(
            kind == ObjectType::Blob && size <= MAX_BYTES,
            "invalid/oversized base crate input"
        );
        self.add(size)?;
        let blob = source.repository().find_blob(oid)?;
        ensure!(
            Oid::hash_object(ObjectType::Blob, blob.content())? == oid,
            "base crate blob digest mismatch"
        );
        Ok(Some(
            std::str::from_utf8(blob.content())
                .context("base crate input must be UTF-8")?
                .to_owned(),
        ))
    }

    pub fn verify(&self) -> Result<()> {
        for file in &self.held {
            file.verify()?;
        }
        for path in &self.absent {
            ensure!(
                read_optional(&self.directory, path, MAX_BYTES)
                    .map_err(anyhow::Error::msg)?
                    .is_none(),
                "deleted crate source unexpectedly present: {path}"
            );
        }
        Ok(())
    }
}

fn regular(entry: &TreeEntry) -> Result<()> {
    ensure!(
        entry.object_type == "blob"
            && matches!(entry.mode.as_str(), "100644" | "100755")
            && entry.size.is_some_and(|size| size <= MAX_BYTES as u64),
        "crate input must be a regular Git blob of at most 1 MiB: {}",
        entry.path
    );
    Ok(())
}
