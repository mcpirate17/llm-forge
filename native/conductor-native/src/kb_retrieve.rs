//! Retrieval scoring for the knowledge-base card index.
//!
//! The Python boundary owns the embedding HTTP request and the index JSON. Rust
//! owns the per-query float arithmetic: one dot product per card over the whole
//! index, which ran as a generator-expression `sum()` per card in Python.

use std::cmp::Ordering;

#[cfg(feature = "python")]
use pyo3::exceptions::PyValueError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::{PyDict, PyList};

#[derive(Clone, Debug, PartialEq)]
pub struct KbCard {
    pub name: String,
    pub path: String,
    pub text: String,
    pub vector: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScoredKbCard {
    pub name: String,
    pub path: String,
    pub score: f64,
    pub text: String,
}

/// Match the improved Kahan-Babuska/Neumaier loop used by CPython 3.12 sum(),
/// so scores and norms stay bit-identical to the Python they replaced. Same
/// loop as ``memory_index.rs``'s ``compensated_dot``.
pub(crate) fn compensated_sum(values: impl Iterator<Item = f64>) -> f64 {
    let mut total = 0.0_f64;
    let mut compensation = 0.0_f64;
    for value in values {
        let next = total + value;
        if total.abs() >= value.abs() {
            compensation += (total - next) + value;
        } else {
            compensation += (value - next) + total;
        }
        total = next;
    }
    if compensation != 0.0 && compensation.is_finite() {
        total += compensation;
    }
    total
}

fn compensated_dot(left: &[f64], right: &[f64]) -> f64 {
    compensated_sum(left.iter().zip(right).map(|(a, b)| a * b))
}

/// Sort descending while treating NaN the way Python's sort does: every
/// comparison against NaN is false, so a NaN score behaves as "equal to"
/// everything and a stable sort keeps it at its original relative position.
fn sort_descending_python_stable(scores: &mut [(f64, usize)]) {
    scores.sort_by(|(left, _), (right, _)| {
        left.partial_cmp(right).unwrap_or(Ordering::Equal).reverse()
    });
}

#[cfg(feature = "python")]
fn dict_string(row: &Bound<'_, PyDict>, field: &str) -> PyResult<String> {
    row.get_item(field)?
        .ok_or_else(|| PyValueError::new_err(format!("index card is missing {field:?}")))?
        .extract()
        .map_err(|_| PyValueError::new_err(format!("index card field {field:?} must be a string")))
}

/// Mirror ``kb_retrieve._l2_normalize``: ``math.sqrt(sum(x * x))`` then divide.
pub fn l2_normalize(vector: &[f64]) -> Result<Vec<f64>, String> {
    let norm = compensated_sum(vector.iter().map(|value| value * value)).sqrt();
    if norm == 0.0 {
        return Err("embedding is the zero vector".to_owned());
    }
    Ok(vector.iter().map(|value| value / norm).collect())
}

#[cfg(feature = "python")]
#[pyfunction]
fn kb_retrieve_l2_normalize_native(vector: Vec<f64>) -> PyResult<Vec<f64>> {
    l2_normalize(&vector).map_err(PyValueError::new_err)
}

/// Score every index card against ``query`` and return the top ``top_k`` rows
/// as ``(name, path, score, text)`` tuples: highest score first, ties keep
/// index order (Python's stable sort).
pub fn score_cards(
    query: &[f64],
    cards: &[KbCard],
    top_k: usize,
) -> Result<Vec<ScoredKbCard>, String> {
    let mut scored: Vec<(f64, usize)> = Vec::with_capacity(cards.len());
    for (ordinal, card) in cards.iter().enumerate() {
        if card.vector.len() != query.len() {
            return Err(format!(
                "vector dim mismatch for {}: {} != {}",
                card.name,
                card.vector.len(),
                query.len()
            ));
        }
        scored.push((compensated_dot(query, &card.vector), ordinal));
    }
    sort_descending_python_stable(&mut scored);
    scored.truncate(top_k);
    Ok(scored
        .into_iter()
        .map(|(score, ordinal)| {
            let card = &cards[ordinal];
            ScoredKbCard {
                name: card.name.clone(),
                path: card.path.clone(),
                score,
                text: card.text.clone(),
            }
        })
        .collect())
}

#[cfg(feature = "python")]
#[pyfunction]
#[pyo3(signature = (query, cards, top_k))]
fn kb_retrieve_score_cards_native(
    query: Vec<f64>,
    cards: &Bound<'_, PyList>,
    top_k: usize,
) -> PyResult<Vec<(String, String, f64, String)>> {
    let mut rows = Vec::with_capacity(cards.len());
    for card in cards.iter() {
        let row = card
            .cast::<PyDict>()
            .map_err(|_| PyValueError::new_err("index card must be a JSON object"))?;
        let name = dict_string(row, "name")?;
        let path = dict_string(row, "path")?;
        let text = dict_string(row, "text")?;
        let vector = row
            .get_item("vector")?
            .ok_or_else(|| PyValueError::new_err("index card is missing \"vector\""))?
            .extract::<Vec<f64>>()?;
        rows.push(KbCard {
            name,
            path,
            text,
            vector,
        });
    }
    let scored = score_cards(&query, &rows, top_k).map_err(PyValueError::new_err)?;
    Ok(scored
        .into_iter()
        .map(|row| (row.name, row.path, row.score, row.text))
        .collect())
}

#[cfg(feature = "python")]
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(kb_retrieve_l2_normalize_native, module)?)?;
    module.add_function(wrap_pyfunction!(kb_retrieve_score_cards_native, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compensated_sum_recovers_what_a_naive_fold_drops() {
        let values = [1e100, 1.0, -1e100];
        let naive = values.iter().sum::<f64>();
        let ours = compensated_sum(values.iter().copied());
        assert_eq!(naive, 0.0, "the unlucky vector must actually bite");
        assert!((ours - 1.0).abs() < 1e-6, "got {ours}");
    }

    #[test]
    fn compensated_sum_agrees_with_plain_arithmetic_on_benign_vectors() {
        let values: Vec<f64> = (0..256).map(|i| f64::from(i % 7) * 0.1).collect();
        let naive: f64 = values.iter().sum();
        let ours = compensated_sum(values.iter().copied());
        assert!((ours - naive).abs() < 1e-9);
    }

    #[test]
    fn descending_sort_keeps_nan_in_place() {
        let mut scores = vec![(1.0, 0), (f64::NAN, 1), (0.5, 2)];
        sort_descending_python_stable(&mut scores);
        let order: Vec<usize> = scores.iter().map(|(_, i)| *i).collect();
        assert_eq!(order, vec![0, 1, 2], "NaN keeps its insertion position");
    }

    #[test]
    fn descending_sort_is_stable_on_ties() {
        let mut scores = vec![(0.5, 0), (1.0, 1), (0.5, 2), (1.0, 3)];
        sort_descending_python_stable(&mut scores);
        let order: Vec<usize> = scores.iter().map(|(_, i)| *i).collect();
        assert_eq!(order, vec![1, 3, 0, 2]);
    }

    #[test]
    fn dot_matches_the_scalar_reference() {
        let left = [0.1, 0.2, 0.3];
        let right = [0.4, 0.5, 0.6];
        let expected = 0.1 * 0.4 + 0.2 * 0.5 + 0.3 * 0.6;
        assert!((compensated_dot(&left, &right) - expected).abs() < 1e-15);
    }

    #[test]
    fn l2_normalize_produces_a_unit_norm() {
        let normalized = l2_normalize(&[3.0, 4.0]).unwrap();
        let norm = compensated_sum(normalized.iter().map(|v| v * v)).sqrt();
        assert!((norm - 1.0).abs() < 1e-12);
    }

    #[test]
    fn l2_normalize_rejects_the_zero_vector() {
        assert_eq!(
            l2_normalize(&[0.0, 0.0, 0.0]).unwrap_err(),
            "embedding is the zero vector"
        );
    }

    fn card(index: usize, vector: Vec<f64>) -> KbCard {
        KbCard {
            name: format!("c{index}"),
            path: format!("n/c{index}.md"),
            text: format!("t{index}"),
            vector,
        }
    }

    #[test]
    fn score_cards_preserves_metadata_and_compensated_dot() {
        let cards = [card(0, vec![1e100, 1.0, -1e100])];
        let scored = score_cards(&[1.0, 1.0, 1.0], &cards, 1).unwrap();
        assert_eq!(scored.len(), 1);
        assert_eq!(
            (&scored[0].name, &scored[0].path, &scored[0].text),
            (&cards[0].name, &cards[0].path, &cards[0].text)
        );
        assert_eq!(scored[0].score.to_bits(), 1.0_f64.to_bits());
    }

    #[test]
    fn score_cards_ties_keep_index_order_and_top_k_truncates() {
        let ties = (0..4)
            .map(|index| card(index, vec![1.0, 0.0]))
            .collect::<Vec<_>>();
        let scored = score_cards(&[1.0, 0.0], &ties, 4).unwrap();
        assert_eq!(
            scored
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["c0", "c1", "c2", "c3"]
        );
        let increasing = (0..10)
            .map(|index| card(index, vec![(index + 1) as f64]))
            .collect::<Vec<_>>();
        let scored = score_cards(&[1.0], &increasing, 3).unwrap();
        assert_eq!(
            scored
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            ["c9", "c8", "c7"]
        );
    }

    #[test]
    fn score_cards_names_the_card_and_dimensions_on_mismatch() {
        let cards = [card(3, vec![1.0, 2.0])];
        assert_eq!(
            score_cards(&[1.0, 2.0, 3.0], &cards, 1).unwrap_err(),
            "vector dim mismatch for c3: 2 != 3"
        );
    }

    #[test]
    fn l2_normalize_preserves_exact_division_for_large_vectors() {
        let vector = [-1e6, 2e6, 3e6, -4e6];
        let norm = compensated_sum(vector.iter().map(|value| value * value)).sqrt();
        let expected = vector.map(|value| value / norm);
        let normalized = l2_normalize(&vector).unwrap();
        assert_eq!(
            normalized
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|value| value.to_bits())
                .collect::<Vec<_>>()
        );
    }
}
