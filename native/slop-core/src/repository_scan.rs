//! Deterministic repository scanning and logical-line accounting for audit snapshots.
//!
//! Policy stays in Python. This module owns the filesystem-heavy portion: one recursive
//! walk, exact suffix and relative-component filtering, logical LOC counts, stable test
//! classification, and the content-independent snapshot digest.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

fn is_line_break(ch: char) -> bool {
    matches!(
        ch,
        '\n' | '\r'
            | '\u{000b}'
            | '\u{000c}'
            | '\u{001c}'
            | '\u{001d}'
            | '\u{001e}'
            | '\u{0085}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

fn logical_loc(bytes: &[u8], comment_prefixes: &[String]) -> usize {
    let text = String::from_utf8_lossy(bytes);
    let mut count = 0;
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if !is_line_break(ch) {
            continue;
        }
        let stripped = text[start..index].trim();
        if !stripped.is_empty()
            && !comment_prefixes
                .iter()
                .any(|prefix| stripped.starts_with(prefix))
        {
            count += 1;
        }
        if ch == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
            let (newline_index, _) = chars.next().expect("peeked newline exists");
            start = newline_index + 1;
        } else {
            start = index + ch.len_utf8();
        }
    }
    let stripped = text[start..].trim();
    if !stripped.is_empty()
        && !comment_prefixes
            .iter()
            .any(|prefix| stripped.starts_with(prefix))
    {
        count += 1;
    }
    count
}

fn has_code_suffix(path: &Path, suffixes: &HashSet<String>) -> bool {
    path.extension()
        .and_then(|suffix| suffix.to_str())
        .is_some_and(|suffix| suffixes.contains(&format!(".{}", suffix.to_ascii_lowercase())))
}

fn relative_has_skip_part(relative: &Path, skip_parts: &HashSet<String>) -> bool {
    relative.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|part| skip_parts.contains(part))
    })
}

fn collect_files(
    root: &Path,
    suffixes: &HashSet<String>,
    skip_parts: &HashSet<String>,
    seen: &mut HashSet<PathBuf>,
    files: &mut Vec<PathBuf>,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        if relative_has_skip_part(relative, skip_parts) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_files(&path, suffixes, skip_parts, seen, files);
            continue;
        }
        let is_file = file_type.is_file()
            || (file_type.is_symlink() && fs::metadata(&path).is_ok_and(|meta| meta.is_file()));
        if !is_file || !has_code_suffix(&path, suffixes) {
            continue;
        }
        if !seen.insert(path.clone()) {
            continue;
        }
        files.push(path);
    }
}

fn scan(
    repo: &Path,
    targets: &[String],
    suffixes: &[String],
    skip_parts: &[String],
    comment_prefixes: &[String],
) -> Result<Vec<(String, usize)>, String> {
    let suffixes: HashSet<String> = suffixes
        .iter()
        .map(|suffix| suffix.to_ascii_lowercase())
        .collect();
    let skip_parts: HashSet<String> = skip_parts.iter().cloned().collect();
    let roots: Vec<PathBuf> = if targets.is_empty() {
        vec![repo.to_path_buf()]
    } else {
        targets.iter().map(|target| repo.join(target)).collect()
    };
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        collect_files(&root, &suffixes, &skip_parts, &mut seen, &mut files);
    }
    files.sort();

    let mut records = Vec::with_capacity(files.len());
    for path in files {
        let relative = path.strip_prefix(repo).map_err(|_| {
            format!(
                "audit snapshot path {:?} is outside repository {:?}",
                path.display(),
                repo.display()
            )
        })?;
        let loc = fs::read(&path).map_or(0, |bytes| logical_loc(&bytes, comment_prefixes));
        let relative_text = relative
            .to_str()
            .ok_or_else(|| format!("audit snapshot path {:?} is not UTF-8", path.display()))?
            .replace('\\', "/");
        records.push((relative_text, loc));
    }
    Ok(records)
}

/// Scan code files under `targets`, returning sorted repository-relative path/LOC pairs.
#[pyfunction]
#[pyo3(signature = (repo, targets, suffixes, skip_parts, comment_prefixes))]
pub fn audit_repository_scan(
    py: Python<'_>,
    repo: &str,
    targets: Vec<String>,
    suffixes: Vec<String>,
    skip_parts: Vec<String>,
    comment_prefixes: Vec<String>,
) -> PyResult<Vec<(String, usize)>> {
    let repo = PathBuf::from(repo);
    py.detach(move || scan(&repo, &targets, &suffixes, &skip_parts, &comment_prefixes))
        .map_err(PyValueError::new_err)
}
