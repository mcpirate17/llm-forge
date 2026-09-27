//! Independent Rust-owned AST normalizer and collector for reuse contracts.

use crate::ast_ref::{self, ast, attr_str, children, dump, is_any, is_kind, kind, walk};
use crate::support::{module, path};
use pyo3::exceptions::{PyOSError, PySyntaxError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use std::collections::{HashMap, HashSet};
use std::path::Path;

fn is_docstring(node: &Bound<'_, PyAny>) -> bool {
    if !is_kind(node, "Expr") {
        return false;
    }
    let value = node.getattr("value").unwrap();
    is_kind(&value, "Constant") && value.getattr("value").unwrap().extract::<String>().is_ok()
}

fn drop_first_docstring(body: &Bound<'_, PyList>) {
    if !body.is_empty() && is_docstring(&body.get_item(0).unwrap()) {
        body.del_item(0).unwrap();
    }
}

fn without_docstrings(body: &Bound<'_, PyList>) {
    drop_first_docstring(body);
    for statement in body.iter() {
        for child in walk(&statement) {
            if is_any(&child, &["FunctionDef", "AsyncFunctionDef", "ClassDef"]) {
                let nested = child
                    .getattr("body")
                    .unwrap()
                    .cast_into::<PyList>()
                    .unwrap();
                drop_first_docstring(&nested);
            }
        }
    }
}

fn binding_name(node: &Bound<'_, PyAny>, names: &mut HashSet<String>) {
    let kind = kind(node);
    match kind.as_str() {
        "Name" => {
            let context = node.getattr("ctx").unwrap();
            if is_any(&context, &["Store", "Del"]) {
                names.insert(attr_str(node, "id"));
            }
        }
        "arg" => {
            names.insert(attr_str(node, "arg"));
        }
        "FunctionDef" | "AsyncFunctionDef" | "ClassDef" => {
            names.insert(attr_str(node, "name"));
        }
        "Lambda" => {}
        "Import" | "ImportFrom" => {
            for alias in node
                .getattr("names")
                .unwrap()
                .cast::<PyList>()
                .unwrap()
                .iter()
            {
                let asname = alias.getattr("asname").unwrap();
                let name = if asname.is_none() {
                    let value = attr_str(&alias, "name");
                    if kind == "Import" {
                        value.split('.').next().unwrap().to_owned()
                    } else {
                        value
                    }
                } else {
                    asname.extract().unwrap()
                };
                names.insert(name);
            }
        }
        "ExceptHandler" => {
            let name = node.getattr("name").unwrap();
            if !name.is_none() {
                names.insert(name.extract().unwrap());
            }
            for statement in node
                .getattr("body")
                .unwrap()
                .cast::<PyList>()
                .unwrap()
                .iter()
            {
                binding_name(&statement, names);
            }
        }
        _ => {
            for child in children(node) {
                binding_name(&child, names);
            }
        }
    }
}

fn renamed(value: &str, names: &mut HashMap<String, String>) -> String {
    if let Some(found) = names.get(value) {
        return found.clone();
    }
    let next = format!("_v{}", names.len());
    names.insert(value.to_owned(), next.clone());
    next
}

fn rename_locals(
    node: &Bound<'_, PyAny>,
    locals: &HashSet<String>,
    names: &mut HashMap<String, String>,
) {
    match kind(node).as_str() {
        "Name" => {
            let value = attr_str(node, "id");
            if locals.contains(&value) {
                node.setattr("id", renamed(&value, names)).unwrap();
            }
        }
        "arg" => {
            let value = attr_str(node, "arg");
            if locals.contains(&value) {
                node.setattr("arg", renamed(&value, names)).unwrap();
            }
            for child in children(node) {
                rename_locals(&child, locals, names);
            }
        }
        "FunctionDef" | "AsyncFunctionDef" | "ClassDef" => {
            let value = attr_str(node, "name");
            if locals.contains(&value) {
                node.setattr("name", renamed(&value, names)).unwrap();
            }
        }
        "Lambda" => {}
        _ => {
            for child in children(node) {
                rename_locals(&child, locals, names);
            }
        }
    }
}

pub fn reference_normalize(py: Python<'_>, node: &Bound<'_, PyAny>) -> (String, usize) {
    let deepcopy = module(py, "copy").getattr("deepcopy").unwrap();
    let body = deepcopy
        .call1((node.getattr("body").unwrap(),))
        .unwrap()
        .cast_into::<PyList>()
        .unwrap();
    let args = deepcopy.call1((node.getattr("args").unwrap(),)).unwrap();
    without_docstrings(&body);
    let mut locals = HashSet::new();
    for key in ["posonlyargs", "args", "kwonlyargs"] {
        for argument in args.getattr(key).unwrap().cast::<PyList>().unwrap().iter() {
            binding_name(&argument, &mut locals);
        }
    }
    for key in ["vararg", "kwarg"] {
        let argument = args.getattr(key).unwrap();
        if !argument.is_none() {
            binding_name(&argument, &mut locals);
        }
    }
    for statement in body.iter() {
        binding_name(&statement, &mut locals);
    }
    let mut names = HashMap::new();
    rename_locals(&args, &locals, &mut names);
    for statement in body.iter() {
        rename_locals(&statement, &locals, &mut names);
    }
    let wrapper_options = PyDict::new(py);
    wrapper_options.set_item("body", &body).unwrap();
    wrapper_options
        .set_item("type_ignores", PyList::empty(py))
        .unwrap();
    let wrapper = ast(py)
        .getattr("Module")
        .unwrap()
        .call((), Some(&wrapper_options))
        .unwrap();
    let returns = node.getattr("returns").unwrap();
    let ret = if returns.is_none() {
        String::new()
    } else {
        dump(&returns)
    };
    let canonical = [kind(node), dump(&args), dump(&wrapper), ret].join("|");
    let digest = ast_ref::hash_hex(py, "sha1", canonical.as_bytes());
    (digest, walk(&wrapper).len())
}

pub fn reference_collect<'py>(
    py: Python<'py>,
    paths: &[&Path],
    repo: &Path,
    min_lines: usize,
) -> (Bound<'py, PyList>, usize) {
    let records = PyList::empty(py);
    let pathlib = module(py, "pathlib").getattr("Path").unwrap();
    let mut unparsable = 0;
    for source_path in paths {
        let source_file = pathlib
            .call1((source_path.to_string_lossy().as_ref(),))
            .unwrap();
        let read_options = PyDict::new(py);
        read_options.set_item("encoding", "utf-8").unwrap();
        read_options.set_item("errors", "replace").unwrap();
        let source: String = match source_file.call_method("read_text", (), Some(&read_options)) {
            Ok(value) => value.extract().unwrap(),
            Err(error) if error.is_instance_of::<PyOSError>(py) => {
                unparsable += 1;
                continue;
            }
            Err(error) => panic!("unexpected read failure: {error}"),
        };
        let tree = match ast_ref::parse(py, &source, &source_path.to_string_lossy()) {
            Ok(tree) => tree,
            Err(error) if error.is_instance_of::<PySyntaxError>(py) => {
                unparsable += 1;
                continue;
            }
            Err(error) => panic!("unexpected parse failure: {error}"),
        };
        let relative = source_path
            .strip_prefix(repo)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        for node in walk(&tree) {
            if !is_any(&node, &["FunctionDef", "AsyncFunctionDef"]) {
                continue;
            }
            let start = ast_ref::attr_usize(&node, "lineno");
            let ending = node.getattr("end_lineno").unwrap();
            let end: usize = if ending.is_none() {
                start
            } else {
                ending.extract().unwrap()
            };
            if end - start + 1 < min_lines {
                continue;
            }
            let (hash, tokens) = reference_normalize(py, &node);
            let segment = ast(py)
                .getattr("get_source_segment")
                .unwrap()
                .call1((&source, &node))
                .unwrap();
            let text = if segment.is_none() {
                String::new()
            } else {
                segment.extract().unwrap()
            };
            let record = module(py, "conductor.reuse.consolidation")
                .getattr("FuncRecord")
                .unwrap()
                .call1((
                    &relative,
                    start,
                    end,
                    attr_str(&node, "name"),
                    hash,
                    tokens,
                    text,
                ))
                .unwrap();
            records.append(record).unwrap();
        }
    }
    (records, unparsable)
}

pub fn payload<'py>(py: Python<'py>, value: &(Bound<'py, PyList>, usize)) -> Bound<'py, PyAny> {
    let asdict = module(py, "dataclasses").getattr("asdict").unwrap();
    let rows = PyList::empty(py);
    for row in value.0.iter() {
        rows.append(asdict.call1((row,)).unwrap()).unwrap();
    }
    (rows, value.1).into_pyobject(py).unwrap().into_any()
}

pub fn collect_production<'py>(
    py: Python<'py>,
    paths: &[&Path],
    repo: &Path,
    min_lines: usize,
) -> (Bound<'py, PyList>, usize) {
    let py_paths = PyList::new(py, paths.iter().map(|item| path(py, item))).unwrap();
    let result = module(py, "conductor.reuse.consolidation")
        .getattr("collect_functions")
        .unwrap()
        .call1((py_paths, path(py, repo), min_lines))
        .unwrap();
    (
        result.get_item(0).unwrap().cast_into::<PyList>().unwrap(),
        result.get_item(1).unwrap().extract().unwrap(),
    )
}
