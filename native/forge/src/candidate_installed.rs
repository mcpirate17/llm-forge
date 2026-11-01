//! Observe only requested distribution metadata in explicitly ordered operator roots.
//! Metadata versions do not establish which binary another process loaded.
use crate::candidate_inputs::{self, BoundFile, DirectoryBinding};
use anyhow::{ensure, Context, Result};
use conductor_native::candidate_crate_version::normalize_distribution_name as normalize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

const MAX_METADATA: usize = 1024 * 1024;
const MAX_TOTAL: usize = 32 * 1024 * 1024;
const ROOT_ENV: &str = "FORGE_CRATE_INSTALLED_ROOTS";

struct Root {
    path: PathBuf,
    binding: DirectoryBinding,
    wanted: BTreeSet<String>,
    files: Vec<MetadataFile>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MetadataFile {
    path: PathBuf,
    name: String,
}

pub struct Installed {
    roots: Vec<Root>,
    held: Vec<BoundFile>,
    pub packages: Vec<Value>,
    pub evidence: Vec<Value>,
}

fn label_name(label: &str) -> Option<String> {
    let name = if let Some(stem) = label.strip_suffix(".dist-info") {
        let (name, version) = stem.rsplit_once('-')?;
        if version.is_empty() {
            return None;
        }
        name
    } else {
        let mut stem = label.strip_suffix(".egg-info")?;
        if let Some((prefix, tag)) = stem.rsplit_once("-py") {
            if !tag.is_empty()
                && tag.bytes().all(|b| b.is_ascii_digit() || b == b'.')
                && prefix
                    .rsplit_once('-')
                    .is_some_and(|(_, version)| version.starts_with(|c: char| c.is_ascii_digit()))
            {
                stem = prefix;
            }
        }
        match stem.rsplit_once('-') {
            Some((name, version)) if version.starts_with(|c: char| c.is_ascii_digit()) => name,
            _ => stem,
        }
    };
    normalize(name).ok()
}

fn discover(root: &Path, wanted: &BTreeSet<String>) -> Result<Vec<MetadataFile>> {
    let mut files = Vec::new();
    for (index, entry) in fs::read_dir(root)?.enumerate() {
        ensure!(
            index < 32768,
            "installed root exceeds 32768 directory entries"
        );
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(project) = label_name(&name).filter(|name| wanted.contains(name)) else {
            continue;
        };
        let kind = entry.file_type()?;
        let file = if kind.is_dir() {
            entry.path().join(if name.ends_with(".dist-info") {
                "METADATA"
            } else {
                "PKG-INFO"
            })
        } else {
            ensure!(
                kind.is_file() && name.ends_with(".egg-info"),
                "distribution metadata entry is not a regular directory/file"
            );
            entry.path()
        };
        ensure!(
            files.len() < 4096,
            "installed root exceeds 4096 selected metadata records"
        );
        files.push(MetadataFile {
            path: file,
            name: project,
        });
    }
    files.sort();
    Ok(files)
}

fn headers(bytes: &[u8]) -> Result<(String, String)> {
    let text = std::str::from_utf8(bytes).context("distribution metadata must be UTF-8")?;
    let mut name = None;
    let mut version = None;
    let mut previous_identity = false;
    let mut header_bytes = 0usize;
    for line in text.lines() {
        if line.is_empty() {
            break;
        }
        header_bytes += line.len() + 1;
        ensure!(
            header_bytes <= 65536 && !line.contains('\0'),
            "metadata headers exceed size/NUL bound"
        );
        if line.starts_with([' ', '\t']) {
            ensure!(
                !previous_identity,
                "folded package identity header is unsupported"
            );
            continue;
        }
        let (key, value) = line.split_once(':').context("malformed metadata header")?;
        let value = value.trim();
        previous_identity = key.eq_ignore_ascii_case("Name") || key.eq_ignore_ascii_case("Version");
        if key.eq_ignore_ascii_case("Name") {
            ensure!(name.is_none(), "duplicate Name metadata header");
            name = Some(normalize(value).map_err(anyhow::Error::msg)?);
        } else if key.eq_ignore_ascii_case("Version") {
            ensure!(
                version.is_none()
                    && !value.is_empty()
                    && value.len() <= 256
                    && value.bytes().all(|b| b.is_ascii_graphic()),
                "invalid/duplicate Version metadata header"
            );
            version = Some(value.to_owned());
        }
    }
    Ok((
        name.context("missing Name metadata header")?,
        version.context("missing Version metadata header")?,
    ))
}

impl Installed {
    pub fn from_env(wanted: BTreeSet<String>, snapshot: &Path) -> Result<Self> {
        let raw = env::var(ROOT_ENV).with_context(|| format!("{ROOT_ENV} must explicitly name ordered metadata roots as a JSON array ([] declares no installed distributions)"))?;
        ensure!(
            raw.len() <= 128 * 1024,
            "installed roots JSON exceeds 128 KiB"
        );
        let roots: Vec<PathBuf> =
            serde_json::from_str(&raw).context("invalid installed roots JSON array")?;
        Self::capture(&roots, wanted, snapshot)
    }

    fn capture(roots: &[PathBuf], wanted: BTreeSet<String>, snapshot: &Path) -> Result<Self> {
        ensure!(roots.len() <= 32, "installed roots exceed 32");
        let mut out = Self {
            roots: Vec::new(),
            held: Vec::new(),
            packages: Vec::new(),
            evidence: Vec::new(),
        };
        let mut seen_roots = BTreeSet::new();
        let mut found = BTreeMap::new();
        let mut total = 0usize;
        for path in roots {
            ensure!(seen_roots.insert(path), "duplicate installed root");
            let binding = DirectoryBinding::open(path, Some(snapshot))?;
            let effective: BTreeSet<_> = wanted
                .iter()
                .filter(|name| !found.contains_key(*name))
                .cloned()
                .collect();
            let files = discover(path, &effective)?;
            let mut local = BTreeMap::new();
            for file in &files {
                ensure!(
                    out.held.len() < 4096,
                    "installed metadata record count exceeds 4096"
                );
                let (held, raw) = candidate_inputs::observed(&file.path, MAX_METADATA, snapshot)?;
                total = total
                    .checked_add(raw.len())
                    .context("metadata byte overflow")?;
                ensure!(
                    total <= MAX_TOTAL,
                    "selected installed metadata exceeds 32 MiB"
                );
                let (name, version) = headers(&raw)?;
                ensure!(
                    name == file.name,
                    "distribution label and Name metadata disagree"
                );
                ensure!(
                    local.insert(name, version).is_none(),
                    "ambiguous duplicate distribution in one root"
                );
                out.evidence
                    .push(json!({"path":file.path,"sha256":format!("{:x}",Sha256::digest(&raw))}));
                out.held.push(held);
            }
            found.extend(local);
            out.roots.push(Root {
                path: path.clone(),
                binding,
                wanted: effective,
                files,
            });
        }
        out.packages = found
            .into_iter()
            .map(|(name, version)| json!({"name":name,"version":version}))
            .collect();
        out.verify()?;
        Ok(out)
    }

    pub fn verify(&self) -> Result<()> {
        for root in &self.roots {
            root.binding.verify()?;
            ensure!(
                discover(&root.path, &root.wanted)? == root.files,
                "installed metadata selection changed"
            );
        }
        for file in &self.held {
            file.verify()?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "candidate_installed_tests.rs"]
mod tests;
