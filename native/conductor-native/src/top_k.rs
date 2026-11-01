//! Stable bounded selection: memory is O(k), with the worst retained row at the root.
use std::cmp::Ordering;
use std::collections::BinaryHeap;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Ranked {
    pub score: f64,
    pub index: usize,
}
impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Ranked {}
impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| self.index.cmp(&other.index))
    }
}
struct Entry<T> {
    rank: Ranked,
    payload: T,
}
impl<T> PartialEq for Entry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.rank == other.rank
    }
}
impl<T> Eq for Entry<T> {}
impl<T> PartialOrd for Entry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<T> Ord for Entry<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.rank.cmp(&other.rank)
    }
}

pub(crate) struct TopK<T> {
    capacity: usize,
    heap: BinaryHeap<Entry<T>>,
}
impl<T> TopK<T> {
    pub fn new(capacity: usize) -> Result<Self, String> {
        if !(1..=4096).contains(&capacity) {
            return Err("candidate count must be 1..4096".into());
        }
        Ok(Self {
            capacity,
            heap: BinaryHeap::with_capacity(capacity),
        })
    }
    pub fn accepts(&self, rank: Ranked) -> bool {
        self.heap.len() < self.capacity || self.heap.peek().is_some_and(|worst| rank < worst.rank)
    }
    pub fn push(&mut self, rank: Ranked, payload: T) {
        if self.heap.len() < self.capacity {
            self.heap.push(Entry { rank, payload });
        } else if self.accepts(rank) {
            *self.heap.peek_mut().expect("nonempty bounded heap") = Entry { rank, payload };
        }
    }
    pub fn sorted(self) -> Vec<(Ranked, T)> {
        self.heap
            .into_sorted_vec()
            .into_iter()
            .map(|entry| (entry.rank, entry.payload))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_selection_matches_an_independent_full_sort_with_ties() {
        for capacity in [1, 7, 64, 4096] {
            let mut selected = TopK::new(capacity).unwrap();
            let mut all = Vec::new();
            for index in 0..10000 {
                let score = ((index * 997) % 113) as f64;
                let rank = Ranked { score, index };
                selected.push(rank, index);
                all.push((score, index));
                assert!(selected.heap.len() <= capacity);
            }
            all.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let actual: Vec<_> = selected
                .sorted()
                .into_iter()
                .map(|(r, index)| {
                    assert_eq!(r.index, index);
                    (r.score, index)
                })
                .collect();
            assert_eq!(actual, all[..capacity]);
        }
        assert!(TopK::<()>::new(0).is_err());
        assert!(TopK::<()>::new(4097).is_err());
    }
}
