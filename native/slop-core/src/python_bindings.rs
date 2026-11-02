//! Transitional CPython entry points; excluded from native builds.
use crate::*;
use engine::Ablation;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

fn to_dict<'py>(py: Python<'py>, a: &Ablation) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("rule", &a.rule)?;
    d.set_item("qualname", &a.qualname)?;
    d.set_item("line", a.line)?;
    d.set_item("description", &a.description)?;
    let edits = PyList::empty(py);
    for e in &a.edits {
        edits.append((e.start, e.end, &e.replacement))?;
    }
    d.set_item("edits", edits)?;
    Ok(d)
}

/// Every ablation the enabled rules find in `source`.
#[pyfunction]
#[pyo3(signature = (source, rules=None, extra=None))]
fn ablations(
    py: Python<'_>,
    source: &str,
    rules: Option<Vec<String>>,
    extra: Option<Vec<String>>,
) -> PyResult<Py<PyList>> {
    let chosen = crate::ablation::select(rules.as_deref(), extra.as_deref().unwrap_or_default())
        .map_err(pyo3::exceptions::PyKeyError::new_err)?;
    let found = crate::ablation::collect(source, &chosen)
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
    let out = PyList::empty(py);
    for a in &found {
        out.append(to_dict(py, a)?)?;
    }
    Ok(out.unbind())
}

/// Apply one ablation's edits, returning the mutated source.
#[pyfunction]
fn apply(source: &str, edits: Vec<(usize, usize, String)>) -> PyResult<String> {
    let a = Ablation {
        rule: String::new(),
        qualname: String::new(),
        line: 0,
        description: String::new(),
        edits: edits
            .into_iter()
            .map(|(start, end, replacement)| engine::Edit {
                start,
                end,
                replacement,
            })
            .collect(),
    };
    engine::apply(source, &a).map_err(pyo3::exceptions::PyValueError::new_err)
}

/// `(default_rules, optional_rules)`.
#[pyfunction]
fn rule_names() -> (Vec<&'static str>, Vec<&'static str>) {
    (
        rules::DEFAULT.iter().map(|(n, _)| *n).collect(),
        rules::OPTIONAL.iter().map(|(n, _)| *n).collect(),
    )
}

/// A repository's test files, indexed once and queried many times.
///
/// The gate asks two questions per unit of work -- which tests drive this module,
/// and does any test so much as name this function -- and each used to cost a full
/// repository scan or a subprocess. Both are answered here from one pass.
#[pyclass(name = "TestIndex", module = "slop_core", frozen)]
struct PyTestIndex {
    inner: index::TestIndex,
}

#[pymethods]
impl PyTestIndex {
    /// Test files that import `module`, given as a repository-relative path
    /// (`conductor/equivalence_gate.py`) or a dotted name.
    fn drivers_for(&self, module: &str) -> Vec<String> {
        self.inner.drivers_for(module)
    }

    /// Test files in which `name` appears as a whole word.
    fn named_by(&self, name: &str) -> Vec<String> {
        self.inner.named_by(name)
    }

    #[getter]
    fn file_count(&self) -> usize {
        self.inner.file_count()
    }

    #[getter]
    fn import_key_count(&self) -> usize {
        self.inner.import_key_count()
    }

    #[getter]
    fn name_key_count(&self) -> usize {
        self.inner.name_key_count()
    }

    fn __repr__(&self) -> String {
        format!(
            "<TestIndex {} files, {} import keys, {} name keys>",
            self.inner.file_count(),
            self.inner.import_key_count(),
            self.inner.name_key_count()
        )
    }
}

/// Index every `test_*.py` under `root`.
#[pyfunction]
fn build_test_index(py: Python<'_>, root: &str) -> PyResult<PyTestIndex> {
    let path = std::path::PathBuf::from(root);
    // Loudly, because the quiet failure here is the expensive one: an index built
    // over a directory that does not exist answers "no driver tests" for every
    // module, and the gate reports a clean sweep that measured nothing.
    if !path.is_dir() {
        return Err(pyo3::exceptions::PyNotADirectoryError::new_err(format!(
            "cannot index {root:?}: not a directory"
        )));
    }
    // The walk and the parse touch no Python object, so the GIL is free meanwhile.
    let inner = py.detach(move || index::build(&path));
    Ok(PyTestIndex { inner })
}

/// `pkg/mod.py` -> `pkg.mod`.
#[pyfunction]
fn dotted_for(module: &str) -> String {
    index::dotted_for(module)
}

#[pymodule]
fn slop_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(
        audit_inventory::audit_inventory_candidates,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(ablations, m)?)?;
    m.add_function(wrap_pyfunction!(apply, m)?)?;
    m.add_function(wrap_pyfunction!(rule_names, m)?)?;
    m.add_function(wrap_pyfunction!(build_test_index, m)?)?;
    m.add_function(wrap_pyfunction!(dotted_for, m)?)?;
    m.add_function(wrap_pyfunction!(gate::equivalence_gate, m)?)?;
    m.add_function(wrap_pyfunction!(
        equivalence_guest::equivalence_probe_guest,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(
        equivalence_guest::equivalence_probe_guest_cli,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(gate_report::equivalence_report, m)?)?;
    m.add_function(wrap_pyfunction!(gate_report::equivalence_findings, m)?)?;
    m.add_function(wrap_pyfunction!(backlog::equivalence_backlog, m)?)?;
    m.add_function(wrap_pyfunction!(
        file_families::audit_file_family_compare,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(
        file_families::audit_file_family_exact_pairs,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(
        file_families::audit_file_family_lsh_pairs,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(
        file_families::audit_file_family_groups,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(
        file_profiles::audit_file_family_profiles,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(detector_scan::audit_detector_scan, m)?)?;
    m.add_function(wrap_pyfunction!(detector_scan::fallback_scan_files, m)?)?;
    m.add_function(wrap_pyfunction!(
        consolidation::audit_consolidation_normalize,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(
        consolidation::audit_consolidation_collect,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(
        consolidation::audit_consolidation_collect_profiles,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(consolidation::audit_abstraction_pairs, m)?)?;
    m.add_function(wrap_pyfunction!(
        consolidation::audit_consolidation_build,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(
        consolidation::audit_consolidation_assign,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(
        consolidation::audit_consolidation_suggested_home,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(
        consolidation::audit_consolidation_evidence,
        m
    )?)?;
    m.add_function(wrap_pyfunction!(repository_scan::audit_repository_scan, m)?)?;
    m.add_function(wrap_pyfunction!(rust_scan::rust_scan_files, m)?)?;
    m.add_function(wrap_pyfunction!(rust_scan::rust_scan_rules, m)?)?;
    m.add_function(wrap_pyfunction!(style_scan::style_scan_files, m)?)?;
    m.add_function(wrap_pyfunction!(style_scan::style_scan_rules, m)?)?;
    m.add_function(wrap_pyfunction!(hot_loop_scan::hot_loop_scan_files, m)?)?;
    m.add_function(wrap_pyfunction!(test_value_scan::test_value_scan_files, m)?)?;
    m.add_function(wrap_pyfunction!(test_value_scan::test_value_scan_rules, m)?)?;
    m.add_function(wrap_pyfunction!(verbosity_scan::verbosity_scan_files, m)?)?;
    m.add_function(wrap_pyfunction!(verbosity_scan::verbosity_scan_rules, m)?)?;
    m.add_class::<PyTestIndex>()?;
    Ok(())
}
