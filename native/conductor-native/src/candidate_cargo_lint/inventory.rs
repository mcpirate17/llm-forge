use super::{directory, io_error, joined, metadata, path_parts, Budget, MAX_DEPTH};
use crate::candidate_policy::{GlobWorkspace, PreparedGlob};
use cap_std::fs::Dir;
use std::collections::BTreeSet;
pub(super) struct Pattern {
    segments: Vec<Segment>,
}
enum Segment {
    Recursive,
    Name {
        raw: String,
        literal: bool,
        glob: PreparedGlob,
    },
}
impl Pattern {
    pub(super) fn new(raw: &str) -> Result<Self, String> {
        let parts = path_parts(raw, false)?;
        if parts.last() == Some(&"**") {
            return Err("lint manifest glob must end in a file component, not **".into());
        }
        let segments = parts
            .into_iter()
            .map(|p| {
                if p == "**" {
                    Segment::Recursive
                } else {
                    Segment::Name {
                        raw: p.into(),
                        literal: !p.contains(['*', '?', '[']),
                        glob: PreparedGlob::new(p),
                    }
                }
            })
            .collect();
        Ok(Self { segments })
    }
}
struct Walker<'a> {
    found: BTreeSet<String>,
    budget: &'a mut Budget,
    matching: GlobWorkspace,
}
pub(super) fn collect(
    root: &Dir,
    patterns: &[Pattern],
    budget: &mut Budget,
) -> Result<BTreeSet<String>, String> {
    let mut walker = Walker {
        found: BTreeSet::new(),
        budget,
        matching: GlobWorkspace::default(),
    };
    for pattern in patterns {
        walker.walk(root, ".", &pattern.segments, 0, 0)?;
    }
    Ok(walker.found)
}
impl Walker<'_> {
    fn walk(
        &mut self,
        dir: &Dir,
        prefix: &str,
        segments: &[Segment],
        index: usize,
        depth: usize,
    ) -> Result<(), String> {
        if depth > MAX_DEPTH {
            return Err("lint inventory directory depth exceeds 128".into());
        }
        let segment = &segments[index];
        if let Segment::Name {
            raw, literal: true, ..
        } = segment
        {
            return self.named(dir, prefix, raw, segments, index, depth);
        }
        if matches!(segment, Segment::Recursive) {
            self.walk(dir, prefix, segments, index + 1, depth)?;
        }
        self.budget.probe()?;
        let entries = dir
            .entries()
            .map_err(|e| io_error("enumerate manifest directory", &e))?;
        for entry in entries {
            self.budget.entry()?;
            let entry = entry.map_err(|e| io_error("read manifest entry", &e))?;
            let name = entry.file_name();
            let name = name.to_str().ok_or("lint inventory path must be UTF-8")?;
            match segment {
                Segment::Recursive => {
                    let Some(meta) = metadata(dir, name, self.budget)? else {
                        return Err("lint inventory entry disappeared".into());
                    };
                    if meta.is_dir() {
                        let child = directory(dir, name, self.budget)?;
                        self.walk(&child, &joined(prefix, name)?, segments, index, depth + 1)?;
                    }
                }
                Segment::Name { raw, glob, .. } => {
                    self.budget.matching(raw, name)?;
                    if self.matching.matches(name, glob) {
                        self.named(dir, prefix, name, segments, index, depth)?;
                    }
                }
            }
        }
        Ok(())
    }
    fn named(
        &mut self,
        dir: &Dir,
        prefix: &str,
        name: &str,
        segments: &[Segment],
        index: usize,
        depth: usize,
    ) -> Result<(), String> {
        let Some(meta) = metadata(dir, name, self.budget)? else {
            return Ok(());
        };
        let path = joined(prefix, name)?;
        if index + 1 == segments.len() {
            if !meta.is_file() {
                return Err("lint manifest glob matched a nonregular file".into());
            }
            if !self.found.contains(&path) && self.found.len() >= 10_000 {
                return Err("lint inventory manifests exceed 10000".into());
            }
            if !self.found.contains(&path) {
                self.budget.retain(path.len())?;
            }
            self.found.insert(path);
        } else if meta.is_dir() {
            let child = directory(dir, name, self.budget)?;
            self.walk(&child, &path, segments, index + 1, depth + 1)?;
        } else if !meta.is_file() {
            return Err("lint manifest traversal encountered a special file".into());
        }
        Ok(())
    }
}
