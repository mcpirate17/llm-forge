//! Bounded streaming row publication and offset-only incremental reuse.
use super::MAX_INDEX_BYTES;
use crate::memory_store::Pinned;
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub(super) struct Writer {
    output: BufWriter<File>,
    pub count: usize,
    bytes: usize,
}
impl Writer {
    pub fn new(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        Ok(Self {
            output: BufWriter::new(file),
            count: 0,
            bytes: 0,
        })
    }
    pub fn raw(&mut self, line: &[u8]) -> Result<()> {
        ensure!(line.len() <= 1024 * 1024, "memory row exceeds 1 MiB");
        let newline = usize::from(!line.ends_with(b"\n"));
        ensure!(
            self.count < 1_000_000 && self.bytes + line.len() + newline <= MAX_INDEX_BYTES,
            "memory generation exceeds row or byte limit"
        );
        self.output.write_all(line)?;
        if newline != 0 {
            self.output.write_all(b"\n")?;
        }
        self.bytes += line.len() + newline;
        self.count += 1;
        Ok(())
    }
    pub fn value(&mut self, value: &Value) -> Result<()> {
        self.raw(&serde_json::to_vec(value)?)
    }
    pub fn finish(mut self) -> Result<usize> {
        self.output.flush()?;
        self.output.get_ref().sync_all()?;
        Ok(self.count)
    }
}

#[derive(Default)]
struct OldFile {
    hash: String,
    recipe: String,
    offsets: Vec<(u64, usize)>,
}
pub(super) struct Previous {
    file: File,
    files: BTreeMap<(String, String), OldFile>,
    pub preserved: usize,
    pub dropped: usize,
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("memory row lacks {key}"))
}

fn row_in_root(source: &str, path: &str, wanted_source: &str, root: &Path) -> bool {
    source == wanted_source && Path::new(path).starts_with(root)
}

/// Read-only preflight against the pinned generation; never creates staging files.
pub(super) fn validate_missing_roots(pin: &Pinned, missing: &[(String, PathBuf)]) -> Result<()> {
    if missing.is_empty() {
        return Ok(());
    }
    let mut roots: BTreeMap<&str, BTreeSet<PathBuf>> = BTreeMap::new();
    for (source, root) in missing {
        roots.entry(source).or_default().insert(root.clone());
    }
    pin.reader.verify_source_hash()?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&pin.source)?;
    let mut input = BufReader::new(file);
    let mut bytes = 0_usize;
    let mut count = 0_usize;
    let mut line = Vec::new();
    let mut hash = Sha256::new();
    let mut conflict = None;
    while conductor_native::bounded_io::read_line(&mut input, &mut line, 1024 * 1024)? != 0 {
        bytes = bytes
            .checked_add(line.len())
            .context("prior memory byte count overflow")?;
        ensure!(
            bytes <= MAX_INDEX_BYTES,
            "prior memory generation exceeds byte bound"
        );
        hash.update(&line);
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        count += 1;
        ensure!(
            count <= 1_000_000,
            "prior memory generation exceeds row bound"
        );
        let row: Value = serde_json::from_slice(&line)?;
        let source = string(&row, "source")?;
        let path = string(&row, "path")?;
        if let Some(roots) = roots.get(source) {
            for ancestor in Path::new(path).ancestors() {
                if let Some(root) = roots.get(ancestor) {
                    if row_in_root(source, path, source, root) && conflict.is_none() {
                        conflict = Some((source.to_owned(), root.clone()));
                    }
                }
            }
        }
    }
    ensure!(
        format!("{:x}", hash.finalize()) == pin.manifest.source_sha256,
        "prior memory source changed during missing-root preflight"
    );
    pin.reader.verify_source_hash()?;
    if let Some((source, root)) = conflict {
        anyhow::bail!(
            "source {source} root {} is missing; refusing implicit removal of prior rows",
            root.display()
        );
    }
    Ok(())
}
impl Previous {
    pub fn load(
        pin: &Pinned,
        available: &BTreeSet<String>,
        selected: &BTreeSet<String>,
        output: &mut Writer,
    ) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&pin.source)?;
        let mut reader = BufReader::new(&file);
        let mut files: BTreeMap<(String, String), OldFile> = BTreeMap::new();
        let mut line = Vec::new();
        let (mut offset, mut preserved, mut dropped) = (0_u64, 0, 0);
        while conductor_native::bounded_io::read_line(&mut reader, &mut line, 1024 * 1024)? != 0 {
            let start = offset;
            offset += line.len() as u64;
            ensure!(
                offset <= MAX_INDEX_BYTES as u64,
                "prior memory generation exceeds byte bound"
            );
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let row: Value = serde_json::from_slice(&line)?;
            let source = string(&row, "source")?;
            if !available.contains(source) {
                dropped += 1;
                continue;
            }
            if !selected.contains(source) {
                output.raw(&line)?;
                preserved += 1;
                continue;
            }
            let path = string(&row, "path")?;
            let hash = string(&row, "source_sha256")?;
            let recipe = string(&row, "recipe_sha256")?;
            conductor_native::sha256_hex::parse(hash).map_err(anyhow::Error::msg)?;
            conductor_native::sha256_hex::parse(recipe).map_err(anyhow::Error::msg)?;
            let old = files.entry((source.into(), path.into())).or_default();
            ensure!(
                old.offsets.is_empty() || (old.hash == hash && old.recipe == recipe),
                "inconsistent memory file reuse identity"
            );
            old.hash = hash.into();
            old.recipe = recipe.into();
            old.offsets.push((start, line.len()));
        }
        Ok(Self {
            file,
            files,
            preserved,
            dropped,
        })
    }
    pub fn reuse(
        &self,
        source: &str,
        path: &str,
        hash: &str,
        recipe: &str,
        output: &mut Writer,
    ) -> Result<Option<usize>> {
        let Some(old) = self.files.get(&(source.into(), path.into())) else {
            return Ok(None);
        };
        if old.hash != hash || old.recipe != recipe {
            return Ok(None);
        }
        for (offset, length) in &old.offsets {
            let mut line = vec![0; *length];
            self.file.read_exact_at(&mut line, *offset)?;
            output.raw(&line)?;
        }
        Ok(Some(old.offsets.len()))
    }
    pub fn has_root(&self, source: &str, root: &Path) -> bool {
        self.files
            .keys()
            .any(|(id, path)| row_in_root(id, path, source, root))
    }
    pub fn removed(&self, emitted: &BTreeSet<(String, String)>) -> usize {
        self.dropped
            + self
                .files
                .iter()
                .filter(|(key, _)| !emitted.contains(*key))
                .map(|(_, file)| file.offsets.len())
                .sum::<usize>()
    }
}
