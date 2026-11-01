//! Version 2 snapshot: fixed header, aligned normalized f32 matrix, u64 node
//! offsets, then packed u32 adjacency lists. All fields are little-endian.
//! Hashes detect damage; they are not authentication against the trusted owner.
use super::{
    Builder, Config, Graph, Query, QueryWorkspace, SearchReport, Space, MAX_LEVEL, MAX_ROWS,
    MAX_VECTOR_BYTES,
};
use anyhow::{bail, Context, Result};
use memmap2::Mmap;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions, Permissions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

const HEADER: usize = 256;
const MAGIC: &[u8; 8] = b"FORGEV02";
const MAX_FILE: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Debug)]
struct Layout {
    space: Space,
    config: Config,
    rows: usize,
    entry: usize,
    top: usize,
    table: usize,
    graph: usize,
    length: usize,
    source_sha256: [u8; 32],
}
impl Layout {
    fn from_builder(builder: &Builder, source_sha256: [u8; 32]) -> Result<(Self, Vec<u64>)> {
        if source_sha256 == [0; 32] {
            bail!("snapshot requires a source-set SHA-256");
        }
        let table = HEADER
            .checked_add(builder.vectors.len() * 4)
            .and_then(|n| n.checked_add(7))
            .context("snapshot matrix offset overflow")?
            & !7;
        let graph = table
            .checked_add((builder.len() + 1) * 8)
            .context("snapshot graph offset overflow")?;
        let mut offsets = Vec::with_capacity(builder.len() + 1);
        let mut end = graph;
        for layers in &builder.links {
            offsets.push(end as u64);
            let node_bytes = 4 + layers
                .iter()
                .map(|links| 4 + links.len() * 4)
                .sum::<usize>();
            end = end
                .checked_add(node_bytes)
                .context("snapshot length overflow")?;
        }
        offsets.push(end as u64);
        if end as u64 > MAX_FILE {
            bail!("snapshot exceeds 8 GiB");
        }
        Ok((
            Self {
                space: builder.space.clone(),
                config: builder.config,
                rows: builder.len(),
                entry: builder.entry,
                top: builder.top,
                table,
                graph,
                length: end,
                source_sha256,
            },
            offsets,
        ))
    }
    fn header(&self, payload: [u8; 32]) -> [u8; HEADER] {
        let mut bytes = [0u8; HEADER];
        bytes[..8].copy_from_slice(MAGIC);
        for (at, value) in [
            (8, 2),
            (12, HEADER),
            (16, self.space.dimension),
            (20, self.rows),
            (24, self.config.neighbors),
            (28, self.config.ef_construction),
            (32, self.entry),
            (36, self.top),
        ] {
            bytes[at..at + 4].copy_from_slice(&(value as u32).to_le_bytes());
        }
        for (at, value) in [
            (40, HEADER),
            (48, self.rows * self.space.dimension * 4),
            (56, self.table),
            (64, self.graph),
            (72, self.length),
        ] {
            bytes[at..at + 8].copy_from_slice(&(value as u64).to_le_bytes());
        }
        bytes[80..112].copy_from_slice(&self.space.model_sha256);
        bytes[112..144].copy_from_slice(&self.source_sha256);
        bytes[144..176].copy_from_slice(&payload);
        bytes[208..216].copy_from_slice(&self.config.seed.to_le_bytes());
        let hash = Sha256::digest(bytes);
        bytes[176..208].copy_from_slice(&hash);
        bytes
    }
}

