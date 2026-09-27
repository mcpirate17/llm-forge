//! Independent Rust-owned detector scan and fallback reference.

use crate::ast_ref::{self, ast, attr_str, attr_usize, children, is_any, is_kind, walk};
use crate::support::{module, path};
use pyo3::exceptions::{PyOSError, PySyntaxError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

fn allowlist(repo: &Path) -> (HashSet<String>, HashSet<String>) {
    let path = repo.join("conductor/guardrail_allowlist.json");
    let Ok(bytes) = fs::read(path) else {
        return (HashSet::new(), HashSet::new());
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return (HashSet::new(), HashSet::new());
    };
    let values = |key: &str| -> HashSet<String> {
        value
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    (values("god_files"), values("god_functions"))
}

fn read_text(py: Python<'_>, source_path: &Path) -> PyResult<String> {
    let file = module(py, "pathlib")
        .getattr("Path")?
        .call1((source_path.to_string_lossy().as_ref(),))?;
    let options = PyDict::new(py);
    options.set_item("encoding", "utf-8")?;
    options.set_item("errors", "replace")?;
    file.call_method("read_text", (), Some(&options))?.extract()
}

fn fallback<'py>(
    py: Python<'py>,
    handler: &Bound<'py, PyAny>,
    relative: &str,
) -> Option<Bound<'py, PyDict>> {
    let body = handler
        .getattr("body")
        .unwrap()
        .cast_into::<PyList>()
        .unwrap();
    let nodes: Vec<_> = body.iter().flat_map(|statement| walk(&statement)).collect();
    let calls: Vec<_> = nodes.iter().filter(|node| is_kind(node, "Call")).collect();
    let has_raise = nodes.iter().any(|node| is_kind(node, "Raise"));
    let signals = calls.iter().any(|call| {
        let func = call.getattr("func").unwrap();
        (is_kind(&func, "Name") && ["print", "warn"].contains(&attr_str(&func, "id").as_str()))
            || (is_kind(&func, "Attribute")
                && ["critical", "error", "exception", "warning", "warn"]
                    .contains(&attr_str(&func, "attr").as_str()))
    });
    if has_raise || signals {
        return None;
    }
    let control = |statement: &Bound<'_, PyAny>| is_any(statement, &["Pass", "Continue", "Break"]);
    let control_only = body.iter().all(|statement| control(&statement));
    let exception = handler.getattr("type").unwrap();
    let broad = exception.is_none()
        || walk(&exception).iter().any(|node| {
            is_kind(node, "Name")
                && ["BaseException", "Exception"].contains(&attr_str(node, "id").as_str())
        });
    let silent_default = broad
        && calls.is_empty()
        && body
            .iter()
            .all(|statement| control(&statement) || is_any(&statement, &["Return", "Assign"]));
    if !control_only && !silent_default {
        return None;
    }
    let (confidence, value, severity) = if exception.is_none() {
        (0.98, 100, "critical")
    } else if broad && control_only {
        (0.90, 80, "high")
    } else if broad {
        (0.75, 45, "medium")
    } else {
        (0.65, 25, "medium")
    };
    let name: String = if exception.is_none() {
        "bare except".into()
    } else {
        ast(py)
            .getattr("unparse")
            .unwrap()
            .call1((&exception,))
            .unwrap()
            .extract()
            .unwrap()
    };
    let location = format!("{relative}:{}", attr_usize(handler, "lineno"));
    let result = PyDict::new(py);
    result
        .set_item("id", format!("fallback:{location}"))
        .unwrap();
    result.set_item("category", "silent_fallbacks").unwrap();
    result.set_item("severity", severity).unwrap();
    result.set_item("confidence", confidence).unwrap();
    result.set_item("value", value).unwrap();
    result
        .set_item("files", PyList::new(py, [relative]).unwrap())
        .unwrap();
    result.set_item("location", location).unwrap();
    result
        .set_item(
            "evidence",
            format!("except {name} has no raise, warning, or error log"),
        )
        .unwrap();
    Some(result)
}

struct Scan<'py> {
    god_files: Bound<'py, PyList>,
    god_functions: Bound<'py, PyList>,
    fallbacks: Bound<'py, PyList>,
    unparsable: Bound<'py, PyList>,
}

