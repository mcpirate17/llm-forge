//! Version-2 memory snapshot layout, checked before any mapped indexing.
use anyhow::{ensure, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

pub(super) const HEADER_LEN: usize = 384;
pub(super) const MAX_SOURCE: u64 = 512 * 1024 * 1024;
pub(super) const MAX_FILE: u64 = 2 * 1024 * 1024 * 1024;
pub(super) const MAX_ROWS: usize = 1_000_000;
const MAGIC: &[u8; 8] = b"FORGEM02";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stamp(pub [u64; 7]);
impl Stamp {
    pub fn read(file: &File) -> Result<Self> {
        let meta = file.metadata()?;
        ensure!(
            meta.is_file() && meta.len() <= MAX_SOURCE,
            "memory source must be a regular file at most 512 MiB"
        );
        Ok(Self([
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mtime() as u64,
            meta.mtime_nsec() as u64,
            meta.ctime() as u64,
            meta.ctime_nsec() as u64,
        ]))
    }
    pub fn bytes(&self) -> u64 {
        self.0[2]
    }
}

pub(super) fn source(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    Stamp::read(&file)?;
    Ok(file)
}

#[derive(Debug, Serialize)]
pub struct Metadata {
    pub schema_version: u32,
    pub rows: usize,
    pub dimension: usize,
    pub fingerprint: String,
    pub source_bytes: u64,
    pub mapped_bytes: usize,
    #[serde(with = "crate::sha256_hex")]
    pub source_sha256: [u8; 32],
}

pub(super) struct Header {
    pub dims: usize,
    pub rows: usize,
    pub stamp: Stamp,
    pub source_sha256: [u8; 32],
    pub fingerprint: String,
    pub table: usize,
    pub length: usize,
}

impl Header {
    pub fn metadata(&self) -> Metadata {
        Metadata {
            schema_version: 2,
            rows: self.rows,
            dimension: self.dims,
            fingerprint: self.fingerprint.clone(),
            source_bytes: self.stamp.bytes(),
            mapped_bytes: self.length,
            source_sha256: self.source_sha256,
        }
    }
    pub fn new(
        dims: usize,
        rows: usize,
        stamp: Stamp,
        source_sha256: [u8; 32],
        fingerprint: String,
    ) -> Result<Self> {
        ensure!(
            (1..=8192).contains(&dims) && (1..=MAX_ROWS).contains(&rows),
            "memory dimensions or row count exceed limits"
        );
        ensure!(
            fingerprint.starts_with("sha256:")
                && fingerprint.len() < 128
                && !fingerprint.contains('\0'),
            "invalid memory fingerprint"
        );
        let table = HEADER_LEN
            + rows
                .checked_mul(dims)
                .and_then(|n| n.checked_mul(4))
                .ok_or_else(|| anyhow::anyhow!("matrix overflow"))?;
        let length = table
            .checked_add(rows * 8)
            .ok_or_else(|| anyhow::anyhow!("offset table overflow"))?;
        ensure!(length as u64 <= MAX_FILE, "memory snapshot exceeds 2 GiB");
        Ok(Self {
            dims,
            rows,
            stamp,
            source_sha256,
            fingerprint,
            table,
            length,
        })
    }
    pub fn bytes(&self, payload_hash: [u8; 32]) -> [u8; HEADER_LEN] {
        let mut bytes = [0; HEADER_LEN];
        bytes[..8].copy_from_slice(MAGIC);
        for (at, value) in [(8, 2), (12, HEADER_LEN), (16, self.dims), (20, self.rows)] {
            bytes[at..at + 4].copy_from_slice(&(value as u32).to_le_bytes());
        }
        for (index, value) in self.stamp.0.iter().enumerate() {
            bytes[24 + index * 8..32 + index * 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[80..112].copy_from_slice(&self.source_sha256);
        bytes[112..112 + self.fingerprint.len()].copy_from_slice(self.fingerprint.as_bytes());
        for (at, value) in [
            (240, self.table - HEADER_LEN),
            (248, self.table),
            (256, self.length),
        ] {
            bytes[at..at + 8].copy_from_slice(&(value as u64).to_le_bytes());
        }
        bytes[264..296].copy_from_slice(&payload_hash);
        let hash = Sha256::digest(bytes);
        bytes[296..328].copy_from_slice(&hash);
        bytes
    }
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() >= HEADER_LEN && &bytes[..8] == MAGIC,
            "unsupported memory snapshot; rebuild version 2"
        );
        let mut checked: [u8; HEADER_LEN] = bytes[..HEADER_LEN].try_into()?;
        checked[296..328].fill(0);
        ensure!(
            Sha256::digest(checked).as_slice() == &bytes[296..328],
            "memory snapshot header checksum mismatch"
        );
        ensure!(
            bytes[328..HEADER_LEN].iter().all(|b| *b == 0),
            "nonzero reserved snapshot bytes"
        );
        let u32_at = |at: usize| {
            u32::from_le_bytes(bytes[at..at + 4].try_into().expect("header bounds")) as usize
        };
        let u64_at =
            |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().expect("header bounds"));
        ensure!(
            u32_at(8) == 2 && u32_at(12) == HEADER_LEN,
            "unsupported memory snapshot header"
        );
        let fingerprint = &bytes[112..240];
        let end = fingerprint
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(128);
        ensure!(
            fingerprint[end..].iter().all(|byte| *byte == 0),
            "invalid fingerprint padding"
        );
        let stamp = Stamp(std::array::from_fn(|index| u64_at(24 + index * 8)));
        ensure!(stamp.bytes() <= MAX_SOURCE, "memory source exceeds 512 MiB");
        let header = Self::new(
            u32_at(16),
            u32_at(20),
            stamp,
            bytes[80..112].try_into()?,
            std::str::from_utf8(&fingerprint[..end])?.into(),
        )?;
        ensure!(
            header.length == bytes.len()
                && u64_at(240) == (header.table - HEADER_LEN) as u64
                && u64_at(248) == header.table as u64
                && u64_at(256) == header.length as u64,
            "snapshot layout mismatch"
        );
        ensure!(
            Sha256::digest(&bytes[HEADER_LEN..]).as_slice() == &bytes[264..296],
            "memory snapshot payload checksum mismatch"
        );
        Ok(header)
    }
}