impl Builder {
    /// Write a new, read-only inode. Never overwrites an existing snapshot.
    /// Publish its parent generation atomically only after all index files sync.
    pub fn write(&self, path: &Path, source_sha256: [u8; 32]) -> Result<()> {
        let (layout, offsets) = Layout::from_builder(self, source_sha256)?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        {
            let mut writer = BufWriter::with_capacity(64 * 1024, &mut file);
            writer.write_all(&[0u8; HEADER])?;
            write_vectors(&mut writer, &self.vectors)?;
            let padding = layout.table - HEADER - self.vectors.len() * 4;
            writer.write_all(&[0u8; 8][..padding])?;
            for offset in offsets {
                writer.write_all(&offset.to_le_bytes())?;
            }
            for layers in &self.links {
                writer.write_all(&(layers.len() as u32).to_le_bytes())?;
                for links in layers {
                    writer.write_all(&(links.len() as u32).to_le_bytes())?;
                    for link in links {
                        writer.write_all(&link.to_le_bytes())?;
                    }
                }
            }
            writer.flush()?;
        }
        file.seek(SeekFrom::Start(HEADER as u64))?;
        let mut hash = Sha256::new();
        let mut block = [0u8; 64 * 1024];
        loop {
            let n = file.read(&mut block)?;
            if n == 0 {
                break;
            }
            hash.update(&block[..n]);
        }
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&layout.header(hash.finalize().into()))?;
        file.set_permissions(Permissions::from_mode(0o400))?;
        file.sync_all()?;
        File::open(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all()?;
        Ok(())
    }
}
fn write_vectors(writer: &mut impl Write, vectors: &[f32]) -> Result<()> {
    let mut bytes = [0u8; 16 * 1024];
    for chunk in vectors.chunks(bytes.len() / 4) {
        for (output, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(chunk) {
            output.copy_from_slice(&value.to_le_bytes());
        }
        writer.write_all(&bytes[..chunk.len() * 4])?;
    }
    Ok(())
}

/// Read-only map of an immutable inode. The repository owner is trusted not to
/// chmod/truncate mapped files. Replacement/unlink of a generation is safe and
/// does not change existing readers; in-place modification is unsupported.
pub struct Snapshot {
    map: Mmap,
    layout: Layout,
}
impl Snapshot {
    pub fn open(path: &Path) -> Result<Self> {
        if cfg!(not(target_endian = "little")) {
            bail!("mapped vector snapshots require a little-endian host");
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.len() < HEADER as u64
            || metadata.len() > MAX_FILE
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
            || metadata.mode() & 0o222 != 0
        {
            bail!("snapshot must be a bounded read-only regular inode owned by this user, without hard links");
        }
        // File ownership, mode and publication contract exclude mutation by
        // untrusted users. The map pins this inode across atomic replacements.
        let map = unsafe { memmap2::MmapOptions::new().map(&file)? };
        let layout = parse_header(&map)?;
        let result = Self { map, layout };
        result.validate_graph()?;
        result.validate_vectors()?;
        Ok(result)
    }
    pub fn len(&self) -> usize {
        self.layout.rows
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn space(&self) -> &Space {
        &self.layout.space
    }
    pub fn config(&self) -> Config {
        self.layout.config
    }
    pub fn source_sha256(&self) -> [u8; 32] {
        self.layout.source_sha256
    }
    pub fn mapped_bytes(&self) -> usize {
        self.map.len()
    }
    pub fn search<'a>(
        &self,
        query: &Query,
        k: usize,
        ef: usize,
        scratch: &'a mut QueryWorkspace,
    ) -> Result<SearchReport<'a>> {
        if &query.space != self.space() {
            bail!("query embedding model or dimension mismatch");
        }
        scratch.run(self, &query.vector, k, ef, false)
    }
    pub fn exact<'a>(
        &self,
        query: &Query,
        k: usize,
        scratch: &'a mut QueryWorkspace,
    ) -> Result<SearchReport<'a>> {
        if &query.space != self.space() {
            bail!("query embedding model or dimension mismatch");
        }
        scratch.run(self, &query.vector, k, k, true)
    }
    pub fn score(&self, query: &Query, row: usize) -> Result<f64> {
        if &query.space != self.space() || row >= self.len() {
            bail!("score requires matching model, dimension, and a valid row");
        }
        Ok(super::distance::dot(&query.vector, self.vector(row)))
    }
    fn node_bounds(&self, row: usize) -> (usize, usize) {
        let start = self.layout.table + row * 8;
        (
            read64(&self.map, start) as usize,
            read64(&self.map, start + 8) as usize,
        )
    }
    fn words(&self, start: usize, end: usize) -> &[u32] {
        // Bounds and four-byte alignment validated at open, little-endian host.
        // All bit patterns are valid u32 and the immutable map outlives the slice.
        unsafe {
            std::slice::from_raw_parts(self.map.as_ptr().add(start).cast(), (end - start) / 4)
        }
    }
    fn validate_vectors(&self) -> Result<()> {
        for row in 0..self.len() {
            let norm: f64 = self.vector(row).iter().map(|&v| f64::from(v).powi(2)).sum();
            if !norm.is_finite() || (norm - 1.0).abs() > 1e-5 {
                bail!("snapshot vector {row} is not finite and normalized");
            }
        }
        Ok(())
    }
    fn validate_graph(&self) -> Result<()> {
        if self.is_empty() {
            if read64(&self.map, self.layout.table) != self.map.len() as u64 {
                bail!("invalid empty snapshot graph offset");
            }
            return Ok(());
        }
        let mut previous = self.layout.graph;
        let mut highest = 0;
        for row in 0..self.len() {
            let table = self.layout.table + row * 8;
            if read64(&self.map, table) > self.map.len() as u64
                || read64(&self.map, table + 8) > self.map.len() as u64
            {
                bail!("snapshot node offset exceeds file length");
            }
            let (start, end) = self.node_bounds(row);
            if start != previous || end <= start || end > self.map.len() || end % 4 != 0 {
                bail!("invalid snapshot node offsets at row {row}");
            }
            let words = self.words(start, end);
            let levels = words[0] as usize;
            if !(1..=MAX_LEVEL + 1).contains(&levels) {
                bail!("invalid snapshot node level");
            }
            highest = highest.max(levels - 1);
            self.validate_node(row, words, levels)?;
            previous = end;
        }
        if previous != self.map.len()
            || highest != self.layout.top
            || self.words(
                self.node_bounds(self.layout.entry).0,
                self.node_bounds(self.layout.entry).1,
            )[0] as usize
                != highest + 1
        {
            bail!("snapshot graph entry, length, or top level is inconsistent");
        }
        // Second pass: every edge must point to a node present on the same layer.
        for row in 0..self.len() {
            let (start, end) = self.node_bounds(row);
            for layer in 0..self.words(start, end)[0] as usize {
                for &neighbor in self.neighbors(row, layer) {
                    let (at, to) = self.node_bounds(neighbor as usize);
                    if self.words(at, to)[0] as usize <= layer {
                        bail!("snapshot edge crosses invalid layer");
                    }
                }
            }
        }
        Ok(())
    }
    fn validate_node(&self, row: usize, words: &[u32], levels: usize) -> Result<()> {
        let mut cursor = 1;
        for layer in 0..levels {
            let count = *words.get(cursor).context("truncated graph degree")? as usize;
            let max = self.layout.config.neighbors * if layer == 0 { 2 } else { 1 };
            if count > max {
                bail!("snapshot graph degree exceeds bound");
            }
            cursor += 1;
            let links = words
                .get(cursor..cursor + count)
                .context("truncated graph links")?;
            for (i, &target) in links.iter().enumerate() {
                if target as usize >= self.len()
                    || target as usize == row
                    || links[..i].contains(&target)
                {
                    bail!("invalid or duplicate snapshot graph edge");
                }
            }
            cursor += count;
        }
        if cursor != words.len() {
            bail!("snapshot node has trailing words");
        }
        Ok(())
    }
}

