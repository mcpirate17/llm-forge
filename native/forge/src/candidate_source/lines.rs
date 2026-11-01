//! Immutable Git-object changed-line projection. No working-tree/config diff tools.
//! The caller keeps the source ODB immutable during header admission/object reads.
use super::{safe_path, Candidate, Change, Source, EMPTY_TREE};
use anyhow::{ensure, Context, Result};
use git2::{Blob, DiffOptions, ObjectType, Oid, Repository, Tree};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
const MAX_FILES: usize = 100_000;
const MAX_PATH_BYTES: usize = 8 * 1024 * 1024;
const MAX_BLOB: usize = 8 * 1024 * 1024;
const MAX_BLOB_BYTES: usize = 32 * 1024 * 1024;
const MAX_TREE_BYTES: usize = 32 * 1024 * 1024;
const MAX_HUNKS: usize = 100_000;
const MAX_WORK: u64 = 100_000_000;
const MAX_OUTPUT: usize = 16 * 1024 * 1024;
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RangeScope {
    Changed,
    NoChangedLines,
    Deleted,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct FileRanges {
    pub path: String,
    pub old_path: Option<String>,
    pub blob_oid: Option<String>,
    pub scope: RangeScope,
    pub ranges: Vec<LineRange>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RangePlan {
    pub base_tree_oid: String,
    pub candidate_tree_oid: String,
    pub files: Vec<FileRanges>,
}
impl Source {
    pub fn changed_ranges(&self, candidate: &Candidate, selected: &[&str]) -> Result<RangePlan> {
        checked_ranges(
            self.repository(),
            &candidate.base_tree_oid,
            &candidate.tree_oid,
            &candidate.changes,
            selected,
        )
    }
}
#[derive(Clone, Copy)]
struct Entry {
    id: Oid,
    mode: i32,
    kind: Option<ObjectType>,
}
struct Trees<'r> {
    repository: &'r Repository,
    trees: BTreeMap<Oid, Tree<'r>>,
    empty: Oid,
    bytes: usize,
    entries: usize,
    lookups: usize,
}
impl<'r> Trees<'r> {
    fn new(repository: &'r Repository) -> Self {
        Self {
            repository,
            trees: BTreeMap::new(),
            empty: Oid::from_str(EMPTY_TREE).expect("static empty tree"),
            bytes: 0,
            entries: 0,
            lookups: 0,
        }
    }
    fn load(&mut self, oid: Oid) -> Result<()> {
        if self.trees.contains_key(&oid) {
            return Ok(());
        }
        let (size, kind) = self
            .repository
            .odb()?
            .read_header(oid)
            .context("missing Git tree object")?;
        ensure!(
            kind == ObjectType::Tree && size <= MAX_BLOB,
            "invalid/oversized Git tree object"
        );
        self.bytes = self
            .bytes
            .checked_add(size)
            .context("tree metadata overflow")?;
        ensure!(
            self.bytes <= MAX_TREE_BYTES,
            "range tree metadata exceeds 32MiB"
        );
        let tree = self.repository.find_tree(oid)?;
        self.entries = self
            .entries
            .checked_add(tree.len())
            .context("tree entry overflow")?;
        ensure!(
            self.entries <= 400_000,
            "range tree metadata exceeds 400000 entries"
        );
        self.trees.insert(oid, tree);
        Ok(())
    }
    fn lookup(&mut self, tree: Oid, path: &str) -> Result<Option<Entry>> {
        if tree == self.empty {
            return Ok(None);
        }
        let parts: Vec<_> = path.split('/').collect();
        let mut oid = tree;
        for (index, name) in parts.iter().enumerate() {
            self.lookups += 1;
            ensure!(
                self.lookups <= 1_000_000,
                "range path lookups exceed 1000000"
            );
            self.load(oid)?;
            let entry = self.trees[&oid].get_name(name).map(|e| Entry {
                id: e.id(),
                mode: e.filemode(),
                kind: e.kind(),
            });
            let Some(entry) = entry else {
                return Ok(None);
            };
            if index + 1 == parts.len() {
                return Ok(Some(entry));
            }
            if entry.kind != Some(ObjectType::Tree) {
                return Ok(None);
            }
            oid = entry.id;
        }
        Ok(None)
    }
}
struct Budget {
    bytes: usize,
    line_bytes: usize,
    work: u64,
    hunks: usize,
    ranges: usize,
}
impl Budget {
    fn new() -> Self {
        Self {
            bytes: 0,
            line_bytes: 0,
            work: 0,
            hunks: 0,
            ranges: 0,
        }
    }
    fn blob<'r>(&mut self, repository: &'r Repository, oid: Oid) -> Result<Blob<'r>> {
        let (size, kind) = repository
            .odb()?
            .read_header(oid)
            .context("missing source blob")?;
        ensure!(
            kind == ObjectType::Blob && size <= MAX_BLOB,
            "source blob must be at most 8MiB"
        );
        self.bytes = self
            .bytes
            .checked_add(size)
            .context("source byte overflow")?;
        ensure!(
            self.bytes <= MAX_BLOB_BYTES,
            "range source inputs exceed 32MiB"
        );
        let blob = repository.find_blob(oid)?;
        ensure!(
            blob.content().len() == size,
            "source blob changed after admission"
        );
        ensure!(
            !blob.is_binary() && !blob.content().contains(&0),
            "binary source is unsupported for changed-line ranges"
        );
        std::str::from_utf8(blob.content()).context("range source must be UTF-8")?;
        Ok(blob)
    }
    fn diff_work(&mut self, old: &[u8], new: &[u8]) -> Result<u64> {
        self.line_bytes = self
            .line_bytes
            .checked_add(old.len())
            .and_then(|n| n.checked_add(new.len()))
            .context("line byte overflow")?;
        ensure!(
            self.line_bytes <= MAX_BLOB_BYTES,
            "diff line inputs exceed 32MiB"
        );
        let (old_lines, old_max) = stats(old);
        let (new_lines, new_max) = stats(new);
        ensure!(
            old_lines <= 100_000 && new_lines <= 100_000,
            "diff source line count exceeds 100000"
        );
        let work = (old_lines + 1)
            .checked_mul(new_lines + 1)
            .and_then(|n| n.checked_mul(old_max.min(new_max) + 1))
            .context("diff comparison work overflow")?;
        self.work = self.work.checked_add(work).context("diff work overflow")?;
        ensure!(
            self.work <= MAX_WORK,
            "changed-line diff work exceeds 100000000"
        );
        Ok(new_lines)
    }
}
/// Repository-only entry point for already captured candidate metadata. Object IDs
/// and endpoint paths/modes are verified; no ref resolution or rename-source guessing.
pub(crate) fn checked_ranges(
    repository: &Repository,
    base: &str,
    target: &str,
    changes: &[Change],
    selected: &[&str],
) -> Result<RangePlan> {
    let base_oid = oid(base)?;
    let target_oid = oid(target)?;
    let captured = admit(changes, selected)?;
    let mut trees = Trees::new(repository);
    for (text, id) in [(base, base_oid), (target, target_oid)] {
        if text != EMPTY_TREE {
            trees.load(id)?;
        }
    }
    let mut budget = Budget::new();
    let mut files = Vec::with_capacity(selected.len());
    let selected: BTreeSet<&str> = selected.iter().copied().collect();
    for path in selected {
        let change = captured
            .get(path)
            .context("selected path has no captured change")?;
        let (old, new) = verify(&mut trees, base_oid, target_oid, change)?;
        let ranges = project(repository, change, old, new, &mut budget)?;
        files.push(ranges);
    }
    let plan = RangePlan {
        base_tree_oid: base.into(),
        candidate_tree_oid: target.into(),
        files,
    };
    encoded_size(&plan, MAX_OUTPUT)?;
    Ok(plan)
}
fn oid(raw: &str) -> Result<Oid> {
    ensure!(
        (40..=64).contains(&raw.len())
            && raw
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "Git object ID must be bounded canonical lowercase full hex"
    );
    let oid = Oid::from_str(raw).context("invalid full Git object ID")?;
    ensure!(
        raw.len() == oid.as_bytes().len() * 2
            && raw
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "Git object ID must be canonical lowercase full hex"
    );
    Ok(oid)
}
fn path(raw: &str) -> Result<()> {
    safe_path(raw.as_bytes())?;
    ensure!(
        raw.split('/').count() <= 128,
        "range path depth exceeds 128"
    );
    Ok(())
}
fn admit<'a>(changes: &'a [Change], selected: &[&str]) -> Result<BTreeMap<&'a str, &'a Change>> {
    ensure!(
        selected.len() <= MAX_FILES && changes.len() <= MAX_FILES,
        "range file/change count exceeds 100000"
    );
    let mut bytes = 0usize;
    for selected in selected {
        path(selected)?;
        bytes = bytes
            .checked_add(selected.len() + 1)
            .context("path size overflow")?;
    }
    ensure!(bytes <= MAX_PATH_BYTES, "selected range paths exceed 8MiB");
    let mut out = BTreeMap::new();
    let mut captured_bytes = 0usize;
    for change in changes {
        path(&change.path)?;
        captured_bytes = captured_bytes
            .checked_add(change.path.len() + 1)
            .context("captured path size overflow")?;
        if let Some(old) = &change.old_path {
            path(old)?;
            captured_bytes = captured_bytes
                .checked_add(old.len() + 1)
                .context("captured path size overflow")?;
        }
        ensure!(
            captured_bytes <= MAX_PATH_BYTES,
            "captured range paths exceed 8MiB"
        );
        ensure!(
            out.insert(change.path.as_str(), change).is_none(),
            "duplicate captured change path"
        );
    }
    Ok(out)
}
fn endpoint(entry: Option<Entry>, mode: &str, id: &str) -> Result<Option<Entry>> {
    let expected = oid(id)?;
    if mode == "000000" {
        ensure!(
            entry.is_none() && expected.is_zero(),
            "captured absent endpoint differs from tree"
        );
        return Ok(None);
    }
    ensure!(
        matches!(mode, "100644" | "100755"),
        "changed-line source must be a regular Git file"
    );
    let entry = entry.context("captured endpoint missing from tree")?;
    let expected_mode = if mode == "100644" { 0o100644 } else { 0o100755 };
    ensure!(
        entry.kind == Some(ObjectType::Blob) && entry.mode == expected_mode && entry.id == expected,
        "captured endpoint mode/OID differs from tree"
    );
    Ok(Some(entry))
}
fn verify(
    trees: &mut Trees<'_>,
    base: Oid,
    target: Oid,
    change: &Change,
) -> Result<(Option<Entry>, Option<Entry>)> {
    let status = change.status.as_bytes();
    ensure!(!status.is_empty(), "captured change status missing");
    let renamed = matches!(status[0], b'R' | b'C');
    if renamed {
        ensure!(
            status.len() == 4
                && status[1..].iter().all(u8::is_ascii_digit)
                && change.status[1..].parse::<u8>().is_ok_and(|n| n <= 100),
            "invalid rename/copy score"
        );
        ensure!(
            change.old_path.as_deref().is_some_and(|p| p != change.path),
            "rename/copy requires distinct captured old_path"
        );
    } else {
        ensure!(
            status.len() == 1 && matches!(status[0], b'A' | b'D' | b'M'),
            "unsupported change status"
        );
        ensure!(
            change.old_path.is_none(),
            "unexpected old_path on nonrename change"
        );
    }
    let old_path = change.old_path.as_deref().unwrap_or(&change.path);
    let old = endpoint(
        trees.lookup(base, old_path)?,
        &change.old_mode,
        &change.old_oid,
    )?;
    let new = endpoint(
        trees.lookup(target, &change.path)?,
        &change.new_mode,
        &change.new_oid,
    )?;
    match status[0] {
        b'A' => ensure!(old.is_none() && new.is_some(), "invalid addition endpoints"),
        b'D' => ensure!(old.is_some() && new.is_none(), "invalid deletion endpoints"),
        b'M' => ensure!(
            old.is_some()
                && new.is_some()
                && (change.old_oid != change.new_oid || change.old_mode != change.new_mode),
            "invalid modification endpoints"
        ),
        b'R' | b'C' => {
            ensure!(
                old.is_some() && new.is_some(),
                "invalid rename/copy endpoints"
            );
            let remains = trees.lookup(target, old_path)?.is_some();
            ensure!(
                remains == (status[0] == b'C'),
                "captured rename/copy source presence differs from tree"
            );
        }
        _ => unreachable!("validated status"),
    }
    Ok((old, new))
}
fn project(
    repository: &Repository,
    change: &Change,
    old: Option<Entry>,
    new: Option<Entry>,
    budget: &mut Budget,
) -> Result<FileRanges> {
    let Some(new) = new else {
        return Ok(file(change, None, RangeScope::Deleted, Vec::new()));
    };
    let new_blob = budget.blob(repository, new.id)?;
    if old.is_some_and(|old| old.id == new.id) {
        return Ok(file(
            change,
            Some(new.id),
            RangeScope::NoChangedLines,
            Vec::new(),
        ));
    }
    let old_blob = old.map(|old| budget.blob(repository, old.id)).transpose()?;
    let old_bytes = old_blob.as_ref().map(|b| b.content()).unwrap_or(&[]);
    let new_lines = budget.diff_work(old_bytes, new_blob.content())?;
    let ranges = diff(repository, old_blob.as_ref(), &new_blob, new_lines, budget)?;
    let scope = if ranges.is_empty() {
        RangeScope::NoChangedLines
    } else {
        RangeScope::Changed
    };
    Ok(file(change, Some(new.id), scope, ranges))
}
fn file(
    change: &Change,
    blob: Option<Oid>,
    scope: RangeScope,
    ranges: Vec<LineRange>,
) -> FileRanges {
    FileRanges {
        path: change.path.clone(),
        old_path: change.old_path.clone(),
        blob_oid: blob.map(|o| o.to_string()),
        scope,
        ranges,
    }
}
fn stats(bytes: &[u8]) -> (u64, u64) {
    let mut count = 0;
    let mut max = 0;
    let mut width = 0;
    for byte in bytes {
        width += 1;
        if *byte == b'\n' {
            count += 1;
            max = max.max(width);
            width = 0;
        }
    }
    if width > 0 {
        count += 1;
        max = max.max(width);
    }
    (count, max)
}
fn diff(
    repository: &Repository,
    old: Option<&Blob<'_>>,
    new: &Blob<'_>,
    new_lines: u64,
    budget: &mut Budget,
) -> Result<Vec<LineRange>> {
    let mut options = DiffOptions::new();
    options.context_lines(0).interhunk_lines(0).force_text(true);
    let mut ranges = Vec::new();
    let mut failure = None;
    let mut callback = |_: git2::DiffDelta<'_>, hunk: git2::DiffHunk<'_>| {
        let result = (|| -> Result<()> {
            budget.hunks += 1;
            ensure!(budget.hunks <= MAX_HUNKS, "diff hunks exceed 100000");
            let count = hunk.new_lines();
            if count == 0 {
                return Ok(());
            }
            let start = hunk.new_start();
            let end = start.checked_add(count - 1).context("hunk line overflow")?;
            ensure!(
                start > 0 && u64::from(end) <= new_lines,
                "invalid libgit2 hunk coordinates"
            );
            budget.ranges += 1;
            ensure!(budget.ranges <= MAX_HUNKS, "diff ranges exceed 100000");
            ranges.push(LineRange { start, end });
            Ok(())
        })();
        match result {
            Ok(()) => true,
            Err(error) => {
                failure = Some(error);
                false
            }
        }
    };
    let result = repository.diff_blobs(
        old,
        None,
        Some(new),
        None,
        Some(&mut options),
        None,
        None,
        Some(&mut callback),
        None,
    );
    if let Some(error) = failure {
        return Err(error);
    }
    result.context("native blob diff failed")?;
    ranges.sort_unstable_by_key(|r| r.start);
    let mut merged: Vec<LineRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(last) = merged.last_mut() {
            let touching = last.end.checked_add(1).context("range merge overflow")?;
            if range.start <= touching {
                last.end = last.end.max(range.end);
                continue;
            }
        }
        merged.push(range);
    }
    Ok(merged)
}
#[cfg(test)]
#[path = "lines_tests.rs"]
mod tests;

fn encoded_size(value: &impl Serialize, limit: usize) -> Result<usize> {
    struct Counter {
        used: usize,
        limit: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.used) {
                return Err(std::io::Error::other("range JSON exceeds 16MiB"));
            }
            self.used += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { used: 0, limit };
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.used)
}
