//! Non-writing anchored fallback for legacy mutant patch applicability.
//! The caller must attempt native Git's `apply --check` first; this is not a corpus verdict.
use anyhow::{bail, ensure, Context, Result};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

const MAX_PATCH: usize = 4 * 1024 * 1024;
const MAX_FILE: usize = 16 * 1024 * 1024;
const MAX_HUNKS: usize = 4096;
const MAX_ENTRIES: usize = 200_000;
const MAX_FILES: usize = 4096;
const MAX_SOURCE_LINES: usize = 1_000_000;
const MAX_WORK: usize = 100_000_000;
#[derive(Debug)]
pub struct ResourceLimit(pub &'static str);
impl std::fmt::Display for ResourceLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}
impl std::error::Error for ResourceLimit {}

#[derive(Debug)]
struct Hunk {
    header: String,
    start: usize,
    entries: Vec<(u8, String)>,
}
#[derive(Debug)]
struct FilePatch {
    path: String,
    hunks: Vec<Hunk>,
}
#[derive(Default)]
struct Parser {
    path: Option<String>,
    hunks: Vec<Hunk>,
    header: String,
    entries: Vec<(u8, String)>,
    files: Vec<FilePatch>,
    count: usize,
    entry_count: usize,
}
impl Parser {
    fn hunk(&mut self) -> Result<()> {
        if !self.entries.is_empty() {
            self.count += 1;
            ensure!(self.count <= MAX_HUNKS, "patch exceeds hunk limit");
            let start = self
                .header
                .split_once('-')
                .and_then(|(_, tail)| tail.split_whitespace().next())
                .and_then(|old| old.split(',').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(1);
            self.hunks.push(Hunk {
                header: std::mem::take(&mut self.header),
                start,
                entries: std::mem::take(&mut self.entries),
            });
        } else {
            self.header.clear();
        }
        Ok(())
    }
    fn file(&mut self) -> Result<()> {
        self.hunk()?;
        if let Some(path) = self.path.take() {
            ensure!(!self.hunks.is_empty(), "patch for {path:?} has no hunks");
            ensure!(self.files.len() < MAX_FILES, "patch exceeds file limit");
            self.files.push(FilePatch {
                path,
                hunks: std::mem::take(&mut self.hunks),
            });
        }
        Ok(())
    }
    fn line(&mut self, line: &str) -> Result<()> {
        if line.starts_with("diff --git ") {
            self.file()?;
        } else if let Some(old) = line.strip_prefix("--- ") {
            self.hunk()?;
            ensure!(old.trim() != "/dev/null", "patch creates a file");
        } else if let Some(target) = line.strip_prefix("+++ ") {
            let target = target.trim();
            ensure!(target != "/dev/null", "patch deletes a file");
            let target = target
                .strip_prefix("a/")
                .or_else(|| target.strip_prefix("b/"))
                .unwrap_or(target);
            ensure!(
                target.len() <= 4096
                    && !target.is_empty()
                    && target
                        .split('/')
                        .all(|part| !part.is_empty() && part != "." && part != "..")
                    && Path::new(target)
                        .components()
                        .all(|part| matches!(part, Component::Normal(_))),
                "unsafe patch path"
            );
            self.path = Some(target.into());
        } else if line.starts_with("@@") {
            self.hunk()?;
            self.header = line.into();
        } else if line.starts_with("rename ")
            || line.starts_with("new file mode")
            || line.starts_with("deleted file mode")
        {
            bail!("unsupported patch directive");
        } else if line.starts_with("\\ No newline at end of file") {
        } else if !self.header.is_empty() {
            self.entry_count = self
                .entry_count
                .checked_add(1)
                .context("patch entry count overflow")?;
            ensure!(self.entry_count <= MAX_ENTRIES, "patch exceeds entry limit");
            match line.as_bytes().first().copied() {
                Some(kind @ (b' ' | b'-' | b'+')) => {
                    self.entries.push((kind, line[1..].to_owned()))
                }
                None => self.entries.push((b' ', String::new())),
                _ => (),
            }
        }
        Ok(())
    }
}
fn parse(text: &str) -> Result<Vec<FilePatch>> {
    ensure!(text.len() <= MAX_PATCH, "patch exceeds byte limit");
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut parser = Parser::default();
    // Python splitlines recognizes these non-newline separators as line boundaries.
    for line in normalized.split_terminator([
        '\n', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}', '\u{2029}',
    ]) {
        parser.line(line)?;
    }
    parser.file()?;
    ensure!(!parser.files.is_empty(), "patch contains no file sections");
    Ok(parser.files)
}
fn choose(lines: &[String], hunk: &Hunk, work: &mut usize) -> Result<(usize, usize)> {
    let old: Vec<_> = hunk
        .entries
        .iter()
        .filter(|(kind, _)| *kind != b'+')
        .map(|(_, text)| text)
        .collect();
    let leading = hunk
        .entries
        .iter()
        .take_while(|(kind, _)| *kind == b' ')
        .count();
    let trailing = hunk
        .entries
        .iter()
        .rev()
        .take_while(|(kind, _)| *kind == b' ')
        .count();
    let floor = usize::from(hunk.entries.iter().all(|(kind, _)| *kind != b'-'));
    let max_drop = leading.min(trailing).saturating_sub(floor);
    for drop in 0..=max_drop {
        if old.len() <= 2 * drop {
            break;
        }
        let candidate = &old[drop..old.len() - drop];
        let span = candidate.len();
        if span > lines.len() {
            continue;
        }
        let mut hits = Vec::new();
        for start in 0..=lines.len() - span {
            *work = work
                .checked_add(span)
                .ok_or(ResourceLimit("patch comparison budget overflow"))?;
            if *work > MAX_WORK {
                return Err(ResourceLimit("patch comparison budget exceeded").into());
            }
            if lines[start..start + span]
                .iter()
                .zip(candidate)
                .all(|(a, b)| a == *b)
            {
                hits.push(start);
            }
        }
        if hits.len() == 1 {
            return Ok((hits[0], drop));
        }
        if hits.len() > 1 {
            let target = hunk.start.saturating_add(drop);
            hits.sort_by_key(|hit| (hit.saturating_add(1).abs_diff(target), *hit));
            ensure!(
                hits[0].saturating_add(1).abs_diff(target)
                    != hits[1].saturating_add(1).abs_diff(target),
                "ambiguous patch hunk {:?}",
                hunk.header
            );
            return Ok((hits[0], drop));
        }
    }
    bail!("patch hunk {:?} does not match", hunk.header)
}
fn check_file(text: &str, hunks: &[Hunk], work: &mut usize) -> Result<()> {
    if text.len() > MAX_FILE {
        return Err(ResourceLimit("patch source exceeds byte limit").into());
    }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.bytes().filter(|byte| *byte == b'\n').count() > MAX_SOURCE_LINES {
        return Err(ResourceLimit("patch source exceeds line limit").into());
    }
    let mut lines: Vec<String> = normalized.split('\n').map(str::to_owned).collect();
    if normalized.ends_with('\n') {
        lines.pop();
    }
    for hunk in hunks {
        let (start, drop) = choose(&lines, hunk, work)?;
        let old_count = hunk
            .entries
            .iter()
            .filter(|(kind, _)| *kind != b'+')
            .count();
        let replacement: Vec<_> = hunk
            .entries
            .iter()
            .filter(|(kind, _)| *kind != b'-')
            .map(|(_, text)| text.clone())
            .collect();
        let span = old_count - 2 * drop;
        lines.splice(
            start..start + span,
            replacement[drop..replacement.len() - drop].iter().cloned(),
        );
    }
    Ok(())
}
/// Return the sorted, deduplicated target paths after bounded patch parsing.
pub fn required_paths(patch: &str) -> Result<Vec<String>> {
    let mut paths: Vec<_> = parse(patch)?.into_iter().map(|file| file.path).collect();
    paths.sort_unstable();
    paths.dedup();
    Ok(paths)
}
/// Check captured UTF-8 source bytes in memory, without touching the filesystem.
pub fn check(patch: &str, files: &BTreeMap<String, String>) -> Result<()> {
    let parsed = parse(patch)?;
    let mut work = 0;
    for file in parsed {
        let text = files
            .get(&file.path)
            .with_context(|| format!("patch targets missing file: {}", file.path))?;
        check_file(text, &file.hunks, &mut work)?;
    }
    Ok(())
}
#[cfg(test)]
#[path = "mutation_patch_anchor_tests.rs"]
mod tests;
