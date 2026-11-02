use super::{
    distance, Config, Graph, Query, QueryWorkspace, SearchReport, Space, MAX_LEVEL, MAX_ROWS,
    MAX_VECTOR_BYTES,
};
use crate::top_k::Ranked;
use anyhow::{bail, Result};

pub struct Builder {
    pub(crate) space: Space,
    pub(crate) config: Config,
    pub(crate) vectors: Vec<f32>,
    pub(crate) links: Vec<Vec<Vec<u32>>>,
    pub(crate) entry: usize,
    pub(crate) top: usize,
}
impl Builder {
    /// Consume a contiguous embedding matrix, normalizing it in place. The graph
    /// is deterministic for the same ordered matrix, seed, and scoring backend.
    pub fn new(space: Space, vectors: Vec<f32>, config: Config) -> Result<Self> {
        Self::new_checked(space, vectors, config, || Ok(()))
    }
    /// Check cancellation/resource policy between every normalization and graph
    /// insertion. Failure drops the unpublished builder, never a mapped snapshot.
    pub fn new_checked(
        space: Space,
        vectors: Vec<f32>,
        config: Config,
        checkpoint: impl FnMut() -> Result<()>,
    ) -> Result<Self> {
        Self::new_inner(space, vectors, config, None, checkpoint)
    }
    /// Preserve bit-identical previously validated unit vectors; normalize new rows.
    pub fn new_reusing_checked(
        space: Space,
        vectors: Vec<f32>,
        config: Config,
        reused: &[bool],
        checkpoint: impl FnMut() -> Result<()>,
    ) -> Result<Self> {
        Self::new_inner(space, vectors, config, Some(reused), checkpoint)
    }
    fn new_inner(
        space: Space,
        mut vectors: Vec<f32>,
        config: Config,
        reused: Option<&[bool]>,
        mut checkpoint: impl FnMut() -> Result<()>,
    ) -> Result<Self> {
        space.validate()?;
        config.validate()?;
        let rows = vectors.len() / space.dimension;
        if rows > MAX_ROWS
            || !vectors.len().is_multiple_of(space.dimension)
            || vectors
                .len()
                .checked_mul(4)
                .is_none_or(|n| n as u64 > MAX_VECTOR_BYTES)
        {
            bail!("embedding matrix exceeds shape or byte limits");
        }
        if reused.is_some_and(|flags| flags.len() != rows) {
            bail!("reuse mask does not match matrix rows");
        }
        for (index, row) in vectors.chunks_exact_mut(space.dimension).enumerate() {
            checkpoint()?;
            if reused.is_some_and(|flags| flags[index]) {
                let norm: f64 = row.iter().map(|&v| f64::from(v).powi(2)).sum();
                if !norm.is_finite() || (norm - 1.0).abs() > 1e-5 {
                    bail!("reused vector is not finite and normalized");
                }
            } else {
                distance::normalize(row)?;
            }
        }
        let mut result = Self {
            space,
            config,
            vectors,
            links: Vec::with_capacity(rows),
            entry: 0,
            top: 0,
        };
        let mut workspace = QueryWorkspace::new(rows, config.ef_construction)?;
        for row in 0..rows {
            checkpoint()?;
            result.insert(row, &mut workspace);
        }
        Ok(result)
    }
    pub fn space(&self) -> &Space {
        &self.space
    }
    pub fn len(&self) -> usize {
        self.links.len()
    }
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }
    pub fn search<'a>(
        &self,
        query: &Query,
        k: usize,
        ef: usize,
        scratch: &'a mut QueryWorkspace,
    ) -> Result<SearchReport<'a>> {
        if query.space != self.space {
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
        if query.space != self.space {
            bail!("query embedding model or dimension mismatch");
        }
        scratch.run(self, &query.vector, k, k, true)
    }
    fn insert(&mut self, row: usize, scratch: &mut QueryWorkspace) {
        let level = level(self.config.seed, row, self.config.neighbors);
        self.links.push(vec![Vec::new(); level + 1]);
        if row == 0 {
            self.top = level;
            return;
        }
        let query = self.vector(row);
        let mut entry = self.entry;
        for layer in (level + 1..=self.top).rev() {
            entry = scratch.greedy(self, query, entry, layer);
        }
        let mut starts = vec![entry];
        for layer in (0..=level.min(self.top)).rev() {
            scratch.layer(
                self,
                self.vector(row),
                &starts,
                self.config.ef_construction,
                layer,
            );
            starts.clear();
            starts.extend(scratch.ranked.iter().map(|r| r.index));
            let selected = self.diverse(&scratch.ranked, self.config.neighbors);
            self.links[row][layer] = selected.clone();
            for neighbor in selected {
                let neighbor = neighbor as usize;
                self.links[neighbor][layer].push(row as u32);
                let maximum = self.config.neighbors * if layer == 0 { 2 } else { 1 };
                if self.links[neighbor][layer].len() > maximum {
                    self.prune(neighbor, layer, maximum);
                }
            }
        }
        if level > self.top {
            self.top = level;
            self.entry = row;
        }
    }
    fn prune(&mut self, row: usize, layer: usize, maximum: usize) {
        let query = self.vector(row);
        let mut candidates: Vec<_> = self.links[row][layer]
            .iter()
            .map(|&index| Ranked {
                score: distance::dot(query, self.vector(index as usize)),
                index: index as usize,
            })
            .collect();
        candidates.sort_unstable();
        self.links[row][layer] = self.diverse(&candidates, maximum);
    }
    /// Relative-neighborhood heuristic (paper algorithm 4, extendCandidates=false,
    /// keepPrunedConnections=true). Candidates are already best-first and unique.
    fn diverse(&self, candidates: &[Ranked], maximum: usize) -> Vec<u32> {
        let mut chosen = Vec::with_capacity(maximum);
        for candidate in candidates {
            if chosen.len() == maximum {
                break;
            }
            let vector = self.vector(candidate.index);
            if chosen
                .iter()
                .all(|&other| distance::dot(vector, self.vector(other as usize)) <= candidate.score)
            {
                chosen.push(candidate.index as u32);
            }
        }
        if chosen.len() < maximum {
            for candidate in candidates {
                if chosen.len() == maximum {
                    break;
                }
                if !chosen.contains(&(candidate.index as u32)) {
                    chosen.push(candidate.index as u32);
                }
            }
        }
        chosen
    }
}
impl Graph for Builder {
    fn len(&self) -> usize {
        self.len()
    }
    fn vector(&self, row: usize) -> &[f32] {
        let start = row * self.space.dimension;
        &self.vectors[start..start + self.space.dimension]
    }
    fn neighbors(&self, row: usize, layer: usize) -> &[u32] {
        &self.links[row][layer]
    }
    fn entry(&self) -> usize {
        self.entry
    }
    fn top(&self) -> usize {
        self.top
    }
}

fn level(seed: u64, row: usize, neighbors: usize) -> usize {
    // SplitMix64 gives reproducible independent bits without an RNG dependency.
    let mut bits = seed.wrapping_add((row as u64).wrapping_mul(0x9e3779b97f4a7c15));
    bits = (bits ^ (bits >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    bits = (bits ^ (bits >> 27)).wrapping_mul(0x94d049bb133111eb);
    bits ^= bits >> 31;
    if bits == 0 {
        return MAX_LEVEL;
    }
    let mut level = 0;
    // P(level >= l) = (1/M)^l, the paper's mL=1/ln(M).
    while level < MAX_LEVEL && bits < u64::MAX / neighbors as u64 {
        level += 1;
        bits = bits.saturating_mul(neighbors as u64);
    }
    level
}
