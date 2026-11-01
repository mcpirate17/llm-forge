use super::{distance, Graph, Space, MAX_EF, MAX_ROWS};
use crate::top_k::Ranked;
use anyhow::{bail, Result};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// An embedding normalized once for reuse across snapshot queries.
pub struct Query {
    pub(crate) space: Space,
    pub(crate) vector: Vec<f32>,
}
impl Query {
    pub fn new(space: Space, mut vector: Vec<f32>) -> Result<Self> {
        space.validate()?;
        if vector.len() != space.dimension {
            bail!("query embedding dimension mismatch");
        }
        distance::normalize(&mut vector)?;
        Ok(Self { space, vector })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Hit {
    pub row: u32,
    pub cosine: f64,
}

#[derive(Debug, Serialize)]
pub struct SearchReport<'a> {
    pub hits: &'a [Hit],
    pub distance_evaluations: usize,
}

/// Reusable query scratch. Allocation is O(rows + ef), never O(rows * dimension).
/// The frontier reserves rows entries so even adversarial graphs cannot trigger
/// allocations in a query. Use one workspace per concurrent reader.
pub struct QueryWorkspace {
    marks: Vec<u32>,
    epoch: u32,
    frontier: BinaryHeap<Reverse<Ranked>>,
    best: BinaryHeap<Ranked>,
    pub(crate) ranked: Vec<Ranked>,
    hits: Vec<Hit>,
    ef_capacity: usize,
    evaluations: usize,
}
impl QueryWorkspace {
    pub fn new(rows: usize, ef_capacity: usize) -> Result<Self> {
        if rows > MAX_ROWS || !(1..=MAX_EF).contains(&ef_capacity) {
            bail!("query workspace exceeds row or ef limits");
        }
        let mut marks = Vec::new();
        marks.try_reserve_exact(rows)?;
        marks.resize(rows, 0);
        let mut frontier = BinaryHeap::new();
        frontier.try_reserve_exact(rows)?;
        let mut best = BinaryHeap::new();
        best.try_reserve_exact(ef_capacity)?;
        let mut ranked = Vec::new();
        ranked.try_reserve_exact(ef_capacity)?;
        let mut hits = Vec::new();
        hits.try_reserve_exact(ef_capacity)?;
        Ok(Self {
            marks,
            epoch: 0,
            frontier,
            best,
            ranked,
            hits,
            ef_capacity,
            evaluations: 0,
        })
    }
    pub fn reserved_bytes(&self) -> usize {
        self.marks.capacity() * size_of::<u32>()
            + (self.frontier.capacity() + self.best.capacity() + self.ranked.capacity())
                * size_of::<Ranked>()
            + self.hits.capacity() * size_of::<Hit>()
    }
    pub(crate) fn validate(&self, rows: usize, k: usize, ef: usize) -> Result<()> {
        if self.marks.len() < rows || k == 0 || k > ef || ef > self.ef_capacity {
            bail!("query requires rows within workspace and 1 <= k <= ef <= workspace capacity");
        }
        Ok(())
    }
    fn begin_layer(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.marks.fill(0);
            self.epoch = 1;
        }
        self.frontier.clear();
        self.best.clear();
        self.ranked.clear();
    }
    fn score(&mut self, graph: &impl Graph, query: &[f32], index: usize) -> Ranked {
        self.evaluations += 1;
        Ranked {
            score: distance::dot(query, graph.vector(index)),
            index,
        }
    }
    fn retain(&mut self, rank: Ranked, ef: usize) -> bool {
        if self.best.len() == ef {
            if self.best.peek().is_some_and(|worst| rank >= *worst) {
                return false;
            }
            self.best.pop();
        }
        self.best.push(rank);
        true
    }
    pub(crate) fn layer(
        &mut self,
        graph: &impl Graph,
        query: &[f32],
        starts: &[usize],
        ef: usize,
        layer: usize,
    ) {
        self.begin_layer();
        for &index in starts {
            if self.marks[index] == self.epoch {
                continue;
            }
            self.marks[index] = self.epoch;
            let rank = self.score(graph, query, index);
            if self.retain(rank, ef) {
                self.frontier.push(Reverse(rank));
            }
        }
        while let Some(Reverse(candidate)) = self.frontier.pop() {
            if self.best.len() == ef && self.best.peek().is_some_and(|worst| candidate > *worst) {
                break;
            }
            for &neighbor in graph.neighbors(candidate.index, layer) {
                let index = neighbor as usize;
                if self.marks[index] == self.epoch {
                    continue;
                }
                self.marks[index] = self.epoch;
                let rank = self.score(graph, query, index);
                if self.retain(rank, ef) {
                    self.frontier.push(Reverse(rank));
                }
            }
        }
        self.ranked.extend(self.best.drain());
        self.ranked.sort_unstable();
    }
    pub(crate) fn greedy(
        &mut self,
        graph: &impl Graph,
        query: &[f32],
        mut index: usize,
        layer: usize,
    ) -> usize {
        let mut current = self.score(graph, query, index);
        loop {
            let before = index;
            for &neighbor in graph.neighbors(before, layer) {
                let candidate = self.score(graph, query, neighbor as usize);
                if candidate < current {
                    current = candidate;
                    index = current.index;
                }
            }
            if index == before {
                return index;
            }
        }
    }
    pub(crate) fn run<'a>(
        &'a mut self,
        graph: &impl Graph,
        query: &[f32],
        k: usize,
        ef: usize,
        exact: bool,
    ) -> Result<SearchReport<'a>> {
        self.validate(graph.len(), k, ef)?;
        self.evaluations = 0;
        if graph.len() == 0 {
            self.begin_layer();
            self.hits.clear();
            return Ok(SearchReport {
                hits: &self.hits,
                distance_evaluations: 0,
            });
        }
        if exact {
            self.begin_layer();
            for index in 0..graph.len() {
                let rank = self.score(graph, query, index);
                self.retain(rank, k);
            }
            self.ranked.extend(self.best.drain());
            self.ranked.sort_unstable();
        } else {
            let mut entry = graph.entry();
            for layer in (1..=graph.top()).rev() {
                entry = self.greedy(graph, query, entry, layer);
            }
            self.layer(graph, query, &[entry], ef, 0);
        }
        self.hits.clear();
        self.hits.extend(self.ranked.iter().take(k).map(|r| Hit {
            row: r.index as u32,
            cosine: r.score,
        }));
        Ok(SearchReport {
            hits: &self.hits,
            distance_evaluations: self.evaluations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generation_rollover_clears_old_visits() {
        let mut scratch = QueryWorkspace::new(3, 2).unwrap();
        scratch.marks.fill(1);
        scratch.epoch = u32::MAX;
        scratch.begin_layer();
        assert_eq!(scratch.epoch, 1);
        assert_eq!(scratch.marks, [0, 0, 0]);
        assert!(scratch.validate(4, 1, 1).is_err());
        assert!(scratch.validate(3, 2, 1).is_err());
        assert!(scratch.validate(3, 1, 3).is_err());
    }
}