fn scan_file(
    py: Python<'_>,
    source_path: &Path,
    repo: &Path,
    source: &str,
    allowed_files: &HashSet<String>,
    allowed_functions: &HashSet<String>,
    result: &Scan<'_>,
) {
    let relative = source_path
        .strip_prefix(repo)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    let detectors = module(py, "conductor.reuse.detectors");
    let file_limit: usize = detectors
        .getattr("GOD_FILE_LINES")
        .unwrap()
        .extract()
        .unwrap();
    let function_limit: usize = detectors
        .getattr("GOD_FUNC_LINES")
        .unwrap()
        .extract()
        .unwrap();
    let lines: Vec<&str> = source.lines().collect();
    let line_count = source.matches('\n').count() + 1;
    if line_count > file_limit
        && !allowed_files.contains(&relative)
        && !source.contains("# guardrail: allow-god-file")
    {
        result
            .god_files
            .append(format!("{} ({line_count})", source_path.display()))
            .unwrap();
    }
    if source_path.extension().and_then(|part| part.to_str()) != Some("py") {
        return;
    }
    let tree = match ast_ref::parse(py, source, &source_path.to_string_lossy()) {
        Ok(tree) => tree,
        Err(error) if error.is_instance_of::<PySyntaxError>(py) => {
            result.unparsable.append(relative).unwrap();
            return;
        }
        Err(error) => panic!("unexpected parse failure: {error}"),
    };
    for node in walk(&tree) {
        if is_any(&node, &["FunctionDef", "AsyncFunctionDef"]) {
            scan_function(
                &node,
                source_path,
                &relative,
                function_limit,
                allowed_functions,
                &lines,
                result,
            );
        } else if is_kind(&node, "ExceptHandler") {
            if let Some(candidate) = fallback(py, &node, &relative) {
                result.fallbacks.append(candidate).unwrap();
            }
        }
    }
}

fn scan_function(
    node: &Bound<'_, PyAny>,
    source_path: &Path,
    relative: &str,
    function_limit: usize,
    allowed_functions: &HashSet<String>,
    lines: &[&str],
    result: &Scan<'_>,
) {
    let start = attr_usize(node, "lineno");
    let end_attr = node.getattr("end_lineno").unwrap();
    let end: usize = if end_attr.is_none() {
        start
    } else {
        end_attr.extract().unwrap()
    };
    let span = end - start + 1;
    let name = attr_str(node, "name");
    let route_wrapper = name.starts_with("register_")
        && children(node)
            .iter()
            .any(|child| is_any(child, &["FunctionDef", "AsyncFunctionDef"]));
    let key = format!("{relative}::{name}");
    let marker = lines[start - 1..end]
        .join("\n")
        .contains("# guardrail: allow-god-function");
    if span > function_limit && !route_wrapper && !allowed_functions.contains(&key) && !marker {
        result
            .god_functions
            .append(format!("{}:{start} {name} ({span})", source_path.display()))
            .unwrap();
    }
}

pub fn reference_scan<'py>(py: Python<'py>, paths: &[&Path], repo: &Path) -> Bound<'py, PyAny> {
    let (allowed_files, allowed_functions) = allowlist(repo);
    let result = Scan {
        god_files: PyList::empty(py),
        god_functions: PyList::empty(py),
        fallbacks: PyList::empty(py),
        unparsable: PyList::empty(py),
    };
    for source_path in paths {
        match read_text(py, source_path) {
            Ok(source) => scan_file(
                py,
                source_path,
                repo,
                &source,
                &allowed_files,
                &allowed_functions,
                &result,
            ),
            Err(error) if error.is_instance_of::<PyOSError>(py) => {}
            Err(error) => panic!("unexpected read failure: {error}"),
        }
    }
    (
        result.god_files.len(),
        result.god_functions.len(),
        result.god_files,
        result.god_functions,
        result.fallbacks,
        result.unparsable,
    )
        .into_pyobject(py)
        .unwrap()
        .into_any()
}

pub fn native_scan<'py>(py: Python<'py>, paths: &[&Path], repo: &Path) -> Bound<'py, PyAny> {
    let (allowed_files, allowed_functions) = allowlist(repo);
    let mut allowed_files: Vec<String> = allowed_files.into_iter().collect();
    let mut allowed_functions: Vec<String> = allowed_functions.into_iter().collect();
    allowed_files.sort();
    allowed_functions.sort();
    let options = PyDict::new(py);
    options
        .set_item(
            "paths",
            paths
                .iter()
                .map(|item| item.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap();
    options
        .set_item("repo", repo.to_string_lossy().to_string())
        .unwrap();
    options.set_item("allowed_files", allowed_files).unwrap();
    options
        .set_item("allowed_functions", allowed_functions)
        .unwrap();
    let detectors = module(py, "conductor.reuse.detectors");
    options
        .set_item(
            "god_file_lines",
            detectors.getattr("GOD_FILE_LINES").unwrap(),
        )
        .unwrap();
    options
        .set_item(
            "god_func_lines",
            detectors.getattr("GOD_FUNC_LINES").unwrap(),
        )
        .unwrap();
    module(py, "conductor.reuse")
        .getattr("core")
        .unwrap()
        .getattr("audit_detector_scan")
        .unwrap()
        .call((), Some(&options))
        .unwrap()
}

pub fn detector_scan<'py>(
    py: Python<'py>,
    paths: &[&Path],
    repo: &Path,
) -> PyResult<Bound<'py, PyAny>> {
    let py_paths = PyList::new(py, paths.iter().map(|item| path(py, item))).unwrap();
    module(py, "conductor.reuse.detectors")
        .getattr("_detector_scan")?
        .call1((py_paths, path(py, repo)))
}
