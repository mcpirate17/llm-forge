//! Observed include coverage for one clang invocation/configuration, not a
//! complete dependency inventory or execution/approval attestation. The caller
//! supplies the untruncated stderr of a pinned native tool after successful
//! bounded supervision. Do not combine traces from independent invocations.
use anyhow::{ensure, Context, Result};
use conductor_native::candidate_clang_args::source_path;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    path::{Path, PathBuf},
};
pub(super) const MAX_TRACE: usize = 1024 * 1024;
const MAX_LINE: usize = 8 * 1024;
const MAX_HEADERS: usize = 4096;
const MAX_DEPTH: usize = 128;
const MAX_PATH: usize = 4096;
const MAX_FILTER: usize = 64 * 1024;
const MAX_FILTER_ENTRIES: usize = 4096;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FilterEntry {
    name: String,
    lines: Vec<[u32; 2]>,
}

/// Preserve internally generated canonical entries and add exact observed
/// spellings for required headers only. Normalization establishes membership;
/// string identity (not Path equality) determines filter aliases. The caller
/// owns the single rerun and subsequent stable-trace check.
pub fn expanded_filter(
    stderr: &[u8],
    directory: &Path,
    headers: &[PathBuf],
    base_filter: &str,
) -> Result<String> {
    let mut entries = parse_filter(base_filter)?;
    let names: BTreeMap<_, _> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.name.clone(), index))
        .collect();
    ensure!(
        names.len() == entries.len(),
        "duplicate clang-tidy base filter name"
    );
    let mut encoded_bytes = encoded_size(&entries)?;
    let mut aliases = BTreeMap::new();
    observe(stderr, directory, headers, |canonical, raw| {
        let index = *names.get(path_text(canonical)?).context(
            "clang-tidy include coverage incomplete: canonical header has no filter ranges",
        )?;
        let spelling = if Path::new(raw).is_absolute() {
            raw.to_owned()
        } else {
            directory
                .join(raw)
                .to_str()
                .context("clang-tidy alias is not UTF8")?
                .to_owned()
        };
        ensure!(
            spelling.len() <= MAX_PATH,
            "clang-tidy filter alias exceeds path limit"
        );
        if names.contains_key(&spelling) || aliases.contains_key(&spelling) {
            return Ok(());
        }
        ensure!(
            entries.len() + aliases.len() < MAX_FILTER_ENTRIES,
            "clang-tidy filter entry count exceeds limit"
        );
        let entry = BorrowedEntry {
            name: &spelling,
            lines: &entries[index].lines,
        };
        encoded_bytes = encoded_bytes
            .checked_add(encoded_size(&entry)? + 1)
            .filter(|size| *size <= MAX_FILTER)
            .context("clang-tidy expanded filter exceeds byte limit")?;
        aliases.insert(spelling, index);
        Ok(())
    })?;
    if aliases.is_empty() {
        return Ok(base_filter.to_owned());
    }
    for (name, index) in aliases {
        let lines = entries[index].lines.clone();
        entries.push(FilterEntry { name, lines });
    }
    let mut output = Vec::with_capacity(encoded_bytes);
    serde_json::to_writer(&mut output, &entries)?;
    ensure!(
        output.len() == encoded_bytes && output.len() <= MAX_FILTER,
        "clang-tidy filter serialization size mismatch"
    );
    String::from_utf8(output).context("clang-tidy filter encoding failed")
}