impl Graph for Snapshot {
    fn len(&self) -> usize {
        self.len()
    }
    fn vector(&self, row: usize) -> &[f32] {
        let start = HEADER + row * self.layout.space.dimension * 4;
        // Header bounds prove the matrix fits the mapping; HEADER is aligned.
        // Every f32 bit pattern is valid, and normalization was checked at open.
        unsafe {
            std::slice::from_raw_parts(
                self.map.as_ptr().add(start).cast(),
                self.layout.space.dimension,
            )
        }
    }
    fn neighbors(&self, row: usize, layer: usize) -> &[u32] {
        let (start, end) = self.node_bounds(row);
        let words = self.words(start, end);
        let mut cursor = 1;
        for _ in 0..layer {
            cursor += 1 + words[cursor] as usize;
        }
        &words[cursor + 1..cursor + 1 + words[cursor] as usize]
    }
    fn entry(&self) -> usize {
        self.layout.entry
    }
    fn top(&self) -> usize {
        self.layout.top
    }
}

fn read32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("bounded field"))
}
fn read64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("bounded field"))
}
fn parse_header(bytes: &[u8]) -> Result<Layout> {
    let mut header: [u8; HEADER] = bytes[..HEADER].try_into()?;
    let expected = header[176..208].to_vec();
    header[176..208].fill(0);
    if Sha256::digest(header).as_slice() != expected
        || &header[..8] != MAGIC
        || read32(&header, 8) != 2
        || read32(&header, 12) as usize != HEADER
        || header[216..].iter().any(|&v| v != 0)
    {
        bail!("invalid vector snapshot header or checksum");
    }
    let layout = Layout {
        space: Space {
            dimension: read32(&header, 16) as usize,
            model_sha256: header[80..112].try_into()?,
        },
        config: Config {
            neighbors: read32(&header, 24) as usize,
            ef_construction: read32(&header, 28) as usize,
            seed: read64(&header, 208),
        },
        rows: read32(&header, 20) as usize,
        entry: read32(&header, 32) as usize,
        top: read32(&header, 36) as usize,
        table: usize::try_from(read64(&header, 56))?,
        graph: usize::try_from(read64(&header, 64))?,
        length: usize::try_from(read64(&header, 72))?,
        source_sha256: header[112..144].try_into()?,
    };
    layout.space.validate()?;
    layout.config.validate()?;
    let vector_bytes = layout
        .rows
        .checked_mul(layout.space.dimension)
        .and_then(|n| n.checked_mul(4))
        .context("vector size overflow")?;
    let graph_bounds_invalid = if layout.rows == 0 {
        layout.entry != 0 || layout.top != 0 || layout.graph != bytes.len()
    } else {
        layout.entry >= layout.rows || layout.graph >= bytes.len()
    };
    let vector_end = HEADER
        .checked_add(vector_bytes)
        .context("snapshot matrix end overflow")?;
    let expected_table = vector_end
        .checked_add(7)
        .context("snapshot alignment overflow")?
        & !7;
    let expected_graph = expected_table
        .checked_add((layout.rows + 1) * 8)
        .context("snapshot graph start overflow")?;
    if layout.rows > MAX_ROWS
        || vector_bytes as u64 > MAX_VECTOR_BYTES
        || graph_bounds_invalid
        || layout.top > MAX_LEVEL
        || layout.length != bytes.len()
        || layout.source_sha256 == [0; 32]
        || read64(&header, 40) != HEADER as u64
        || read64(&header, 48) != vector_bytes as u64
        || layout.table != expected_table
        || layout.graph != expected_graph
    {
        bail!("vector snapshot shape or bounds are inconsistent");
    }
    if bytes[HEADER + vector_bytes..layout.table]
        .iter()
        .any(|&v| v != 0)
        || Sha256::digest(&bytes[HEADER..]).as_slice() != &header[144..176]
    {
        bail!("vector snapshot payload checksum mismatch");
    }
    Ok(layout)
}
