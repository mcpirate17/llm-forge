//! Independent Rust-owned file-family profile reference over Python stdlib AST.

use crate::ast_ref::{self, attr_str, children, dump, is_any, is_kind, kind, walk};
use crate::support::{module, path};
use pyo3::exceptions::{PyOSError, PySyntaxError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyFrozenSet, PyList};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

type Features = BTreeSet<String>;
type Counts = BTreeMap<String, usize>;

fn expanded(prefix: &str, counts: &Counts, cap: usize) -> Features {
    counts
        .iter()
        .flat_map(|(feature, count)| {
            (1..=(*count).min(cap)).map(move |index| format!("{prefix}:{feature}:{index}"))
        })
        .collect()
}

fn normalize_shape(node: &Bound<'_, PyAny>) {
    match kind(node).as_str() {
        "Name" => {
            node.setattr("id", "_name").unwrap();
        }
        "arg" => {
            node.setattr("arg", "_arg").unwrap();
            for child in children(node) {
                normalize_shape(&child);
            }
        }
        "Constant" => {
            let value = node.getattr("value").unwrap();
            node.setattr("value", format!("<const:{}>", kind(&value)))
                .unwrap();
        }
        _ => {
            for child in children(node) {
                normalize_shape(&child);
            }
        }
    }
}

fn statement_shapes(py: Python<'_>, tree: &Bound<'_, PyAny>) -> Features {
    let mut counts = Counts::new();
    let deepcopy = module(py, "copy").getattr("deepcopy").unwrap();
    for node in walk(tree) {
        if !is_any(
            &node,
            &[
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
            ],
        ) {
            continue;
        }
        let normalized = deepcopy.call1((&node,)).unwrap();
        normalize_shape(&normalized);
        let digest = ast_ref::hash_hex(py, "sha1", dump(&normalized).as_bytes());
        *counts.entry(digest[..16].to_owned()).or_default() += 1;
    }
    expanded("stmt", &counts, 4)
}

fn walk_paths(node: &Bound<'_, PyAny>, parent: &str, grandparent: &str, counts: &mut Counts) {
    let node_kind = kind(node);
    if !parent.is_empty() {
        *counts.entry(format!("{parent}>{node_kind}")).or_default() += 1;
    }
    if !grandparent.is_empty() {
        *counts
            .entry(format!("{grandparent}>{parent}>{node_kind}"))
            .or_default() += 1;
    }
    for child in children(node) {
        walk_paths(&child, &node_kind, parent, counts);
    }
}

fn structural_paths(tree: &Bound<'_, PyAny>) -> Features {
    let mut counts = Counts::new();
    walk_paths(tree, "", "", &mut counts);
    expanded("path", &counts, 6)
}

fn signature(node: &Bound<'_, PyAny>) -> (String, String) {
    let args = node.getattr("args").unwrap();
    let count = |name: &str| args.getattr(name).unwrap().cast::<PyList>().unwrap().len();
    let flags = format!(
        "p{}:k{}:v{}:kw{}:a{}",
        count("posonlyargs") + count("args"),
        count("kwonlyargs"),
        usize::from(!args.getattr("vararg").unwrap().is_none()),
        usize::from(!args.getattr("kwarg").unwrap().is_none()),
        usize::from(is_kind(node, "AsyncFunctionDef"))
    );
    (
        format!("api-name:{}:{flags}", attr_str(node, "name")),
        format!("api-shape:{flags}"),
    )
}

fn call_name(node: &Bound<'_, PyAny>) -> Option<String> {
    if is_kind(node, "Name") {
        Some(attr_str(node, "id"))
    } else if is_kind(node, "Attribute") {
        Some(attr_str(node, "attr"))
    } else {
        None
    }
}