#[derive(Serialize)]
struct BorrowedEntry<'a> {
    name: &'a str,
    lines: &'a [[u32; 2]],
}
struct Size(usize);
impl Write for Size {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|size| *size <= MAX_FILTER)
            .ok_or_else(|| io::Error::other("clang-tidy filter exceeds byte limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn encoded_size(value: &impl Serialize) -> Result<usize> {
    let mut size = Size(0);
    serde_json::to_writer(&mut size, value).context("clang-tidy filter exceeds byte limit")?;
    Ok(size.0)
}
fn parse_filter(text: &str) -> Result<Vec<FilterEntry>> {
    ensure!(
        text.len() <= MAX_FILTER,
        "clang-tidy base filter exceeds byte limit"
    );
    let entries: Vec<FilterEntry> = serde_json::from_str(text)
        .map_err(|_| anyhow::anyhow!("invalid clang-tidy base filter JSON"))?;
    ensure!(
        entries.len() <= MAX_FILTER_ENTRIES,
        "clang-tidy filter entry count exceeds limit"
    );
    for entry in &entries {
        path_text(Path::new(&entry.name))?;
        let normalized = source_path("/", &entry.name).map_err(anyhow::Error::msg)?;
        ensure!(
            Path::new(&entry.name).is_absolute()
                && normalized.to_str() == Some(entry.name.as_str()),
            "clang-tidy base filter name must be normalized and absolute"
        );
        ensure!(
            !entry.lines.is_empty()
                && entry
                    .lines
                    .iter()
                    .all(|[start, end]| *start > 0 && start <= end),
            "invalid clang-tidy filter line range"
        );
    }
    Ok(entries)
}

/// Check that every required snapshot header was printed by LLVM's `-H` trace.
/// Only a leading run of dots followed by one ASCII space introduces a path;
/// the remainder is used exactly, without trimming, lossy UTF8 or basename matching.
/// Paths are lexical: this verifier never opens/canonicalizes a compiler operand.
/// Like any stderr protocol this relies on the pinned compiler's output grammar;
/// it cannot authenticate a byte-identical forged trace line from another source.
#[cfg(test)]
fn verify(stderr: &[u8], directory: &Path, headers: &[PathBuf]) -> Result<()> {
    observe(stderr, directory, headers, |_, _| Ok(()))
}
fn observe(
    stderr: &[u8],
    directory: &Path,
    headers: &[PathBuf],
    mut visit: impl FnMut(&Path, &str) -> Result<()>,
) -> Result<()> {
    ensure!(
        stderr.len() <= MAX_TRACE,
        "clang-tidy include trace exceeds byte limit"
    );
    ensure!(
        stderr
            .split(|byte| *byte == b'\n')
            .all(|line| line.len() <= MAX_LINE),
        "clang-tidy include trace line exceeds byte limit"
    );
    ensure!(
        headers.len() <= MAX_HEADERS,
        "clang-tidy required header count exceeds limit"
    );
    let directory_text = path_text(directory)?;
    let normalized = source_path(directory_text, ".").map_err(anyhow::Error::msg)?;
    ensure!(
        normalized == directory,
        "clang-tidy compile directory must be normalized and absolute"
    );
    let mut required = required(headers)?;
    // No header-coverage obligation: retain cheap input/path admission without
    // decoding unrelated diagnostic text or claiming it proves any inclusion.
    if required.is_empty() {
        return Ok(());
    }
    for line in stderr.split(|byte| *byte == b'\n') {
        let line = std::str::from_utf8(line).context("clang-tidy include trace is not UTF8")?;
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(path) = traced_path(line)? {
            let normalized = source_path(directory_text, path)
                .map_err(|_| anyhow::anyhow!("invalid clang-tidy include trace path"))?;
            if let Some(seen) = required.get_mut(&normalized) {
                *seen = true;
                visit(&normalized, path)?;
            }
        }
    }
    ensure!(
        required.values().all(|seen| *seen),
        "clang-tidy include coverage incomplete: {} required header(s) not observed",
        required.values().filter(|seen| !**seen).count()
    );
    Ok(())
}
fn path_text(path: &Path) -> Result<&str> {
    let text = path
        .to_str()
        .context("clang-tidy coverage path is not UTF8")?;
    ensure!(
        !text.is_empty() && text.len() <= MAX_PATH && !text.chars().any(char::is_control),
        "invalid clang-tidy coverage path"
    );
    Ok(text)
}
fn required(headers: &[PathBuf]) -> Result<BTreeMap<PathBuf, bool>> {
    let mut required = BTreeMap::new();
    for header in headers {
        let text = path_text(header)?;
        ensure!(
            header.is_absolute(),
            "clang-tidy required header must be absolute"
        );
        let normalized = source_path("/", text).map_err(anyhow::Error::msg)?;
        ensure!(
            &normalized == header,
            "clang-tidy required header must be normalized"
        );
        required.insert(normalized, false);
    }
    Ok(required)
}
fn traced_path(line: &str) -> Result<Option<&str>> {
    let depth = line.bytes().take_while(|byte| *byte == b'.').count();
    if depth == 0 || line.as_bytes().get(depth) != Some(&b' ') {
        return Ok(None);
    }
    ensure!(
        depth <= MAX_DEPTH,
        "clang-tidy include trace depth exceeds limit"
    );
    let path = &line[depth + 1..];
    ensure!(
        !path.is_empty() && path.len() <= MAX_PATH && !path.chars().any(char::is_control),
        "invalid clang-tidy include trace path"
    );
    Ok(Some(path))
}
#[cfg(test)]
#[path = "candidate_clang_tidy_coverage_tests.rs"]
mod tests;
