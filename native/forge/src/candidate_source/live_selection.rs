//! Select tracked paths from an already captured live Git tree.
use super::{budget, safe_path, Source, TreeEntry, MAX_ENTRIES, MAX_OUTPUT};
use anyhow::{ensure, Context, Result};
use git2::{ErrorCode, Index, Oid};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LiveSelectionMode {
    ChangedFrom(String),
    AllTracked,
}

#[derive(Debug, PartialEq, Eq)]
struct Evidence {
    root: PathBuf,
    index_path: PathBuf,
    index_hash: Option<String>,
    head: String,
    base: Option<String>,
    merge_base: Option<String>,
    captured_root: PathBuf,
    captured_tree: String,
    filemode: bool,
    paths: Vec<String>,
}

pub(crate) struct LiveSelection {
    mode: LiveSelectionMode,
    evidence: Evidence,
}

impl LiveSelection {
    pub(crate) fn paths(&self) -> &[String] {
        &self.evidence.paths
    }
    pub(crate) fn evidence(&self) -> Value {
        let (kind, reference) = match &self.mode {
            LiveSelectionMode::ChangedFrom(reference) => ("changed_from", Some(reference)),
            LiveSelectionMode::AllTracked => ("all_tracked", None),
        };
        json!({"mode": kind, "reference": reference, "head": self.evidence.head,
            "base": self.evidence.base, "merge_base": self.evidence.merge_base,
            "index_path": self.evidence.index_path, "index_sha256": self.evidence.index_hash,
            "captured_tree": self.evidence.captured_tree, "core_filemode": self.evidence.filemode})
    }
    pub(crate) fn verify(
        &self,
        source: &Source,
        captured: &Source,
        captured_tree: &str,
    ) -> Result<()> {
        ensure!(
            source.selection_evidence(&self.mode, captured, captured_tree)? == self.evidence,
            "live path selection changed"
        );
        Ok(())
    }
}

impl Source {
    /// The caller retains the validated raw live-snapshot controls and verifies
    /// their config, environment and file bindings before accepting this selection.
    pub(crate) fn select_live_paths(
        &self,
        mode: LiveSelectionMode,
        captured: &Source,
        captured_tree: &str,
    ) -> Result<LiveSelection> {
        let evidence = self.selection_evidence(&mode, captured, captured_tree)?;
        let binding = LiveSelection { mode, evidence };
        binding.verify(self, captured, captured_tree)?;
        Ok(binding)
    }

    fn selection_evidence(
        &self,
        mode: &LiveSelectionMode,
        captured: &Source,
        captured_tree: &str,
    ) -> Result<Evidence> {
        let (index_path, index_hash, index) = index(self)?;
        let head = self.resolve_commit("HEAD")?;
        let filemode = bool_config(self, "core.filemode", true)?;
        let (base, merge_base, paths) = match mode {
            LiveSelectionMode::AllTracked => (None, None, tracked(&index)?),
            LiveSelectionMode::ChangedFrom(reference) => {
                let base = self.resolve_commit(reference)?;
                let bases = self
                    .repository
                    .merge_bases(Oid::from_str(&head)?, Oid::from_str(&base)?)?;
                ensure!(
                    bases.len() == 1,
                    "live selection requires one unambiguous merge base"
                );
                let merge_base = bases.first().context("merge base missing")?.to_string();
                let paths = changed(self, captured, captured_tree, &index, &merge_base, filemode)?;
                (Some(base), Some(merge_base), paths)
            }
        };
        Ok(Evidence {
            root: self.root.clone(),
            index_path,
            index_hash,
            head,
            base,
            merge_base,
            captured_root: captured.root.clone(),
            captured_tree: captured_tree.to_owned(),
            filemode,
            paths,
        })
    }
}

fn index(source: &Source) -> Result<(PathBuf, Option<String>, Index)> {
    let path = match std::env::var_os("GIT_INDEX_FILE") {
        Some(path) if Path::new(&path).is_absolute() => PathBuf::from(path),
        Some(path) => source.root.join(path),
        None => source.repository.path().join("index"),
    };
    ensure!(
        path.as_os_str().len() <= 4096,
        "Git index path exceeds 4096 bytes"
    );
    match crate::bounded_file::read(&path, MAX_OUTPUT) {
        Ok(raw) => {
            let hash = format!("{:x}", Sha256::digest(&raw));
            let parsed = super::parse_index(&raw)?;
            for entry in parsed.iter() {
                ensure!(
                    entry.flags & 0xf000 == 0 && entry.flags_extended == 0,
                    "live selection refuses special Git index flags"
                );
            }
            Ok((path, Some(hash), parsed))
        }
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok((path, None, Index::new()?))
        }
        Err(error) => Err(error).context("read live selection index"),
    }
}

