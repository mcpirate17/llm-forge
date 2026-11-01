//! Confined, fail-loud syntactic Python caller scan, not a graph edge or proof
//! of semantic calls. Configured host ancestors/mounts and same-UID writers
//! remain trusted; the deadline is cooperative and does not bound filesystem I/O.
//! Hidden, ignored and symlink entries are excluded from caller discovery.
#[path = "graph_context_ignore.rs"]
mod ignore_rules;
use super::GraphRelationship;
use crate::graph_context::files::{SourceRoot, MAX_SOURCE_BYTES};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, Metadata, MetadataExt, OpenOptions, OpenOptionsExt};
use ignore_rules::IgnoreState;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const BYTES: usize = 64_000_000;
const ENTRIES: usize = 100_000;
const HITS: usize = 10_000;
const DEPTH: usize = 128;
#[derive(Clone, Copy)]
struct Limits {
    bytes: usize,
    entries: usize,
    hits: usize,
    depth: usize,
}
struct Scan<'a> {
    sources: &'a SourceRoot,
    pattern: Regex,
    target: PathBuf,
    limits: Limits,
    bytes: usize,
    entries: usize,
    hits: Vec<GraphRelationship>,
    matches: usize,
    ignores: IgnoreState,
    deadline: Instant,
}
fn directory_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    options
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}
impl Scan<'_> {
    fn check_time(&self) -> Result<()> {
        ensure!(
            Instant::now() < self.deadline,
            "syntactic scan deadline exceeded"
        );
        Ok(())
    }
    fn visit(
        &mut self,
        directory: &Dir,
        relative: &Path,
        depth: usize,
        boundary: &mut impl FnMut(&Path) -> Result<()>,
    ) -> Result<()> {
        self.check_time()?;
        ensure!(
            depth <= self.limits.depth,
            "syntactic scan depth limit exceeded"
        );
        self.ignores.enter(self.sources, relative)?;
        let mut entries = Vec::new();
        for entry in directory.entries()? {
            self.entries += 1;
            ensure!(
                self.entries <= self.limits.entries,
                "syntactic scan entry limit exceeded"
            );
            entries.push(entry?);
        }
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            self.check_time()?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .context("syntactic scan entry name is not UTF-8")?;
            if name.starts_with('.') {
                continue;
            }
            let path = relative.join(name);
            let metadata = directory.symlink_metadata(name)?;
            if self
                .ignores
                .ignored(&self.sources.host().join(&path), metadata.is_dir())
            {
                continue;
            }
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                let opened = directory.open_with(name, &directory_options())?;
                ensure!(
                    same(&metadata, &opened.metadata()?),
                    "syntactic scan directory changed"
                );
                let child = Dir::from_std_file(opened.into_std());
                self.visit(&child, &path, depth + 1, boundary)?;
                ensure!(
                    same(&metadata, &directory.symlink_metadata(name)?)
                        && same(&metadata, &child.dir_metadata()?),
                    "syntactic scan directory changed"
                );
            } else if name.ends_with(".py") {
                ensure!(
                    metadata.is_file(),
                    "syntactic scan Python source is not a regular file: {}",
                    path.display()
                );
                boundary(&path)?;
                self.source(&path)?;
            }
        }
        self.ignores.leave();
        Ok(())
    }
    fn source(&mut self, path: &Path) -> Result<()> {
        let path_text = path
            .to_str()
            .context("syntactic scan source path is not UTF-8")?;
        let left = self.limits.bytes.saturating_sub(self.bytes);
        let snapshot = self
            .sources
            .read_text(path_text, left.clamp(1, MAX_SOURCE_BYTES))?
            .context("syntactic scan source disappeared")?;
        self.bytes = self
            .bytes
            .checked_add(snapshot.text.len())
            .context("syntactic scan byte counter overflow")?;
        ensure!(
            self.bytes <= self.limits.bytes,
            "syntactic scan byte limit exceeded"
        );
        for (line, text) in snapshot.text.lines().enumerate() {
            if self.pattern.is_match(text) {
                self.matches += 1;
                ensure!(
                    self.matches <= self.limits.hits,
                    "syntactic caller count exceeds graph context limit"
                );
                if path != self.target {
                    self.hits.push(GraphRelationship {
                        qualified_name: format!("{path_text}:{}", line + 1),
                        kind: "calls (syntactic)".into(),
                        file_path: path_text.to_owned(),
                    });
                }
            }
        }
        Ok(())
    }
}
pub(in crate::graph_context) fn syntactic_callers(
    repo: &Path,
    symbol: &str,
    target_file: &str,
) -> Result<Vec<GraphRelationship>, String> {
    run(
        repo,
        symbol,
        target_file,
        Limits {
            bytes: BYTES,
            entries: ENTRIES,
            hits: HITS,
            depth: DEPTH,
        },
        |_| Ok(()),
    )
    .map_err(|error| format!("{error:#}"))
}
fn run(
    repo: &Path,
    symbol: &str,
    target_file: &str,
    limits: Limits,
    mut boundary: impl FnMut(&Path) -> Result<()>,
) -> Result<Vec<GraphRelationship>> {
    ensure!(
        !symbol.is_empty() && symbol.len() <= 512 && !symbol.chars().any(char::is_control),
        "invalid syntactic caller symbol"
    );
    let sources = SourceRoot::open(repo)?;
    let target = sources.normalize(target_file)?;
    let pattern = Regex::new(&format!(r"\b{}\(", regex::escape(symbol)))?;
    let root = sources.directory()?;
    let mut scan = Scan {
        sources: &sources,
        pattern,
        target,
        limits,
        bytes: 0,
        entries: 0,
        hits: Vec::new(),
        matches: 0,
        ignores: IgnoreState::new(),
        deadline: Instant::now() + Duration::from_secs(5),
    };
    scan.visit(&root, Path::new(""), 0, &mut boundary)?;
    scan.ignores.verify(&sources, || scan.check_time())?;
    sources.verify()?;
    Ok(scan.hits)
}
#[cfg(test)]
#[path = "graph_context_syntactic_tests.rs"]
mod tests;
