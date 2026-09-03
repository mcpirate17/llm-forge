//! Native feature extraction over CPython's canonical AST.
//!
//! Family decisions depend on exact CPython AST topology and `ast.dump` spelling.
//! Rust owns the expensive walks, normalization, hashing, and aggregation while
//! CPython remains the parser of record. This is one coarse batch call.

use pyo3::exceptions::{PyRuntimeError, PySyntaxError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyString};
use sha1::{Digest, Sha1};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;

use crate::python_ast::{
    ast_children, ast_fields, descendants, is_ast, list_items, string_attr, type_name,
};

const STRUCTURE_CAP: usize = 6;
const STATEMENT_CAP: usize = 4;
const CONTROL_CAP: usize = 8;

#[derive(Debug, Default)]
struct FileProfile {
    file: String,
    loc: usize,
    classes: BTreeSet<String>,
    function_names: BTreeSet<String>,
    method_names: BTreeSet<String>,
    method_hashes: BTreeMap<String, BTreeSet<String>>,
    api: BTreeSet<String>,
    fields: BTreeSet<String>,
    calls: BTreeSet<String>,
    control: BTreeSet<String>,
    imports: BTreeSet<String>,
    structure: BTreeSet<String>,
    schemas: BTreeSet<String>,
}

struct LoadedFile {
    path: PathBuf,
    relative: String,
    source: String,
}

fn py_repr(value: &Bound<'_, PyAny>) -> PyResult<String> {
    Ok(value.repr()?.to_string_lossy().into_owned())
}

fn omit_none_field(node: &Bound<'_, PyAny>, field: &str, value: &Bound<'_, PyAny>) -> bool {
    value.is_none()
        && node
            .get_type()
            .getattr(field)
            .is_ok_and(|default| default.is_none())
}

fn repr_string(py: Python<'_>, value: &str) -> PyResult<String> {
    py_repr(&PyString::new(py, value).into_any())
}

fn optional_string_attr(node: &Bound<'_, PyAny>, name: &str) -> PyResult<Option<String>> {
    let value = node.getattr(name)?;
    if value.is_none() {
        Ok(None)
    } else {
        value.extract().map(Some)
    }
}

fn increment(counts: &mut HashMap<String, usize>, key: impl Into<String>) {
    *counts.entry(key.into()).or_default() += 1;
}

fn expanded_features(prefix: &str, counts: HashMap<String, usize>, cap: usize) -> BTreeSet<String> {
    let mut output = BTreeSet::new();
    for (feature, count) in counts {
        for index in 1..=count.min(cap) {
            output.insert(format!("{prefix}:{feature}:{index}"));
        }
    }
    output
}

fn sha1_hex(value: &str) -> String {
    format!("{:x}", Sha1::digest(value.as_bytes()))
}

fn dump_shape_value(value: &Bound<'_, PyAny>) -> PyResult<String> {
    if is_ast(value)? {
        return dump_shape(value);
    }
    if let Ok(items) = value.cast::<PyList>() {
        let dumped = items
            .iter()
            .map(|item| dump_shape_value(&item))
            .collect::<PyResult<Vec<_>>>()?;
        return Ok(format!("[{}]", dumped.join(", ")));
    }
    py_repr(value)
}

/// Exact `ast.dump(..., annotate_fields=False)` spelling after the historical
/// Name/arg/Constant normalizer, without allocating a copied AST.
fn dump_shape(node: &Bound<'_, PyAny>) -> PyResult<String> {
    let kind = type_name(node)?;
    let mut fields = Vec::new();
    let mut keywords = false;
    for (field, value) in ast_fields(node)? {
        if omit_none_field(node, &field, &value) {
            keywords = true;
            continue;
        }
        let dumped = if kind == "Name" && field == "id" {
            "'_name'".to_owned()
        } else if kind == "arg" && field == "arg" {
            "'_arg'".to_owned()
        } else if kind == "Constant" && field == "value" {
            repr_string(value.py(), &format!("<const:{}>", type_name(&value)?))?
        } else {
            dump_shape_value(&value)?
        };
        fields.push(if keywords {
            format!("{field}={dumped}")
        } else {
            dumped
        });
    }
    Ok(format!("{kind}({})", fields.join(", ")))
}