fn schemas(tree: &Bound<'_, PyAny>) -> Features {
    let mut features = Features::new();
    for node in walk(tree) {
        if is_kind(&node, "Dict") {
            let mut keys = Vec::new();
            for key in node
                .getattr("keys")
                .unwrap()
                .cast::<PyList>()
                .unwrap()
                .iter()
            {
                if is_kind(&key, "Constant") {
                    if let Ok(value) = key.getattr("value").unwrap().extract::<String>() {
                        keys.push(value);
                    }
                }
            }
            if keys.len() >= 3 {
                keys.sort();
                features.insert(format!("dict:{}", keys.join(",")));
            }
        } else if is_kind(&node, "Call") {
            let mut names = Vec::new();
            for keyword in node
                .getattr("keywords")
                .unwrap()
                .cast::<PyList>()
                .unwrap()
                .iter()
            {
                let arg = keyword.getattr("arg").unwrap();
                if !arg.is_none() {
                    let name: String = arg.extract().unwrap();
                    if !name.is_empty() {
                        names.push(name);
                    }
                }
            }
            if names.len() >= 2 {
                names.sort();
                let name = call_name(&node.getattr("func").unwrap()).unwrap_or_else(|| "?".into());
                features.insert(format!("ctor:{name}:{}", names.join(",")));
            }
        }
    }
    features
}

fn frozen<'py>(py: Python<'py>, values: &Features) -> Bound<'py, PyFrozenSet> {
    PyFrozenSet::new(py, values.iter().cloned()).unwrap()
}

struct ProfileFacts {
    classes: Features,
    functions: Features,
    methods: Features,
    method_hashes: BTreeMap<String, Features>,
    api: Features,
    fields: Features,
    calls: Features,
    imports: Features,
    controls: Counts,
}

impl ProfileFacts {
    fn new() -> Self {
        Self {
            classes: Features::new(),
            functions: Features::new(),
            methods: Features::new(),
            method_hashes: BTreeMap::new(),
            api: Features::new(),
            fields: Features::new(),
            calls: Features::new(),
            imports: Features::new(),
            controls: Counts::new(),
        }
    }
}

fn class_methods(
    py: Python<'_>,
    tree: &Bound<'_, PyAny>,
    facts: &mut ProfileFacts,
) -> HashSet<usize> {
    let mut method_ids = HashSet::new();
    let consolidation = module(py, "conductor.reuse.consolidation");
    for class in walk(tree)
        .into_iter()
        .filter(|node| is_kind(node, "ClassDef"))
    {
        let name = attr_str(&class, "name");
        facts.classes.insert(name.clone());
        facts.api.insert(format!("class:{name}"));
        for node in class
            .getattr("body")
            .unwrap()
            .cast::<PyList>()
            .unwrap()
            .iter()
        {
            if !is_any(&node, &["FunctionDef", "AsyncFunctionDef"]) {
                continue;
            }
            method_ids.insert(node.as_ptr() as usize);
            let name = attr_str(&node, "name");
            facts.methods.insert(name.clone());
            let (named, shaped) = signature(&node);
            facts.api.insert(format!("method:{named}"));
            facts.api.insert(format!("method:{shaped}"));
            let hash: String = consolidation
                .getattr("_normalize_hash")
                .unwrap()
                .call1((&node,))
                .unwrap()
                .get_item(0)
                .unwrap()
                .extract()
                .unwrap();
            facts.method_hashes.entry(name).or_default().insert(hash);
        }
    }
    method_ids
}

