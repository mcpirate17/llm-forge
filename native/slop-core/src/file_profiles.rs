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
    literal_data_only: bool,
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

fn literal_value(node: &Bound<'_, PyAny>) -> PyResult<bool> {
    let kind = type_name(node)?;
    if kind == "Constant" {
        return Ok(true);
    }
    if !matches!(kind.as_str(), "List" | "Tuple" | "Set" | "Dict") {
        return Ok(false);
    }
    for child in ast_children(node)? {
        if type_name(&child)? != "Load" && !literal_value(&child)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn literal_module(tree: &Bound<'_, PyAny>) -> PyResult<bool> {
    if type_name(tree)? != "Module" {
        return Ok(false);
    }
    for node in list_items(&tree.getattr("body")?)? {
        match type_name(&node)?.as_str() {
            "Import" | "ImportFrom" => {}
            "Expr" if is_docstring(&node)? => {}
            "Assign" => {
                if !literal_value(&node.getattr("value")?)?
                    || list_items(&node.getattr("targets")?)?
                        .iter()
                        .any(|target| type_name(target).is_ok_and(|kind| kind != "Name"))
                {
                    return Ok(false);
                }
            }
            _ => return Ok(false),
        }
    }
    Ok(true)
}

fn exact_dump(value: &Bound<'_, PyAny>) -> PyResult<String> {
    if is_ast(value)? {
        return value
            .py()
            .import("ast")?
            .call_method1("dump", (value,))?
            .extract();
    }
    if let Ok(items) = value.cast::<PyList>() {
        let parts = items
            .iter()
            .map(|item| exact_dump(&item))
            .collect::<PyResult<Vec<_>>>()?;
        return Ok(format!("[{}]", parts.join(",")));
    }
    py_repr(value)
}

/// Binding fingerprints are context evidence, never a name-resolution proof.
/// Conditional/reassigned bindings deliberately retain every source assignment.
pub(crate) fn module_bindings(
    tree: &Bound<'_, PyAny>,
) -> PyResult<BTreeMap<String, BTreeSet<String>>> {
    let mut output: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut pending = list_items(&tree.getattr("body")?)?;
    while let Some(node) = pending.pop() {
        let kind = type_name(&node)?;
        if matches!(
            kind.as_str(),
            "FunctionDef" | "AsyncFunctionDef" | "ClassDef"
        ) {
            output
                .entry(string_attr(&node, "name")?)
                .or_default()
                .insert(sha1_hex(&exact_dump(&node)?));
            continue;
        }
        if matches!(kind.as_str(), "Import" | "ImportFrom") {
            let fingerprint = exact_dump(&node)?;
            for alias in list_items(&node.getattr("names")?)? {
                let raw = string_attr(&alias, "name")?;
                let name = optional_string_attr(&alias, "asname")?.unwrap_or_else(|| {
                    if kind == "Import" {
                        raw.split('.').next().unwrap_or_default().to_owned()
                    } else {
                        raw
                    }
                });
                output.entry(name).or_default().insert(fingerprint.clone());
            }
            continue;
        }
        if matches!(kind.as_str(), "Assign" | "AnnAssign" | "AugAssign") {
            let targets = if kind == "Assign" {
                list_items(&node.getattr("targets")?)?
            } else {
                vec![node.getattr("target")?]
            };
            let digest = sha1_hex(&exact_dump(&node)?);
            for target in targets {
                for item in descendants(&target)? {
                    if type_name(&item)? == "Name" {
                        output
                            .entry(string_attr(&item, "id")?)
                            .or_default()
                            .insert(digest.clone());
                    }
                }
            }
        }
        pending.extend(ast_children(&node)?);
    }
    Ok(output)
}

fn compact_evidence(value: String) -> String {
    if value.chars().count() <= 160 {
        value
    } else {
        format!(
            "{}… [sha1:{}]",
            value.chars().take(120).collect::<String>(),
            sha1_hex(&value)
        )
    }
}

fn semantic_features(
    node: &Bound<'_, PyAny>,
    bindings: &BTreeMap<String, BTreeSet<String>>,
) -> PyResult<BTreeMap<String, BTreeSet<String>>> {
    let mut evidence: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut locals = BTreeSet::new();
    collect_local_names(&node.getattr("args")?, &mut locals)?;
    for statement in list_items(&node.getattr("body")?)? {
        collect_local_names(&statement, &mut locals)?;
    }
    let nodes = descendants(node)?;
    for item in &nodes {
        if matches!(type_name(item)?.as_str(), "Global" | "Nonlocal") {
            for name in item.getattr("names")?.extract::<Vec<String>>()? {
                locals.remove(&name);
            }
        }
    }
    for item in nodes {
        let kind = type_name(&item)?;
        let feature = match kind.as_str() {
            "Constant" => Some(("literals", compact_evidence(exact_dump(&item)?))),
            "Call" => Some((
                "callees",
                compact_evidence(exact_dump(&item.getattr("func")?)?),
            )),
            "Name" if type_name(&item.getattr("ctx")?)? == "Load" => {
                let name = string_attr(&item, "id")?;
                if !locals.contains(&name) {
                    let values = bindings
                        .get(&name)
                        .cloned()
                        .unwrap_or_else(|| BTreeSet::from(["unresolved".to_owned()]));
                    evidence
                        .entry("global_bindings".to_owned())
                        .or_default()
                        .extend(values.iter().map(|value| format!("{name}:{value}")));
                    Some(("globals", name))
                } else {
                    None
                }
            }
            "Import" | "ImportFrom" => Some(("import_bindings", exact_dump(&item)?)),
            "Add" | "Sub" | "Mult" | "Div" | "FloorDiv" | "Mod" | "Pow" | "MatMult" | "LShift"
            | "RShift" | "BitOr" | "BitXor" | "BitAnd" | "And" | "Or" | "Not" | "Invert"
            | "UAdd" | "USub" | "Eq" | "NotEq" | "Lt" | "LtE" | "Gt" | "GtE" | "Is" | "IsNot"
            | "In" | "NotIn" => Some(("operators", kind.clone())),
            "Await" | "Yield" | "YieldFrom" | "Raise" | "Try" | "Global" | "Nonlocal"
            | "Delete" => Some(("effects", kind.clone())),
            "Attribute" | "Subscript" if type_name(&item.getattr("ctx")?)? == "Store" => {
                Some(("effects", format!("write:{kind}")))
            }
            _ => None,
        };
        if let Some((key, value)) = feature {
            evidence.entry(key.to_owned()).or_default().insert(value);
        }
    }
    for (key, value) in [
        ("signature", node.getattr("args")?),
        ("returns", node.getattr("returns")?),
        ("decorators", node.getattr("decorator_list")?),
    ] {
        evidence
            .entry(key.to_owned())
            .or_default()
            .insert(exact_dump(&value)?);
    }
    let args = node.getattr("args")?;
    evidence.insert(
        "defaults".to_owned(),
        BTreeSet::from([
            exact_dump(&args.getattr("defaults")?)?,
            exact_dump(&args.getattr("kw_defaults")?)?,
        ]),
    );
    Ok(evidence)
}

pub(crate) fn function_profile<'py>(
    node: &Bound<'py, PyAny>,
    bindings: &BTreeMap<String, BTreeSet<String>>,
) -> PyResult<Bound<'py, PyDict>> {
    let py = node.py();
    let mut profile = profile_ast(node, String::new(), "")?;
    profile.api = BTreeSet::from([function_signature(node)?.1]);
    let output = profile_to_dict(py, profile)?;
    for key in [
        "classes",
        "function_names",
        "method_names",
        "method_hashes",
        "loc",
        "literal_data_only",
    ] {
        output.del_item(key)?;
    }
    for key in [
        "api",
        "fields",
        "calls",
        "control",
        "imports",
        "structure",
        "schemas",
    ] {
        let values: BTreeSet<String> = output.get_item(key)?.expect("profile field").extract()?;
        output.set_item(key, values.into_iter().collect::<Vec<_>>())?;
    }
    let body = list_items(&node.getattr("body")?)?
        .into_iter()
        .filter(|item| !is_docstring(item).unwrap_or(false))
        .collect::<Vec<_>>();
    let shaped = body.iter().map(dump_shape).collect::<PyResult<Vec<_>>>()?;
    output.set_item("coarse_shape_hash", sha1_hex(&shaped.join("\n")))?;
    let nodes = body
        .iter()
        .map(descendants)
        .collect::<PyResult<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let meaningful = nodes
        .iter()
        .filter(|item| {
            type_name(item).is_ok_and(|kind| {
                matches!(
                    kind.as_str(),
                    "Call"
                        | "BinOp"
                        | "Compare"
                        | "For"
                        | "AsyncFor"
                        | "While"
                        | "If"
                        | "Match"
                        | "Try"
                        | "With"
                        | "AsyncWith"
                        | "Await"
                        | "Yield"
                )
            })
        })
        .count();
    output.set_item("meaningful_operations", meaningful)?;
    output.set_item("body_statements", body.len())?;
    let semantic: BTreeMap<String, Vec<String>> = semantic_features(node, bindings)?
        .into_iter()
        .map(|(key, values)| (key, values.into_iter().collect()))
        .collect();
    output.set_item("semantic", semantic)?;
    output.set_item(
        "binding_resolution",
        "syntactic_module_context; closures and dynamic bindings unresolved",
    )?;
    Ok(output)
}