fn structural_paths(root: &Bound<'_, PyAny>) -> PyResult<BTreeSet<String>> {
    let mut counts = HashMap::new();
    let mut stack = vec![(root.clone(), None::<String>, None::<String>)];
    while let Some((node, parent, grandparent)) = stack.pop() {
        let kind = type_name(&node)?;
        if let Some(parent) = parent.as_deref() {
            increment(&mut counts, format!("{parent}>{kind}"));
        }
        if let (Some(grandparent), Some(parent)) = (grandparent.as_deref(), parent.as_deref()) {
            increment(&mut counts, format!("{grandparent}>{parent}>{kind}"));
        }
        let children = ast_children(&node)?;
        stack.extend(
            children
                .into_iter()
                .rev()
                .map(|child| (child, Some(kind.clone()), parent.clone())),
        );
    }
    Ok(expanded_features("path", counts, STRUCTURE_CAP))
}

fn statement_shapes(nodes: &[Bound<'_, PyAny>]) -> PyResult<BTreeSet<String>> {
    let selected = [
        "Assign",
        "AugAssign",
        "Expr",
        "For",
        "AsyncFor",
        "If",
        "Match",
        "Return",
        "Try",
        "While",
        "With",
        "AsyncWith",
    ];
    let mut counts = HashMap::new();
    for node in nodes {
        if selected.contains(&type_name(node)?.as_str()) {
            increment(&mut counts, sha1_hex(&dump_shape(node)?)[..16].to_owned());
        }
    }
    Ok(expanded_features("stmt", counts, STATEMENT_CAP))
}

fn function_signature(node: &Bound<'_, PyAny>) -> PyResult<(String, String)> {
    let args = node.getattr("args")?;
    let positional = list_items(&args.getattr("posonlyargs")?)?.len()
        + list_items(&args.getattr("args")?)?.len();
    let keyword = list_items(&args.getattr("kwonlyargs")?)?.len();
    let flags = format!(
        "p{positional}:k{keyword}:v{}:kw{}:a{}",
        usize::from(!args.getattr("vararg")?.is_none()),
        usize::from(!args.getattr("kwarg")?.is_none()),
        usize::from(type_name(node)? == "AsyncFunctionDef")
    );
    let name = string_attr(node, "name")?;
    Ok((
        format!("api-name:{name}:{flags}"),
        format!("api-shape:{flags}"),
    ))
}

fn call_name(node: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
    let function = node.getattr("func")?;
    match type_name(&function)?.as_str() {
        "Name" => string_attr(&function, "id").map(Some),
        "Attribute" => string_attr(&function, "attr").map(Some),
        _ => Ok(None),
    }
}

fn schema_features(nodes: &[Bound<'_, PyAny>]) -> PyResult<BTreeSet<String>> {
    let mut output = BTreeSet::new();
    for node in nodes {
        match type_name(node)?.as_str() {
            "Dict" => {
                let mut keys = Vec::new();
                for key in list_items(&node.getattr("keys")?)? {
                    if is_ast(&key)? && type_name(&key)? == "Constant" {
                        let value = key.getattr("value")?;
                        if value.is_instance_of::<PyString>() {
                            keys.push(value.extract::<String>()?);
                        }
                    }
                }
                keys.sort();
                if keys.len() >= 3 {
                    output.insert(format!("dict:{}", keys.join(",")));
                }
            }
            "Call" => {
                let mut keywords = Vec::new();
                for keyword in list_items(&node.getattr("keywords")?)? {
                    if let Some(name) = optional_string_attr(&keyword, "arg")? {
                        keywords.push(name);
                    }
                }
                keywords.sort();
                if keywords.len() >= 2 {
                    output.insert(format!(
                        "ctor:{}:{}",
                        call_name(node)?.as_deref().unwrap_or("?"),
                        keywords.join(",")
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(output)
}

fn is_docstring(node: &Bound<'_, PyAny>) -> PyResult<bool> {
    if type_name(node)? != "Expr" {
        return Ok(false);
    }
    let value = node.getattr("value")?;
    Ok(type_name(&value)? == "Constant" && value.getattr("value")?.is_instance_of::<PyString>())
}

fn collect_local_names(node: &Bound<'_, PyAny>, names: &mut BTreeSet<String>) -> PyResult<()> {
    let kind = type_name(node)?;
    match kind.as_str() {
        "Name" => {
            let context = type_name(&node.getattr("ctx")?)?;
            if matches!(context.as_str(), "Store" | "Del") {
                names.insert(string_attr(node, "id")?);
            }
            return Ok(());
        }
        "arg" => {
            names.insert(string_attr(node, "arg")?);
            return Ok(());
        }
        "FunctionDef" | "AsyncFunctionDef" | "ClassDef" => {
            names.insert(string_attr(node, "name")?);
            return Ok(());
        }
        "Lambda" => return Ok(()),
        "Import" | "ImportFrom" => {
            for alias in list_items(&node.getattr("names")?)? {
                let bound = match optional_string_attr(&alias, "asname")? {
                    Some(name) => name,
                    None => string_attr(&alias, "name")?,
                };
                let root = if kind == "Import" {
                    bound.split('.').next().unwrap_or_default()
                } else {
                    bound.as_str()
                };
                names.insert(root.to_owned());
            }
            return Ok(());
        }
        "ExceptHandler" => {
            if let Some(name) = optional_string_attr(node, "name")? {
                names.insert(name);
            }
            for statement in list_items(&node.getattr("body")?)? {
                collect_local_names(&statement, names)?;
            }
            return Ok(());
        }
        _ => {}
    }
    for child in ast_children(node)? {
        collect_local_names(&child, names)?;
    }
    Ok(())
}

struct MethodDump {
    locals: BTreeSet<String>,
    placeholders: BTreeMap<String, usize>,
}

impl MethodDump {
    fn placeholder(&mut self, value: String) -> String {
        let next = self.placeholders.len();
        let index = *self.placeholders.entry(value).or_insert(next);
        format!("_v{index}")
    }

    fn dump_value(&mut self, value: &Bound<'_, PyAny>, normalize: bool) -> PyResult<String> {
        if is_ast(value)? {
            return self.dump(value, normalize);
        }
        if value.cast::<PyList>().is_ok() {
            return self.dump_list(value, normalize, false);
        }
        py_repr(value)
    }

    fn dump_list(
        &mut self,
        value: &Bound<'_, PyAny>,
        normalize: bool,
        strip_first_docstring: bool,
    ) -> PyResult<String> {
        let mut items = list_items(value)?;
        if strip_first_docstring
            && items
                .first()
                .is_some_and(|item| is_docstring(item).unwrap_or(false))
        {
            items.remove(0);
        }
        let dumped = items
            .into_iter()
            .map(|item| self.dump_value(&item, normalize))
            .collect::<PyResult<Vec<_>>>()?;
        Ok(format!("[{}]", dumped.join(", ")))
    }

    fn dump(&mut self, node: &Bound<'_, PyAny>, normalize: bool) -> PyResult<String> {
        let kind = type_name(node)?;
        let nested_scope = matches!(
            kind.as_str(),
            "FunctionDef" | "AsyncFunctionDef" | "ClassDef"
        );
        let lambda = kind == "Lambda";
        let mut fields = Vec::new();
        let mut keywords = false;
        for (field, value) in ast_fields(node)? {
            if omit_none_field(node, &field, &value) {
                keywords = true;
                continue;
            }
            let active = normalize && !lambda;
            let normalizes_identifier = active
                && ((matches!(kind.as_str(), "Name" | "arg")
                    && matches!(field.as_str(), "id" | "arg"))
                    || (nested_scope && field == "name"));
            let dumped = if normalizes_identifier {
                let raw: String = value.extract()?;
                if self.locals.contains(&raw) {
                    repr_string(value.py(), &self.placeholder(raw))?
                } else {
                    py_repr(&value)?
                }
            } else if value.cast::<PyList>().is_ok() {
                self.dump_list(
                    &value,
                    active && !nested_scope,
                    field == "body" && nested_scope,
                )?
            } else {
                self.dump_value(&value, active && !nested_scope)?
            };
            fields.push(if keywords {
                format!("{field}={dumped}")
            } else {
                dumped
            });
        }
        Ok(format!("{kind}({})", fields.join(", ")))
    }
}

fn method_canonical(node: &Bound<'_, PyAny>) -> PyResult<String> {
    let args = node.getattr("args")?;
    let body = node.getattr("body")?;
    let mut locals = BTreeSet::new();
    for field in ["posonlyargs", "args", "kwonlyargs"] {
        for arg in list_items(&args.getattr(field)?)? {
            collect_local_names(&arg, &mut locals)?;
        }
    }
    for field in ["vararg", "kwarg"] {
        let arg = args.getattr(field)?;
        if !arg.is_none() {
            collect_local_names(&arg, &mut locals)?;
        }
    }
    for statement in list_items(&body)? {
        collect_local_names(&statement, &mut locals)?;
    }
    let mut dumper = MethodDump {
        locals,
        placeholders: BTreeMap::new(),
    };
    let args_dump = dumper.dump(&args, true)?;
    let body_dump = dumper.dump_list(&body, true, true)?;
    let returns = node.getattr("returns")?;
    let return_dump = if returns.is_none() {
        String::new()
    } else {
        dumper.dump(&returns, false)?
    };
    Ok(format!(
        "{}|{args_dump}|Module({body_dump}, [])|{return_dump}",
        type_name(node)?
    ))
}

pub(crate) fn normalized_function_hash(node: &Bound<'_, PyAny>) -> PyResult<String> {
    Ok(sha1_hex(&method_canonical(node)?))
}

fn profile_ast(tree: &Bound<'_, PyAny>, relative: String, source: &str) -> PyResult<FileProfile> {
    let nodes = descendants(tree)?;
    let mut profile = FileProfile {
        file: relative,
        loc: source.matches('\n').count() + 1,
        ..FileProfile::default()
    };
    let mut method_ids = BTreeSet::new();

    for class_node in nodes
        .iter()
        .filter(|node| type_name(node).is_ok_and(|kind| kind == "ClassDef"))
    {
        let name = string_attr(class_node, "name")?;
        profile.classes.insert(name.clone());
        profile.api.insert(format!("class:{name}"));
        for method in list_items(&class_node.getattr("body")?)? {
            if !matches!(
                type_name(&method)?.as_str(),
                "FunctionDef" | "AsyncFunctionDef"
            ) {
                continue;
            }
            method_ids.insert(method.as_ptr() as usize);
            let name = string_attr(&method, "name")?;
            profile.method_names.insert(name.clone());
            let (named, shaped) = function_signature(&method)?;
            profile.api.insert(format!("method:{named}"));
            profile.api.insert(format!("method:{shaped}"));
            let digest = normalized_function_hash(&method).map_err(|error| {
                PyRuntimeError::new_err(format!("method hash failed for {name}: {error}"))
            })?;
            profile
                .method_hashes
                .entry(name)
                .or_default()
                .insert(digest);
        }
    }

    let mut control_counts = HashMap::new();
    for node in &nodes {
        let kind = type_name(node)?;
        match kind.as_str() {
            "FunctionDef" | "AsyncFunctionDef"
                if !method_ids.contains(&(node.as_ptr() as usize)) =>
            {
                profile.function_names.insert(string_attr(node, "name")?);
                let (named, shaped) = function_signature(node)?;
                profile.api.insert(format!("function:{named}"));
                profile.api.insert(format!("function:{shaped}"));
            }
            "Attribute" => {
                let value = node.getattr("value")?;
                if type_name(&value)? == "Name"
                    && matches!(string_attr(&value, "id")?.as_str(), "self" | "cls")
                {
                    profile
                        .fields
                        .insert(format!("field:{}", string_attr(node, "attr")?));
                }
            }
            "Call" => {
                if let Some(name) = call_name(node)? {
                    profile.calls.insert(format!("call:{name}"));
                }
            }
            "Import" => {
                for alias in list_items(&node.getattr("names")?)? {
                    let name = string_attr(&alias, "name")?;
                    if let Some(root) = name.split('.').next() {
                        profile.imports.insert(format!("import:{root}"));
                    }
                }
            }
            "ImportFrom" => {
                if let Some(module) = optional_string_attr(node, "module")? {
                    if let Some(root) = module.split('.').next() {
                        if !root.is_empty() {
                            profile.imports.insert(format!("import:{root}"));
                        }
                    }
                }
            }
            "If" | "For" | "AsyncFor" | "While" | "Try" | "Match" | "With" | "AsyncWith" => {
                increment(&mut control_counts, kind)
            }
            _ => {}
        }
    }

    profile.control = expanded_features("control", control_counts, CONTROL_CAP);
    profile.structure = structural_paths(tree)?;
    profile.structure.extend(statement_shapes(&nodes)?);
    profile.schemas = schema_features(&nodes)?;
    Ok(profile)
}

fn load_files(paths: Vec<String>, repo: String) -> Result<(Vec<LoadedFile>, usize), String> {
    let repo = PathBuf::from(repo);
    let mut loaded = Vec::with_capacity(paths.len());
    let mut unreadable = 0;
    for raw_path in paths {
        let path = PathBuf::from(raw_path);
        let relative = path
            .strip_prefix(&repo)
            .map_err(|_| {
                format!(
                    "profile path {} is outside repository {}",
                    path.display(),
                    repo.display()
                )
            })?
            .to_string_lossy()
            .replace('\\', "/");
        match fs::read(&path) {
            Ok(bytes) => loaded.push(LoadedFile {
                path,
                relative,
                source: String::from_utf8_lossy(&bytes).into_owned(),
            }),
            Err(_) => unreadable += 1,
        }
    }
    Ok((loaded, unreadable))
}

fn profile_to_dict<'py>(py: Python<'py>, profile: FileProfile) -> PyResult<Bound<'py, PyDict>> {
    let output = PyDict::new(py);
    output.set_item("file", profile.file)?;
    output.set_item("loc", profile.loc)?;
    output.set_item("classes", profile.classes)?;
    output.set_item("function_names", profile.function_names)?;
    output.set_item("method_names", profile.method_names)?;
    output.set_item("method_hashes", profile.method_hashes)?;
    output.set_item("api", profile.api)?;
    output.set_item("fields", profile.fields)?;
    output.set_item("calls", profile.calls)?;
    output.set_item("control", profile.control)?;
    output.set_item("imports", profile.imports)?;
    output.set_item("structure", profile.structure)?;
    output.set_item("schemas", profile.schemas)?;
    Ok(output)
}

/// Read files without the GIL, then use CPython only to produce the canonical AST
/// consumed by the native walkers.
#[pyfunction]
pub(crate) fn audit_file_family_profiles(
    py: Python<'_>,
    paths: Vec<String>,
    repo: String,
) -> PyResult<(Py<PyList>, usize)> {
    let (files, mut unparsable) = py
        .detach(move || load_files(paths, repo))
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
    let ast = py.import("ast")?;
    let output = PyList::empty(py);
    for file in files {
        let tree = match ast.call_method1("parse", (&file.source, file.path.to_string_lossy())) {
            Ok(tree) => tree,
            Err(error) if error.is_instance_of::<PySyntaxError>(py) => {
                unparsable += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        let relative = file.relative;
        let profile = profile_ast(&tree, relative.clone(), &file.source).map_err(|error| {
            PyRuntimeError::new_err(format!(
                "native profile extraction failed for {relative}: {error}"
            ))
        })?;
        output.append(profile_to_dict(py, profile)?)?;
    }
    Ok((output.unbind(), unparsable))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_dump_matches_python_spelling() {
        Python::initialize();
        Python::attach(|py| {
            let ast = py.import("ast").unwrap();
            let tree = ast
                .call_method1("parse", ("value = call(name, enabled=True)\n",))
                .unwrap();
            let mut body = list_items(&tree.getattr("body").unwrap()).unwrap();
            let actual = dump_shape(&body.remove(0)).unwrap();
            assert_eq!(
                actual,
                "Assign([Name('_name', Store())], Call(Name('_name', Load()), [Name('_name', Load())], [keyword('enabled', Constant('<const:bool>'))]))"
            );
        });
    }
}
