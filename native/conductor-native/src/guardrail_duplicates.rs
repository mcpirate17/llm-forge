//! Global candidate index for Pylint's similarity checker.
//!
//! Pylint normalizes each file once in Python. This index replaces its Cartesian
//! file-pair walk; Pylint still decides and groups the actual findings. Windows
//! use a sorted multiset because Pylint's LinesChunk hash is order-independent.
//! No directory partition or batch boundary can hide a cross-file duplicate.

use std::collections::{BTreeSet, HashMap, HashSet};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

type CandidateResult = (Vec<(usize, usize)>, usize, usize);

fn candidate_pairs(files: &[Vec<String>], window: usize) -> CandidateResult {
    let mut vocabulary: HashMap<&str, usize> = HashMap::new();
    let mut postings: HashMap<Vec<usize>, Vec<usize>> = HashMap::new();
    let mut pairs = BTreeSet::new();
    let mut window_count = 0;
    for (file_id, lines) in files.iter().enumerate() {
        let ids: Vec<usize> = lines
            .iter()
            .map(|line| {
                let next = vocabulary.len();
                *vocabulary.entry(line.as_str()).or_insert(next)
            })
            .collect();
        let mut seen = HashSet::new();
        for lines in ids.windows(window) {
            window_count += 1;
            let mut key = lines.to_vec();
            key.sort_unstable();
            if !seen.insert(key.clone()) {
                continue;
            }
            let owners = postings.entry(key).or_default();
            for &other in owners.iter() {
                pairs.insert((other, file_id));
            }
            owners.push(file_id);
        }
    }
    (pairs.into_iter().collect(), window_count, postings.len())
}

#[pyfunction]
fn guardrail_duplicate_candidates_native(
    py: Python<'_>,
    files: Vec<Vec<String>>,
    window: usize,
) -> PyResult<CandidateResult> {
    if window == 0 {
        return Err(PyValueError::new_err("similarity window must be positive"));
    }
    Ok(py.detach(|| candidate_pairs(&files, window)))
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(
        guardrail_duplicate_candidates_native,
        module
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn cross_directory_pairs_and_repeated_windows_are_deduplicated() {
        let files = vec![
            lines(&["one", "two", "one", "two"]),
            lines(&["other", "content"]),
            lines(&["two", "one", "two"]),
        ];
        let (pairs, windows, distinct) = candidate_pairs(&files, 2);
        assert_eq!(pairs, vec![(0, 2)]);
        assert_eq!(windows, 6);
        assert_eq!(distinct, 2);
    }

    #[test]
    fn short_files_and_no_common_windows_produce_no_pairs() {
        let files = vec![lines(&["short"]), lines(&["one", "two"])];
        assert_eq!(candidate_pairs(&files, 2), (vec![], 1, 1));
        assert_eq!(candidate_pairs(&[], 2), (vec![], 0, 0));
    }
}