fn gather_facts(tree: &Bound<'_, PyAny>, facts: &mut ProfileFacts, method_ids: &HashSet<usize>) {
    for node in walk(tree) {
        if is_any(&node, &["FunctionDef", "AsyncFunctionDef"]) {
            if !method_ids.contains(&(node.as_ptr() as usize)) {
                let name = attr_str(&node, "name");
                facts.functions.insert(name);
                let (named, shaped) = signature(&node);
                facts.api.insert(format!("function:{named}"));
                facts.api.insert(format!("function:{shaped}"));
            }
        } else if is_kind(&node, "Attribute") {
            let owner = node.getattr("value").unwrap();
            if is_kind(&owner, "Name") && ["self", "cls"].contains(&attr_str(&owner, "id").as_str())
            {
                facts
                    .fields
                    .insert(format!("field:{}", attr_str(&node, "attr")));
            }
        } else if is_kind(&node, "Call") {
            if let Some(name) = call_name(&node.getattr("func").unwrap()) {
                facts.calls.insert(format!("call:{name}"));
            }
        } else if is_kind(&node, "Import") {
            for alias in node
                .getattr("names")
                .unwrap()
                .cast::<PyList>()
                .unwrap()
                .iter()
            {
                let name = attr_str(&alias, "name");
                facts
                    .imports
                    .insert(format!("import:{}", name.split('.').next().unwrap()));
            }
        } else if is_kind(&node, "ImportFrom") {
            let module_name = node.getattr("module").unwrap();
            if !module_name.is_none() {
                let name: String = module_name.extract().unwrap();
                if let Some(first) = name.split('.').next().filter(|part| !part.is_empty()) {
                    facts.imports.insert(format!("import:{first}"));
                }
            }
        } else if is_any(
            &node,
            &[
                "If",
                "For",
                "AsyncFor",
                "While",
                "Try",
                "Match",
                "With",
                "AsyncWith",
            ],
        ) {
            *facts.controls.entry(kind(&node)).or_default() += 1;
        }
    }
}

fn construct<'py>(
    py: Python<'py>,
    file: &str,
    source: &str,
    tree: &Bound<'py, PyAny>,
    facts: ProfileFacts,
) -> Bound<'py, PyAny> {
    let options = PyDict::new(py);
    options.set_item("file", file).unwrap();
    options
        .set_item("loc", source.matches('\n').count() + 1)
        .unwrap();
    for (key, values) in [
        ("classes", &facts.classes),
        ("function_names", &facts.functions),
        ("method_names", &facts.methods),
        ("api", &facts.api),
        ("fields", &facts.fields),
        ("calls", &facts.calls),
        ("imports", &facts.imports),
    ] {
        options.set_item(key, frozen(py, values)).unwrap();
    }
    let hashes = PyDict::new(py);
    for (name, values) in &facts.method_hashes {
        hashes.set_item(name, frozen(py, values)).unwrap();
    }
    options.set_item("method_hashes", hashes).unwrap();
    options
        .set_item(
            "control",
            frozen(py, &expanded("control", &facts.controls, 8)),
        )
        .unwrap();
    let structure = structural_paths(tree)
        .union(&statement_shapes(py, tree))
        .cloned()
        .collect();
    options
        .set_item("structure", frozen(py, &structure))
        .unwrap();
    options
        .set_item("schemas", frozen(py, &schemas(tree)))
        .unwrap();
    module(py, "conductor.reuse.file_families")
        .getattr("FileProfile")
        .unwrap()
        .call((), Some(&options))
        .unwrap()
}

pub fn reference_profile<'py>(
    py: Python<'py>,
    source_path: &Path,
    repo: &Path,
) -> Option<Bound<'py, PyAny>> {
    let py_path = path(py, source_path);
    let options = PyDict::new(py);
    options.set_item("encoding", "utf-8").unwrap();
    options.set_item("errors", "replace").unwrap();
    let source: String = match py_path.call_method("read_text", (), Some(&options)) {
        Ok(value) => value.extract().unwrap(),
        Err(error) if error.is_instance_of::<PyOSError>(py) => return None,
        Err(error) => panic!("unexpected profile read failure: {error}"),
    };
    let tree = match ast_ref::parse(py, &source, &source_path.to_string_lossy()) {
        Ok(value) => value,
        Err(error) if error.is_instance_of::<PySyntaxError>(py) => return None,
        Err(error) => panic!("unexpected profile parse failure: {error}"),
    };
    let mut facts = ProfileFacts::new();
    let method_ids = class_methods(py, &tree, &mut facts);
    gather_facts(&tree, &mut facts, &method_ids);
    let file = source_path
        .strip_prefix(repo)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    Some(construct(py, &file, &source, &tree, facts))
}
