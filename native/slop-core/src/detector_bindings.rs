//! Transitional object conversion only. Parsing and classification run without the GIL.
use super::*;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

type ScanOutput = (
    usize,
    usize,
    Vec<String>,
    Vec<String>,
    Py<PyList>,
    Vec<String>,
);

fn fallback_to_dict(py: Python<'_>, finding: Fallback) -> PyResult<Bound<'_, PyDict>> {
    let output = PyDict::new(py);
    let location = format!("{}:{}", finding.relative, finding.line);
    output.set_item("id", format!("fallback:{location}"))?;
    output.set_item("category", "silent_fallbacks")?;
    output.set_item("severity", finding.severity)?;
    output.set_item("confidence", finding.confidence)?;
    output.set_item("value", finding.value)?;
    output.set_item("files", [finding.relative])?;
    output.set_item("location", location)?;
    output.set_item(
        "evidence",
        format!(
            "except {} has no raise, warning, or error log",
            finding.exception
        ),
    )?;
    Ok(output)
}

fn inspect_paths(
    paths: &[String],
    repo: Option<&str>,
    policy: &Policy<'_>,
) -> Result<Scan, String> {
    if paths.len() > 256 {
        return Err("detector accepts at most 256 files".into());
    }
    let mut out = Scan::default();
    let mut bytes = 0;
    for raw in paths {
        let path = Path::new(raw);
        let relative = match repo {
            Some(root) => path
                .strip_prefix(root)
                .map_err(|_| format!("detector path {raw} is outside repository {root}"))?,
            None => path,
        }
        .to_string_lossy()
        .replace('\\', "/");
        let source = read_source(path)?;
        bytes += source.len();
        if bytes > 16 * MAX_SOURCE_BYTES {
            return Err("detector exceeds 16 MiB input budget".into());
        }
        let scan = inspect(&relative, &source, policy)?;
        // Preserve the adapter's absolute locations; native source APIs use caller labels.
        out.god_files.extend(
            scan.god_files
                .into_iter()
                .map(|s| format!("{raw}{}", &s[relative.len()..])),
        );
        out.god_functions.extend(
            scan.god_functions
                .into_iter()
                .map(|s| format!("{raw}{}", &s[relative.len()..])),
        );
        out.fallbacks.extend(scan.fallbacks);
        out.unparsable.extend(scan.unparsable);
    }
    Ok(out)
}

#[pyfunction]
#[pyo3(signature = (paths, repo, allowed_files, allowed_functions, god_file_lines, god_func_lines))]
pub fn audit_detector_scan(
    py: Python<'_>,
    paths: Vec<String>,
    repo: Option<String>,
    allowed_files: Vec<String>,
    allowed_functions: Vec<String>,
    god_file_lines: usize,
    god_func_lines: usize,
) -> PyResult<ScanOutput> {
    let allowed_files = allowed_files.into_iter().collect();
    let allowed_functions = allowed_functions.into_iter().collect();
    let policy = Policy {
        allowed_files: &allowed_files,
        allowed_functions: &allowed_functions,
        god_file_lines,
        god_func_lines,
    };
    let scan = py
        .detach(|| inspect_paths(&paths, repo.as_deref(), &policy))
        .map_err(PyValueError::new_err)?;
    let output = PyList::empty(py);
    for finding in scan.fallbacks {
        output.append(fallback_to_dict(py, finding)?)?;
    }
    Ok((
        scan.god_files.len(),
        scan.god_functions.len(),
        scan.god_files,
        scan.god_functions,
        output.unbind(),
        scan.unparsable,
    ))
}

#[pyfunction]
pub fn fallback_scan_files(py: Python<'_>, paths: Vec<String>) -> PyResult<Py<PyList>> {
    let findings = py
        .detach(|| scan_paths(&paths))
        .map_err(PyValueError::new_err)?;
    let output = PyList::empty(py);
    for finding in findings {
        let row = PyDict::new(py);
        row.set_item("path", finding.path)?;
        row.set_item("line", finding.line)?;
        row.set_item("rule", finding.rule)?;
        row.set_item("message", finding.message)?;
        output.append(row)?;
    }
    Ok(output.unbind())
}