fn profile_ast(tree: &Bound<'_, PyAny>, relative: String, source: &str) -> PyResult<FileProfile> {
    let nodes = descendants(tree)?;
    let mut profile = FileProfile {
        file: relative,
        loc: source.matches('\n').count() + 1,
        literal_data_only: literal_module(tree)?,
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

fn registration_imports(nodes: &[Bound<'_, PyAny>]) -> PyResult<Option<BTreeMap<String, String>>> {
    let mut bindings = BTreeMap::new();
    for node in nodes {
        let kind = type_name(node)?;
        if !matches!(kind.as_str(), "Import" | "ImportFrom") {
            continue;
        }
        // Relative imports need a package-root resolver; do not guess one.
        if kind == "ImportFrom" && node.getattr("level")?.extract::<usize>()? != 0 {
            return Ok(None);
        }
        for alias in list_items(&node.getattr("names")?)? {
            let name = string_attr(&alias, "name")?;
            if name == "*" {
                return Ok(None);
            }
            let renamed = optional_string_attr(&alias, "asname")?;
            let (binding, origin) = if kind == "ImportFrom" {
                let module = optional_string_attr(node, "module")?.unwrap_or_default();
                (
                    renamed.unwrap_or_else(|| name.clone()),
                    format!("{module}.{name}"),
                )
            } else if let Some(renamed) = renamed {
                (renamed, name)
            } else {
                let root = name.split('.').next().unwrap_or_default().to_owned();
                (root.clone(), root)
            };
            if bindings.insert(binding, origin).is_some() {
                return Ok(None);
            }
        }
    }
    Ok(Some(bindings))
}

fn imported_expression(
    node: &Bound<'_, PyAny>,
    bindings: &BTreeMap<String, String>,
) -> PyResult<Option<String>> {
    match type_name(node)?.as_str() {
        "Name" => Ok(bindings.get(&string_attr(node, "id")?).cloned()),
        "Attribute" => Ok(imported_expression(&node.getattr("value")?, bindings)?
            .map(|base| string_attr(node, "attr").map(|attr| format!("{base}.{attr}")))
            .transpose()?),
        _ => Ok(None),
    }
}

// Some(true) marks an inline implementation, not interchangeable configuration.
// Only a lambda's body is deferred: calls in its defaults execute at import time.
fn registration_argument(
    node: &Bound<'_, PyAny>,
    bindings: &BTreeMap<String, String>,
) -> PyResult<Option<bool>> {
    let kind = type_name(node)?;
    match kind.as_str() {
        "Constant" => return Ok(Some(false)),
        "Name" | "Attribute" => return Ok(imported_expression(node, bindings)?.map(|_| false)),
        "Lambda" => {
            let args = node.getattr("args")?;
            let mut defaults = list_items(&args.getattr("defaults")?)?;
            defaults.extend(list_items(&args.getattr("kw_defaults")?)?);
            for default in defaults {
                if !default.is_none() && registration_argument(&default, bindings)?.is_none() {
                    return Ok(None);
                }
            }
            return Ok(Some(true));
        }
        "UnaryOp" => {
            return Ok(
                (matches!(type_name(&node.getattr("op")?)?.as_str(), "UAdd" | "USub")
                    && type_name(&node.getattr("operand")?)? == "Constant")
                    .then_some(false),
            );
        }
        "List" | "Tuple" | "Set" | "Dict" => {}
        _ => return Ok(None),
    }
    if kind == "Dict"
        && list_items(&node.getattr("keys")?)?
            .iter()
            .any(|key| key.is_none())
    {
        return Ok(None);
    }
    let mut deferred = false;
    for child in ast_children(node)? {
        if type_name(&child)? == "Load" {
            continue;
        }
        let Some(inline) = registration_argument(&child, bindings)? else {
            return Ok(None);
        };
        deferred |= inline;
    }
    Ok(Some(deferred))
}

fn registration_assignment<'py>(tree: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let mut assignment = None;
    for node in list_items(&tree.getattr("body")?)? {
        match type_name(&node)?.as_str() {
            "Import" | "ImportFrom" if assignment.is_none() => {}
            "Expr" if is_docstring(&node)? => {}
            "Assign" if assignment.is_none() => {
                let targets = list_items(&node.getattr("targets")?)?;
                if targets.len() != 1 || type_name(&targets[0])? != "Name" {
                    return Ok(None);
                }
                assignment = Some(node);
            }
            _ => return Ok(None),
        }
    }
    Ok(assignment)
}

/// Syntactic review evidence only: neither the factory return type nor import,
/// descriptor, or factory side effects are inferred from a naming convention.
fn registration_candidate<'py>(
    tree: &Bound<'py, PyAny>,
    file: &str,
) -> PyResult<Option<Bound<'py, PyDict>>> {
    let Some(assignment) = registration_assignment(tree)? else {
        return Ok(None);
    };
    let Some(bindings) = registration_imports(&list_items(&tree.getattr("body")?)?)? else {
        return Ok(None);
    };
    let target = list_items(&assignment.getattr("targets")?)?.remove(0);
    let name = string_attr(&target, "id")?;
    let call = assignment.getattr("value")?;
    if name.starts_with('_') || bindings.contains_key(&name) || type_name(&call)? != "Call" {
        return Ok(None);
    }
    let Some(factory) = imported_expression(&call.getattr("func")?, &bindings)? else {
        return Ok(None);
    };
    let leaf = factory.rsplit('.').next().unwrap_or_default();
    if !["make_", "create_", "build_"]
        .iter()
        .any(|prefix| leaf.starts_with(prefix))
    {
        return Ok(None);
    }
    let py = tree.py();
    let arguments = PyList::empty(py);
    let keywords = PyList::empty(py);
    let mut deferred = false;
    for arg in list_items(&call.getattr("args")?)? {
        let Some(inline) = registration_argument(&arg, &bindings)? else {
            return Ok(None);
        };
        deferred |= inline;
        arguments.append(exact_dump(&arg)?)?;
    }
    for keyword in list_items(&call.getattr("keywords")?)? {
        let Some(name) = optional_string_attr(&keyword, "arg")? else {
            return Ok(None);
        };
        let value = keyword.getattr("value")?;
        let Some(inline) = registration_argument(&value, &bindings)? else {
            return Ok(None);
        };
        deferred |= inline;
        let item = PyDict::new(py);
        item.set_item("name", name)?;
        item.set_item("value", exact_dump(&value)?)?;
        keywords.append(item)?;
    }
    let output = PyDict::new(py);
    output.set_item("file", file)?;
    output.set_item("name", name)?;
    output.set_item("line_start", assignment.getattr("lineno")?)?;
    output.set_item("line_end", assignment.getattr("end_lineno")?)?;
    output.set_item("factory", factory)?;
    output.set_item("factory_expression", exact_dump(&call.getattr("func")?)?)?;
    output.set_item("import_bindings", bindings)?;
    output.set_item("arguments", arguments)?;
    output.set_item("keywords", keywords)?;
    output.set_item("assignment", exact_dump(&assignment)?)?;
    output.set_item("has_deferred_implementation", deferred)?;
    output.set_item("binding_resolution", "unambiguous_absolute_import_syntax")?;
    output.set_item("callable_return", "unknown")?;
    output.set_item("side_effects", "unknown")?;
    Ok(Some(output))
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
    output.set_item("literal_data_only", profile.literal_data_only)?;
    Ok(output)
}

/// Read files without the GIL, then use CPython only to produce the canonical AST
/// consumed by the native walkers.
#[pyfunction]
#[pyo3(signature = (paths, repo, registration_candidates=None))]
pub(crate) fn audit_file_family_profiles(
    py: Python<'_>,
    paths: Vec<String>,
    repo: String,
    registration_candidates: Option<&Bound<'_, PyList>>,
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
        if let Some(candidates) = registration_candidates {
            if let Some(candidate) = registration_candidate(&tree, &relative)? {
                candidates.append(candidate)?;
            }
        }
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
