//! Fixed-capacity byte replay with absolute offsets and explicit eviction reports.
use anyhow::{ensure, Result};

pub struct ReplayBuffer {
    bytes: Box<[u8]>,
    begin: u64,
    end: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Chunk {
    pub start: u64,
    pub next: u64,
    pub end: u64,
    pub evicted: bool,
    pub bytes: Vec<u8>,
}

impl ReplayBuffer {
    pub fn new(capacity: usize) -> Result<Self> {
        ensure!(
            (1..=4 * 1024 * 1024).contains(&capacity),
            "replay capacity must be 1..4194304 bytes"
        );
        Ok(Self {
            bytes: vec![0u8; capacity].into_boxed_slice(),
            begin: 0,
            end: 0,
        })
    }

    /// No allocations after construction. Oversized input retains its exact tail.
    pub fn append(&mut self, input: &[u8]) -> Result<()> {
        let end = self
            .end
            .checked_add(input.len() as u64)
            .ok_or_else(|| anyhow::anyhow!("terminal stream offset exhausted"))?;
        let keep = input.len().min(self.bytes.len());
        let input = &input[input.len() - keep..];
        let offset = ((end - keep as u64) % self.bytes.len() as u64) as usize;
        let first = keep.min(self.bytes.len() - offset);
        self.bytes[offset..offset + first].copy_from_slice(&input[..first]);
        self.bytes[..keep - first].copy_from_slice(&input[first..]);
        self.end = end;
        self.begin = end.saturating_sub(self.bytes.len() as u64);
        Ok(())
    }

    pub fn read(&self, cursor: u64, limit: usize) -> Result<Chunk> {
        ensure!(cursor <= self.end, "cursor lies beyond terminal output");
        ensure!(
            (1..=65536).contains(&limit),
            "read limit must be 1..65536 bytes"
        );
        let start = cursor.max(self.begin);
        let length = (self.end - start).min(limit as u64) as usize;
        let offset = (start % self.bytes.len() as u64) as usize;
        let first = length.min(self.bytes.len() - offset);
        let mut bytes = Vec::with_capacity(length);
        bytes.extend_from_slice(&self.bytes[offset..offset + first]);
        bytes.extend_from_slice(&self.bytes[..length - first]);
        Ok(Chunk {
            start,
            next: start + length as u64,
            end: self.end,
            evicted: cursor < self.begin,
            bytes,
        })
    }

    pub fn end(&self) -> u64 {
        self.end
    }
    pub fn retained_start(&self) -> u64 {
        self.begin
    }
}
