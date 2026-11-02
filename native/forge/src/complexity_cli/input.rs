use super::{MAX_BASELINE, MAX_ROWS};
use anyhow::{ensure, Context, Result};
use conductor_native::{
    candidate_policy::{GlobWorkspace, PreparedGlob},
    radon_complexity::{self, Block},
};
use std::os::unix::ffi::OsStrExt;
use std::{
    fs, io,
    path::{Path, PathBuf},
};
const MAX_FILES: usize = 4096;
const MAX_ENTRIES: usize = 65536;
const MAX_SOURCE: usize = 1024 * 1024;
const MAX_AGGREGATE: usize = 32 * 1024 * 1024;

pub(super) struct Scan {
    pub blocks: Vec<Block>,
    pub errors: Vec<(String, String)>,
}
/// Path.resolve(strict=False), using the existing ancestor's canonical identity.
pub(super) fn resolve(path: &Path) -> Result<PathBuf> {
    ensure!(
        path.as_os_str().len() <= 4096,
        "complexity path exceeds 4096 bytes"
    );
    conductor_native::project_paths::absolute_resolved(path).map_err(anyhow::Error::msg)
}

fn label(root: &Path, path: &Path) -> Result<String> {
    let text = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_str()
        .context("source path must be UTF-8")?;
    ensure!(text.len() <= 4096, "complexity path exceeds 4096 bytes");
    Ok(text.into())
}
fn walk(path: &Path, depth: usize, entries: &mut usize, files: &mut Vec<PathBuf>) -> Result<()> {
    ensure!(depth <= 128, "complexity traversal exceeds depth 128");
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        *entries += 1;
        ensure!(
            *entries <= MAX_ENTRIES,
            "complexity traversal exceeds 65536 entries"
        );
        let path = entry.path();
        ensure!(
            path.as_os_str().len() <= 4096,
            "complexity path exceeds 4096 bytes"
        );
        if path
            .file_name()
            .is_some_and(|name| name.as_bytes().ends_with(b".py"))
        {
            ensure!(
                files.len() < MAX_FILES,
                "complexity scan exceeds 4096 source occurrences"
            );
            files.push(path.clone());
        }
        // Path.rglob in CPython3.12 does not descend into child directory symlinks.
        if entry.file_type()?.is_dir() {
            walk(&path, depth + 1, entries, files)?;
        }
    }
    Ok(())
}
fn candidates(root: &Path, paths: &[String]) -> Result<Vec<PathBuf>> {
    ensure!(
        paths.len() <= MAX_FILES,
        "complexity scan exceeds 4096 source occurrences"
    );
    let (mut files, mut entries) = (Vec::new(), 0);
    for raw in paths {
        ensure!(
            raw.len() <= 4096 && !raw.contains('\0'),
            "invalid complexity source path"
        );
        let path = resolve(&root.join(raw))?;
        let metadata = match fs::metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        ensure!(
            metadata.is_file() || metadata.is_dir(),
            "complexity source is not a regular file or directory: {}",
            path.display()
        );
        if metadata.is_file() {
            files.push(path);
        } else {
            let start = files.len();
            walk(&path, 0, &mut entries, &mut files)?;
            files[start..].sort();
        }
        ensure!(
            files.len() <= MAX_FILES,
            "complexity scan exceeds 4096 source occurrences"
        );
    }
    Ok(files)
}
pub(super) fn scan(root: &Path, paths: &[String], excludes: &[String]) -> Result<Scan> {
    ensure!(
        excludes.len() <= 128
            && excludes
                .iter()
                .all(|p| p.len() <= 4096 && !p.contains('\0')),
        "invalid complexity exclusion limits"
    );
    let patterns: Vec<_> = excludes.iter().map(|p| PreparedGlob::new(p)).collect();
    let pattern_bytes: usize = excludes.iter().map(String::len).sum();
    let mut workspace = GlobWorkspace::default();
    let mut result = Scan {
        blocks: Vec::new(),
        errors: Vec::new(),
    };
    let (mut bytes, mut output, mut glob_work) = (0, 0, 0usize);
    for path in candidates(root, paths)? {
        let shown = label(root, &path)?;
        glob_work = glob_work
            .checked_add(shown.len().saturating_mul(pattern_bytes))
            .context("exclusion work overflow")?;
        ensure!(
            glob_work <= 128 * 1024 * 1024,
            "complexity exclusion work exceeds bound"
        );
        if patterns
            .iter()
            .any(|pattern| workspace.matches(&shown, pattern))
        {
            continue;
        }
        // Preserve file-symlink reads, but open only its resolved regular target.
        let raw = path
            .canonicalize()
            .map_err(anyhow::Error::from)
            .and_then(|target| crate::bounded_file::read(&target, MAX_SOURCE));
        let analyzed = match raw {
            Err(error) => Err(error),
            Ok(raw) => {
                bytes += raw.len();
                ensure!(
                    bytes <= MAX_AGGREGATE,
                    "complexity source exceeds aggregate 32 MiB limit"
                );
                std::str::from_utf8(&raw)
                    .map_err(anyhow::Error::from)
                    .and_then(|source| {
                        radon_complexity::analyze(source, &shown).map_err(anyhow::Error::msg)
                    })
            }
        };
        match analyzed {
            Err(error) => {
                let error = format!("{error:#}");
                output += shown.len() + error.len() + 32;
                ensure!(
                    output <= MAX_BASELINE,
                    "complexity errors exceed 16 MiB output budget"
                );
                result.errors.push((shown, error));
            }
            Ok(analysis) => {
                ensure!(
                    result.blocks.len() + analysis.blocks.len() <= MAX_ROWS,
                    "complexity scan exceeds 65536 blocks"
                );
                for block in analysis.blocks {
                    // Reserve conservative JSON ASCII escaping work before retaining rows.
                    output += serde_json::to_vec(&block)?.len() * 6 + 128;
                    ensure!(
                        output <= MAX_BASELINE,
                        "complexity output exceeds 16 MiB budget"
                    );
                    result.blocks.push(block);
                }
            }
        }
    }
    result.blocks.sort_by(|a, b| {
        b.complexity
            .cmp(&a.complexity)
            .then(a.path.cmp(&b.path))
            .then(a.line.cmp(&b.line))
    });
    Ok(result)
}
