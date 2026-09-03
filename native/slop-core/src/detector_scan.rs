//! One-pass repository guardrail and silent-fallback scan.
//!
//! CPython remains the parser of record. Rust owns file reads, ordered AST
//! traversal, function-span checks, and exception-handler classification.

use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::path::PathBuf;

use pyo3::exceptions::{PyRuntimeError, PySyntaxError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::python_ast::{ast_children, descendants, list_items, string_attr, type_name};

const ALLOW_GOD_FILE: &str = "# guardrail: allow-god-file";
const ALLOW_GOD_FUNCTION: &str = "# guardrail: allow-god-function";

struct LoadedFile {
    path: PathBuf,
    relative: String,
    source: String,
}

#[derive(Debug)]
struct Fallback {
    relative: String,
    line: usize,
    exception: String,
    severity: &'static str,
    confidence: f64,
    value: usize,
}

type ScanOutput = (usize, usize, Vec<String>, Vec<String>, Py<PyList>);

fn load_files(paths: Vec<String>, repo: Option<String>) -> Result<Vec<LoadedFile>, String> {
    let repo = repo.map(PathBuf::from);
    let mut loaded = Vec::with_capacity(paths.len());
    for raw_path in paths {
        let path = PathBuf::from(raw_path);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let relative = match repo.as_ref() {
            Some(root) => path
                .strip_prefix(root)
                .map_err(|_| {
                    format!(
                        "detector path {} is outside repository {}",
                        path.display(),
                        root.display()
                    )
                })?
                .to_string_lossy()
                .replace('\\', "/"),
            None => path.to_string_lossy().replace('\\', "/"),
        };
        loaded.push(LoadedFile {
            path,
            relative,
            source: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    Ok(loaded)
}

fn breadth_first<'py>(root: &Bound<'py, PyAny>) -> PyResult<Vec<Bound<'py, PyAny>>> {
    let mut output = Vec::new();
    let mut queue = VecDeque::from([root.clone()]);
    while let Some(node) = queue.pop_front() {
        queue.extend(ast_children(&node)?);
        output.push(node);
    }
    Ok(output)
}

fn direct_route_wrapper(node: &Bound<'_, PyAny>, name: &str) -> PyResult<bool> {
    if !name.starts_with("register_") {
        return Ok(false);
    }
    for child in ast_children(node)? {
        if matches!(
            type_name(&child)?.as_str(),
            "FunctionDef" | "AsyncFunctionDef"
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn function_marker(source_lines: &[&str], start: usize, end: usize) -> bool {
    source_lines
        .get(start..end.min(source_lines.len()))
        .is_some_and(|lines| lines.iter().any(|line| line.contains(ALLOW_GOD_FUNCTION)))
}

fn function_findings(
    nodes: &[Bound<'_, PyAny>],
    file: &LoadedFile,
    allowed_functions: &BTreeSet<String>,
    threshold: usize,
) -> PyResult<Vec<String>> {
    let source_lines: Vec<_> = file.source.split('\n').collect();
    let mut findings = Vec::new();
    for node in nodes {
        if !matches!(
            type_name(node)?.as_str(),
            "FunctionDef" | "AsyncFunctionDef"
        ) {
            continue;
        }
        let line: usize = node.getattr("lineno")?.extract()?;
        let end: Option<usize> = node.getattr("end_lineno")?.extract()?;
        let end = end.unwrap_or(line);
        let span = end - line + 1;
        let name = string_attr(node, "name")?;
        let key = format!("{}::{name}", file.relative);
        if span > threshold
            && !direct_route_wrapper(node, &name)?
            && !allowed_functions.contains(&key)
            && !function_marker(&source_lines, line.saturating_sub(1), end)
        {
            findings.push(format!("{}:{line} {name} ({span})", file.path.display()));
        }
    }
    Ok(findings)
}

fn body_facts<'py>(handler: &Bound<'py, PyAny>) -> PyResult<(bool, Vec<Bound<'py, PyAny>>)> {
    let mut has_raise = false;
    let mut calls = Vec::new();
    for statement in list_items(&handler.getattr("body")?)? {
        for node in descendants(&statement)? {
            match type_name(&node)?.as_str() {
                "Raise" => has_raise = true,
                "Call" => calls.push(node),
                _ => {}
            }
        }
    }
    Ok((has_raise, calls))
}

fn handler_signals(calls: &[Bound<'_, PyAny>]) -> PyResult<bool> {
    for call in calls {
        let function = call.getattr("func")?;
        match type_name(&function)?.as_str() {
            "Name" if matches!(string_attr(&function, "id")?.as_str(), "print" | "warn") => {
                return Ok(true);
            }
            "Attribute"
                if matches!(
                    string_attr(&function, "attr")?.as_str(),
                    "critical" | "error" | "exception" | "warning" | "warn"
                ) =>
            {
                return Ok(true);
            }
            _ => {}
        }
    }
    Ok(false)
}

fn handler_is_broad(handler: &Bound<'_, PyAny>) -> PyResult<bool> {
    let exception_type = handler.getattr("type")?;
    if exception_type.is_none() {
        return Ok(true);
    }
    for node in descendants(&exception_type)? {
        if type_name(&node)? == "Name"
            && matches!(
                string_attr(&node, "id")?.as_str(),
                "BaseException" | "Exception"
            )
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn fallback_rank(bare: bool, broad: bool, control_only: bool) -> (f64, usize, &'static str) {
    if bare {
        (0.98, 100, "critical")
    } else if broad && control_only {
        (0.90, 80, "high")
    } else if broad {
        (0.75, 45, "medium")
    } else {
        (0.65, 25, "medium")
    }
}

fn fallback_candidate(
    ast: &Bound<'_, PyModule>,
    handler: &Bound<'_, PyAny>,
    relative: &str,
) -> PyResult<Option<Fallback>> {
    let body = list_items(&handler.getattr("body")?)?;
    let (has_raise, calls) = body_facts(handler)?;
    if has_raise {
        return Ok(None);
    }
    if handler_signals(&calls)? {
        return Ok(None);
    }
    let control_only = body.iter().try_fold(true, |all, statement| {
        Ok::<_, PyErr>(
            all && matches!(
                type_name(statement)?.as_str(),
                "Pass" | "Continue" | "Break"
            ),
        )
    })?;
    let broad = handler_is_broad(handler)?;
    let silent_default = broad
        && calls.is_empty()
        && body.iter().try_fold(true, |all, statement| {
            Ok::<_, PyErr>(
                all && matches!(
                    type_name(statement)?.as_str(),
                    "Pass" | "Continue" | "Break" | "Return" | "Assign"
                ),
            )
        })?;
    if !control_only && !silent_default {
        return Ok(None);
    }
    let exception_type = handler.getattr("type")?;
    let bare = exception_type.is_none();
    let exception = if bare {
        "bare except".to_owned()
    } else {
        ast.call_method1("unparse", (&exception_type,))?.extract()?
    };
    let (confidence, value, severity) = fallback_rank(bare, broad, control_only);
    Ok(Some(Fallback {
        relative: relative.to_owned(),
        line: handler.getattr("lineno")?.extract()?,
        exception,
        severity,
        confidence,
        value,
    }))
}

fn fallback_findings(
    ast: &Bound<'_, PyModule>,
    nodes: &[Bound<'_, PyAny>],
    relative: &str,
) -> PyResult<Vec<Fallback>> {
    let mut findings = Vec::new();
    for node in nodes {
        if type_name(node)? == "ExceptHandler" {
            if let Some(candidate) = fallback_candidate(ast, node, relative)? {
                findings.push(candidate);
            }
        }
    }
    Ok(findings)
}

fn fallback_to_dict<'py>(py: Python<'py>, finding: Fallback) -> PyResult<Bound<'py, PyDict>> {
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

/// Return god-file, god-function, and silent-fallback findings in input/AST order.
#[pyfunction]
#[pyo3(signature = (paths, repo, allowed_files, allowed_functions, god_file_lines, god_func_lines))]
pub(crate) fn audit_detector_scan(
    py: Python<'_>,
    paths: Vec<String>,
    repo: Option<String>,
    allowed_files: Vec<String>,
    allowed_functions: Vec<String>,
    god_file_lines: usize,
    god_func_lines: usize,
) -> PyResult<ScanOutput> {
    let files = py
        .detach(move || load_files(paths, repo))
        .map_err(PyValueError::new_err)?;
    let allowed_files: BTreeSet<_> = allowed_files.into_iter().collect();
    let allowed_functions: BTreeSet<_> = allowed_functions.into_iter().collect();
    let ast = py.import("ast")?;
    let mut god_files = Vec::new();
    let mut god_functions = Vec::new();
    let mut fallbacks = Vec::new();
    for file in files {
        let lines = file.source.matches('\n').count() + 1;
        if lines > god_file_lines
            && !allowed_files.contains(&file.relative)
            && !file.source.contains(ALLOW_GOD_FILE)
        {
            god_files.push(format!("{} ({lines})", file.path.display()));
        }
        if file.path.extension().and_then(|value| value.to_str()) != Some("py") {
            continue;
        }
        let tree = match ast.call_method1("parse", (&file.source, file.path.to_string_lossy())) {
            Ok(tree) => tree,
            Err(error) if error.is_instance_of::<PySyntaxError>(py) => continue,
            Err(error) => return Err(error),
        };
        let nodes = breadth_first(&tree).map_err(|error| {
            PyRuntimeError::new_err(format!(
                "native AST traversal failed for {}: {error}",
                file.relative
            ))
        })?;
        god_functions.extend(
            function_findings(&nodes, &file, &allowed_functions, god_func_lines).map_err(
                |error| {
                    PyRuntimeError::new_err(format!(
                        "native guardrail scan failed for {}: {error}",
                        file.relative
                    ))
                },
            )?,
        );
        fallbacks.extend(
            fallback_findings(&ast, &nodes, &file.relative).map_err(|error| {
                PyRuntimeError::new_err(format!(
                    "native fallback scan failed for {}: {error}",
                    file.relative
                ))
            })?,
        );
    }
    let output = PyList::empty(py);
    for fallback in fallbacks {
        output.append(fallback_to_dict(py, fallback)?)?;
    }
    Ok((
        god_files.len(),
        god_functions.len(),
        god_files,
        god_functions,
        output.unbind(),
    ))
}