fn tracked(index: &Index) -> Result<Vec<String>> {
    let mut paths = BTreeSet::new();
    let mut bytes = 0;
    for entry in index.iter() {
        let path = safe_path(&entry.path)?;
        budget(&mut bytes, path.len())?;
        ensure!(
            paths.len() < MAX_ENTRIES,
            "live selection exceeds path limit"
        );
        paths.insert(path.to_owned());
    }
    Ok(paths.into_iter().collect())
}

fn bool_config(source: &Source, key: &str, default: bool) -> Result<bool> {
    match source.repository.config()?.get_bool(key) {
        Ok(value) => Ok(value),
        Err(error) if error.code() == ErrorCode::NotFound => Ok(default),
        Err(error) => Err(error).with_context(|| format!("invalid Git setting {key}")),
    }
}

fn entries(source: &Source, tree: &str) -> Result<BTreeMap<String, TreeEntry>> {
    let mut entries = BTreeMap::new();
    for entry in source.list_tree(tree)? {
        ensure!(
            entries.insert(entry.path.clone(), entry).is_none(),
            "duplicate path in live selection tree"
        );
    }
    Ok(entries)
}

fn changed(
    source: &Source,
    captured: &Source,
    captured_tree: &str,
    index: &Index,
    base: &str,
    filemode: bool,
) -> Result<Vec<String>> {
    ensure!(
        !bool_config(source, "core.autocrlf", false)?,
        "live selection refuses core.autocrlf"
    );
    let base_tree = source.commit_tree(base)?;
    let old = entries(source, &base_tree)?;
    let new = entries(captured, captured_tree)?;
    ensure!(
        index.iter().all(|entry| entry.mode != 0o160000),
        "live changed selection refuses submodules"
    );
    let tracked = tracked(index)?;
    refuse_attributes(source, &tracked)?;
    let mut result = Vec::new();
    for path in tracked {
        let Some(current) = new.get(&path) else {
            continue;
        };
        let differs = old.get(&path).is_none_or(|prior| {
            prior.oid != current.oid || !same_mode(&prior.mode, &current.mode, filemode)
        });
        if differs {
            result.push(path);
        }
    }
    Ok(result)
}

fn same_mode(old: &str, new: &str, filemode: bool) -> bool {
    old == new
        || (!filemode && matches!(old, "100644" | "100755") && matches!(new, "100644" | "100755"))
}

fn refuse_attributes(source: &Source, paths: &[String]) -> Result<()> {
    let config = source.repository.config()?;
    match config.get_string("core.attributesfile") {
        Ok(_) => anyhow::bail!("live selection refuses custom Git attributes file"),
        Err(error) if error.code() == ErrorCode::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let mut locations = BTreeSet::new();
    let mut bytes = 0;
    insert_attribute_path(
        &mut locations,
        &mut bytes,
        source.repository.commondir().join("info/attributes"),
    )?;
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));
    if let Some(home) = config_home {
        insert_attribute_path(&mut locations, &mut bytes, home.join("git/attributes"))?;
    }
    for path in paths {
        ensure!(
            Path::new(path)
                .file_name()
                .is_none_or(|name| name != ".gitattributes"),
            "live selection refuses indexed Git attributes files"
        );
        let mut parent = Path::new(path).parent();
        while let Some(dir) = parent {
            insert_attribute_path(
                &mut locations,
                &mut bytes,
                source.root.join(dir).join(".gitattributes"),
            )?;
            parent = dir.parent();
        }
    }
    for path in locations {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error).context("inspect Git attributes path"),
            Ok(_) => anyhow::bail!("live selection refuses Git attributes files"),
        }
    }
    Ok(())
}

fn insert_attribute_path(
    paths: &mut BTreeSet<PathBuf>,
    bytes: &mut usize,
    path: PathBuf,
) -> Result<()> {
    if !paths.contains(&path) {
        ensure!(
            paths.len() < MAX_ENTRIES,
            "Git attribute path count exceeded"
        );
        budget(bytes, path.as_os_str().len())?;
        paths.insert(path);
    }
    Ok(())
}

#[cfg(test)]
#[path = "live_selection_tests.rs"]
mod tests;
