//! Stable Python input for the candidate-review latency benchmark.
//!
//! The benchmark's small-Python and full-review scenarios edit a shipped module
//! that the fixture already copies and claims. Retired test suites are not inputs.

use std::fs;
use std::path::Path;

#[cfg(feature = "python")]
use pyo3::exceptions::PyValueError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use pyo3::types::PyBytes;

pub const PYTHON_INPUT_PATH: &str = "conductor/candidate_review/benchmark.py";
const MARKER: &[u8] =
    b"\"\"\"Reproducible latency benchmarks using only temporary repositories and indexes.\"\"\"";
const REPLACEMENT: &[u8] = b"\"\"\"Reproducible latency benchmarks using only temporary repositories and indexes. Python benchmark probe.\"\"\"";

fn replace_marker(content: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut matches = content
        .windows(MARKER.len())
        .enumerate()
        .filter_map(|(offset, window)| (window == MARKER).then_some(offset));
    let Some(offset) = matches.next() else {
        return Err("marker was not found");
    };
    if matches.next().is_some() {
        return Err("marker is ambiguous");
    }
    let mut changed = Vec::with_capacity(content.len() + REPLACEMENT.len() - MARKER.len());
    changed.extend_from_slice(&content[..offset]);
    changed.extend_from_slice(REPLACEMENT);
    changed.extend_from_slice(&content[offset + MARKER.len()..]);
    Ok(changed)
}

/// Return an exact one-file candidate change, or fail if the shipped input drifts.
pub fn python_input(source: &Path) -> Result<(&'static str, Vec<u8>), String> {
    let path = source.join(PYTHON_INPUT_PATH);
    let content = fs::read(&path).map_err(|error| {
        format!(
            "required small-Python benchmark source is missing or unreadable: {}: {error}",
            path.display()
        )
    })?;
    let changed = replace_marker(&content)
        .map_err(|error| format!("small-Python benchmark {error} in {}", path.display()))?;
    Ok((PYTHON_INPUT_PATH, changed))
}

#[cfg(feature = "python")]
#[pyfunction]
fn candidate_benchmark_python_input_native(
    py: Python<'_>,
    source_dir: &str,
) -> PyResult<(&'static str, Py<PyBytes>)> {
    let (relative, content) = python_input(Path::new(source_dir)).map_err(PyValueError::new_err)?;
    Ok((relative, PyBytes::new(py, &content).unbind()))
}

#[cfg(feature = "python")]
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(
        candidate_benchmark_python_input_native,
        module
    )?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_shipped_module_has_one_marker_and_one_changed_line() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src");
        let original = fs::read(source.join(PYTHON_INPUT_PATH)).unwrap();
        let (relative, changed) = python_input(&source).unwrap();
        assert_eq!(relative, PYTHON_INPUT_PATH);
        assert_eq!(
            changed.len(),
            original.len() + REPLACEMENT.len() - MARKER.len()
        );
        assert!(changed.starts_with(REPLACEMENT));
        assert_eq!(&changed[REPLACEMENT.len()..], &original[MARKER.len()..]);
    }

    #[test]
    fn absent_and_duplicate_markers_fail_loudly() {
        assert_eq!(
            replace_marker(b"\"\"\"Different module\"\"\"\n").unwrap_err(),
            "marker was not found"
        );
        assert_eq!(
            replace_marker(&[MARKER, b"\n", MARKER].concat()).unwrap_err(),
            "marker is ambiguous"
        );
    }
}
