//! Interpreter-free, bounded import closure. Parse/read errors are explicit.
use super::{
    collect_python_imports,
    inputs::{safe_lookup, valid_path},
    PythonModuleResolver,
};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
};

use super::inputs::MAX_FILES;

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub module: String,
    pub importers: Vec<String>,
    pub tracked_importers: Vec<String>,
}
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Scan {
    pub records: Vec<Finding>,
    pub files_scanned: usize,
    pub bytes_scanned: u64,
    pub sources: BTreeMap<String, String>,
    pub lookups: BTreeMap<String, bool>,
    pub root_identity: (u64, u64),
}
fn read_source(root: &Path, path: &str, total: &mut u64) -> Result<String> {
    let (bytes, _) = super::inputs::read_raw(root, path, total)?;
    String::from_utf8(bytes).with_context(|| format!("closure source is not UTF-8: {path}"))
}

fn untracked(
    resolver: &PythonModuleResolver,
    importer: &str,
    dotted: &str,
    lookups: &mut BTreeMap<String, bool>,
    bytes: &mut usize,
) -> Result<Option<String>> {
    for path in PythonModuleResolver::candidates(dotted, importer) {
        if safe_lookup(&resolver.repo_root, &path, lookups, bytes)? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}
fn record_safe(
    resolver: &PythonModuleResolver,
    importer: &str,
    base: &str,
    attribute: Option<&str>,
    output: &mut HashSet<String>,
    lookups: &mut BTreeMap<String, bool>,
    bytes: &mut usize,
) -> Result<()> {
    if let Some(attribute) = attribute.filter(|attribute| *attribute != "*") {
        let dotted = if base.is_empty() {
            attribute.to_owned()
        } else {
            format!("{base}.{attribute}")
        };
        if resolver.resolve(&dotted, importer).is_some() {
            return Ok(());
        }
        if let Some(path) = untracked(resolver, importer, &dotted, lookups, bytes)? {
            output.insert(path);
            return Ok(());
        }
    }
    if base.is_empty() || resolver.resolve(base, importer).is_some() {
        return Ok(());
    }
    if resolver.is_first_party(base) && !resolver.is_satisfied(base, importer) {
        if let Some(path) = untracked(resolver, importer, base, lookups, bytes)? {
            output.insert(path);
        }
    }
    Ok(())
}
/// Scan tracked Python imports and transitively discovered untracked modules.
/// No result is a gate verdict. Any invalid path, read, parse or budget failure is an error.
pub fn scan(repo_root: &Path, tracked: &[String]) -> Result<Scan> {
    ensure!(
        tracked.len() <= MAX_FILES && tracked.iter().all(|path| valid_path(path)),
        "invalid or excessive tracked paths"
    );
    let root = fs::canonicalize(repo_root).context("resolve closure repository")?;
    ensure!(root.is_dir(), "closure repository must be a directory");
    let root_stat = fs::metadata(&root)?;
    let identity = (root_stat.dev(), root_stat.ino());
    let resolver = PythonModuleResolver::new(&root, tracked);
    let tracked_set: HashSet<_> = tracked.iter().cloned().collect();
    ensure!(
        tracked_set.len() == tracked.len(),
        "duplicate tracked paths"
    );
    let mut importers = BTreeMap::<String, HashSet<String>>::new();
    let mut seen = HashSet::new();
    let mut frontier: Vec<_> = tracked
        .iter()
        .filter(|path| path.ends_with(".py"))
        .cloned()
        .collect();
    let mut total = 0;
    let mut sources = BTreeMap::new();
    let mut lookups = BTreeMap::new();
    let mut lookup_bytes = 0usize;
    while let Some(path) = frontier.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        ensure!(seen.len() <= MAX_FILES, "closure file budget exceeded");
        let source = read_source(&root, &path, &mut total)?;
        sources.insert(
            path.clone(),
            format!("{:x}", Sha256::digest(source.as_bytes())),
        );
        let statements = ruff_python_parser::parse_module(&source)
            .map_err(|error| anyhow::anyhow!("closure source does not parse: {path}: {error}"))?
            .into_syntax()
            .body;
        let mut imports = Vec::new();
        collect_python_imports(&statements, &path, &mut imports);
        let mut found = HashSet::new();
        for (base, attribute) in imports {
            record_safe(
                &resolver,
                &path,
                &base,
                attribute.as_deref(),
                &mut found,
                &mut lookups,
                &mut lookup_bytes,
            )?;
        }
        for dependency in found {
            importers
                .entry(dependency.clone())
                .or_default()
                .insert(path.clone());
            frontier.push(dependency);
            ensure!(
                frontier.len() <= MAX_FILES,
                "closure frontier budget exceeded"
            );
        }
    }
    let records = importers
        .into_iter()
        .map(|(module, paths)| {
            let mut importers: Vec<_> = paths.into_iter().collect();
            importers.sort_unstable();
            let tracked_importers = importers
                .iter()
                .filter(|path| tracked_set.contains(*path))
                .cloned()
                .collect();
            Finding {
                module,
                importers,
                tracked_importers,
            }
        })
        .collect();
    let after = fs::metadata(repo_root)?;
    ensure!(
        fs::canonicalize(repo_root)? == root && (after.dev(), after.ino()) == identity,
        "closure named root changed during scan"
    );
    Ok(Scan {
        records,
        files_scanned: seen.len(),
        bytes_scanned: total,
        sources,
        lookups,
        root_identity: identity,
    })
}
