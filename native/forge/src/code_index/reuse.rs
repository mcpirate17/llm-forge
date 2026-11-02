//! Verified row reuse from a leased, checksummed immutable generation.
use super::{search::ReuseRow, Reader, Store};
use anyhow::{ensure, Result};
use conductor_native::vector_index::Space;
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
};
struct FileRows {
    hash: String,
    spans: BTreeMap<(usize, usize), usize>,
}
pub(super) struct Reuse {
    reader: Reader,
    files: HashMap<String, FileRows>,
}
impl Reuse {
    pub fn open(
        store: &Store,
        source: &Path,
        space: &Space,
        force: bool,
        checkpoint: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Option<Self>> {
        match std::fs::symlink_metadata(store.root().join("current")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(_) => (),
        }
        checkpoint()?;
        let reader = Reader::open(store)?;
        checkpoint()?;
        ensure!(
            reader.manifest().source_root == source.canonicalize()?,
            "index belongs to a different source root"
        );
        if force || reader.manifest().refresh_origin.is_none() || &reader.manifest().space != space
        {
            return Ok(None);
        }
        let mut files: HashMap<String, FileRows> = HashMap::new();
        for ReuseRow {
            index: row,
            path,
            source_sha256: hash,
            start,
            end,
        } in reader.reuse_metadata(checkpoint)?
        {
            super::validate_hash(&hash)?;
            crate::harness::policy::source_path(Path::new(&path))?;
            ensure!(
                path.len() <= 4096
                    && path
                        .split('/')
                        .all(|part| !part.is_empty() && part != "." && part != ".."),
                "invalid reusable source path"
            );
            ensure!(
                end > start && end - start <= super::MAX_CHUNK_BYTES,
                "invalid reusable source span"
            );
            let file = files.entry(path).or_insert_with(|| FileRows {
                hash: hash.clone(),
                spans: BTreeMap::new(),
            });
            ensure!(
                file.hash == hash,
                "inconsistent source identity within reusable file"
            );
            ensure!(
                file.spans.insert((start, end), row).is_none(),
                "duplicate reusable source span"
            );
        }
        Ok(Some(Self { reader, files }))
    }
    pub fn get(
        &self,
        path: &str,
        hash: &str,
        start: usize,
        end: usize,
        text: &str,
    ) -> Result<Option<Vec<f32>>> {
        let Some(file) = self.files.get(path) else {
            return Ok(None);
        };
        if file.hash != hash {
            return Ok(None);
        }
        let Some(&row) = file.spans.get(&(start, end)) else {
            return Ok(None);
        };
        self.reader.reuse_vector(row, text).map(Some)
    }
}
