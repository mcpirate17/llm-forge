//! `perf/python-tensor-loop`: a Python loop over a tensor or array in a folder
//! the host declared hot.
//!
//! A loop that walks `range(len(x))`, `range(x.shape[0])`, a name bound from a
//! `torch.`/`np.` call, or that indexes such a name or calls `.item()` per
//! iteration runs one interpreter step per element. In training, kernel and
//! evaluation code that is throughput given away, and the fix is a vectorised
//! op or a native kernel, not a tidier loop.
//!
//! Which folders are hot is the host's decision (`[tool.forge.hot_paths]`); a
//! file outside them is never read. The analysis is tree-sitter over each file.
//! The waiver is `# guardrail: allow-python-loop` followed by the reason, on the
//! loop line or the line above it.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use tree_sitter::Node;

use crate::engine::{named, parse};
use crate::style_scan::Finding;

const RULE: &str = "perf/python-tensor-loop";
const ALLOW: &str = "guardrail: allow-python-loop";

/// A call rooted in one of these produces a tensor or an array.
const ARRAY_ROOTS: &[&str] = &["torch", "np", "numpy"];
/// Methods whose result is a Python scalar or list, so the name is not an array.
const SCALAR_METHODS: &[&str] = &["item", "tolist", "size", "numel", "dim", "ndimension"];
/// Methods that pull a value out of an array one iteration at a time.
const PULL_METHODS: &[&str] = &["item", "tolist", "numpy"];
/// Annotations that make a parameter an array without a constructing call.
const ARRAY_TYPES: &[&str] = &[
    "torch.Tensor",
    "Tensor",
    "np.ndarray",
    "numpy.ndarray",
    "ndarray",
];
/// Wrappers that iterate their arguments, so the loop walks what they wrap.
const ITER_WRAPPERS: &[&str] = &[
    "enumerate",
    "zip",
    "reversed",
    "iter",
    "list",
    "tuple",
    "sorted",
];
/// Nodes that are loops (and so get their own verdict) or that open a scope.
const LOOP_KINDS: &[&str] = &[
    "for_statement",
    "while_statement",
    "list_comprehension",
    "set_comprehension",
    "dictionary_comprehension",
    "generator_expression",
];
const SCOPE_KINDS: &[&str] = &["function_definition", "class_definition", "lambda"];

struct Scan<'a> {
    src: &'a str,
    lines: Vec<&'a str>,
    path: &'a str,
    out: Vec<Finding>,
}

impl<'a> Scan<'a> {
    fn text(&self, node: Node) -> &'a str {
        &self.src[node.byte_range()]
    }

    fn waived(&self, line: usize) -> bool {
        let at = |n: usize| n >= 1 && self.lines.get(n - 1).is_some_and(|l| l.contains(ALLOW));
        at(line) || at(line.saturating_sub(1))
    }

    fn report(&mut self, node: Node, why: &str) {
        let line = node.start_position().row + 1;
        if self.waived(line) {
            return;
        }
        self.out.push(Finding {
            path: self.path.to_string(),
            line,
            rule: RULE,
            message: format!(
                "{why}; vectorise it, move it into a native or Triton kernel, or write \
                 down why with `# {ALLOW}`"
            ),
        });
    }

    fn method_name(&self, call: Node) -> Option<&'a str> {
        let callee = call.child_by_field_name("function")?;
        if callee.kind() != "attribute" {
            return None;
        }
        Some(self.text(callee.child_by_field_name("attribute")?))
    }

    /// The identifier a chain of attributes, subscripts and calls starts from.
    fn chain_root(&self, node: Node) -> Option<&'a str> {
        let mut node = node;
        loop {
            node = match node.kind() {
                "identifier" => return Some(self.text(node)),
                "attribute" => node.child_by_field_name("object")?,
                "subscript" => node.child_by_field_name("value")?,
                "call" => node.child_by_field_name("function")?,
                "parenthesized_expression" => *named(node).first()?,
                _ => return None,
            };
        }
    }

    /// True when `expr` is a call chain rooted in torch/numpy that yields an array.
    fn builds_array(&self, expr: Node) -> bool {
        if expr.kind() != "call" {
            return false;
        }
        if self
            .method_name(expr)
            .is_some_and(|m| SCALAR_METHODS.contains(&m))
        {
            return false;
        }
        self.chain_root(expr)
            .is_some_and(|r| ARRAY_ROOTS.contains(&r))
    }

    fn bind_targets(&self, left: Node, names: &mut BTreeSet<String>) {
        match left.kind() {
            "identifier" => {
                names.insert(self.text(left).to_string());
            }
            "pattern_list" | "tuple_pattern" => {
                for child in named(left) {
                    self.bind_targets(child, names);
                }
            }
            _ => {}
        }
    }

    /// Names bound to an array anywhere in this scope, nested scopes excluded.
    fn array_names(&self, scope: Node, names: &mut BTreeSet<String>) {
        let mut stack = named(scope);
        while let Some(node) = stack.pop() {
            if SCOPE_KINDS.contains(&node.kind()) {
                continue;
            }
            if node.kind() == "assignment" {
                self.bind_assignment(node, names);
            }
            stack.extend(named(node));
        }
    }

    fn bind_assignment(&self, node: Node, names: &mut BTreeSet<String>) {
        let Some(left) = node.child_by_field_name("left") else {
            return;
        };
        let annotated = node
            .child_by_field_name("type")
            .is_some_and(|t| ARRAY_TYPES.contains(&self.text(t).trim()));
        let built = node
            .child_by_field_name("right")
            .is_some_and(|r| self.builds_array(r));
        if annotated || built {
            self.bind_targets(left, names);
        }
    }

    fn array_params(&self, func: Node, names: &mut BTreeSet<String>) {
        let Some(params) = func.child_by_field_name("parameters") else {
            return;
        };
        for param in named(params) {
            let Some(annotation) = param.child_by_field_name("type") else {
                continue;
            };
            if !ARRAY_TYPES.contains(&self.text(annotation).trim()) {
                continue;
            }
            let name = param
                .child_by_field_name("name")
                .or_else(|| named(param).into_iter().next());
            if let Some(name) = name.filter(|n| n.kind() == "identifier") {
                names.insert(self.text(name).to_string());
            }
        }
    }

    /// `len(array)`, `x.shape`, `x.size(..)` or `x.numel()` anywhere inside `node`.
    /// `len` of a name this scan cannot show to be an array is a list or a string.
    fn sizes_an_array(&self, node: Node, arrays: &BTreeSet<String>) -> bool {
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            match n.kind() {
                "call" => {
                    let callee = n.child_by_field_name("function");
                    let is_len = callee.is_some_and(|c| self.text(c) == "len")
                        && n.child_by_field_name("arguments")
                            .and_then(|a| named(a).into_iter().next())
                            .and_then(|a| self.chain_root(a))
                            .is_some_and(|r| arrays.contains(r));
                    if is_len
                        || self
                            .method_name(n)
                            .is_some_and(|m| m == "size" || m == "numel")
                    {
                        return true;
                    }
                }
                "attribute" => {
                    let attr = n.child_by_field_name("attribute");
                    if attr.is_some_and(|a| self.text(a) == "shape") {
                        return true;
                    }
                }
                _ => {}
            }
            stack.extend(named(n));
        }
        false
    }

    /// The loop is sized by an array: `range(len(x))`, `range(x.shape[0])`, ...
    fn ranges_over_size(&self, iterable: Node, arrays: &BTreeSet<String>) -> bool {
        let mut stack = vec![iterable];
        while let Some(n) = stack.pop() {
            if n.kind() == "call" {
                let callee = n.child_by_field_name("function").map(|c| self.text(c));
                if matches!(callee, Some("range" | "np.arange" | "torch.arange"))
                    && n.child_by_field_name("arguments")
                        .is_some_and(|a| self.sizes_an_array(a, arrays))
                {
                    return true;
                }
            }
            stack.extend(named(n));
        }
        false
    }

    /// The loop walks an array: the iterable, or what `enumerate`/`zip` wrap, is
    /// rooted in an array name.
    fn walks_array(&self, iterable: Node, arrays: &BTreeSet<String>) -> bool {
        if iterable.kind() == "call" {
            let callee = iterable.child_by_field_name("function");
            if callee.is_some_and(|c| ITER_WRAPPERS.contains(&self.text(c))) {
                let args = iterable.child_by_field_name("arguments");
                return args.is_some_and(|a| named(a).iter().any(|n| self.walks_array(*n, arrays)));
            }
        }
        self.chain_root(iterable)
            .is_some_and(|r| arrays.contains(r))
    }

    /// Per-iteration indexing of an array, or `.item()`/`.tolist()`/`.numpy()`,
    /// in `body`. A nested loop is judged on its own, not charged to this one.
    ///
    /// Indexing counts only when the index names a loop variable (`x[i]`, not
    /// `x[0]`), and a body that calls `.backward()` is a training step, whose
    /// driver loop is the outer loop by design.
    fn pulls_per_iteration(
        &self,
        body: &[Node],
        arrays: &BTreeSet<String>,
        targets: &BTreeSet<String>,
    ) -> Option<&'static str> {
        let mut stack: Vec<Node> = body.to_vec();
        let mut found = None;
        while let Some(n) = stack.pop() {
            if n.kind() == "subscript" {
                let value = n.child_by_field_name("value");
                let root = value.and_then(|v| self.chain_root(v));
                let indexed = targets.is_empty()
                    || named(n).iter().any(|c| {
                        Some(c.id()) != value.map(|v| v.id()) && self.mentions(*c, targets)
                    });
                if indexed && root.is_some_and(|r| arrays.contains(r)) {
                    found = Some("indexes an array once per iteration");
                }
            }
            if n.kind() == "call" {
                let method = self.method_name(n);
                if method == Some("backward") {
                    return None;
                }
                let receiver = n
                    .child_by_field_name("function")
                    .and_then(|c| c.child_by_field_name("object"))
                    .and_then(|o| self.chain_root(o));
                let on_array =
                    receiver.is_some_and(|r| arrays.contains(r) || ARRAY_ROOTS.contains(&r));
                if on_array && method.is_some_and(|m| PULL_METHODS.contains(&m)) {
                    found = Some("copies a value out of an array once per iteration");
                }
            }
            for child in named(n) {
                if !LOOP_KINDS.contains(&child.kind()) && !SCOPE_KINDS.contains(&child.kind()) {
                    stack.push(child);
                }
            }
        }
        found
    }

    fn mentions(&self, node: Node, names: &BTreeSet<String>) -> bool {
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if n.kind() == "identifier" && names.contains(self.text(n)) {
                return true;
            }
            stack.extend(named(n));
        }
        false
    }

    fn steps_in_chunks(&self, iterable: Node) -> bool {
        iterable.kind() == "call"
            && iterable
                .child_by_field_name("function")
                .is_some_and(|c| self.text(c) == "range")
            && iterable
                .child_by_field_name("arguments")
                .is_some_and(|a| named(a).len() >= 3)
    }

    fn judge_header(&self, iterable: Node, arrays: &BTreeSet<String>) -> Option<&'static str> {
        if self.ranges_over_size(iterable, arrays) {
            return Some("loop is sized by an array's length or shape");
        }
        if self.walks_array(iterable, arrays) {
            return Some("loop walks an array element by element");
        }
        None
    }

    fn judge(&mut self, node: Node, arrays: &BTreeSet<String>) {
        let mut body: Vec<Node> = Vec::new();
        let mut why = None;
        match node.kind() {
            "for_statement" => {
                if let Some(right) = node.child_by_field_name("right") {
                    // `range(0, total, step)` walks chunks, not elements: sizing
                    // it by a shape and slicing inside it is how a batch is cut.
                    if !self.steps_in_chunks(right) {
                        why = self.judge_header(right, arrays);
                        body.extend(node.child_by_field_name("body"));
                    }
                }
            }
            "while_statement" => {
                if let Some(cond) = node.child_by_field_name("condition") {
                    if self.sizes_an_array(cond, arrays) {
                        why = Some("loop is bounded by an array's length or shape");
                    }
                }
                body.extend(node.child_by_field_name("body"));
            }
            _ => {
                for clause in named(node) {
                    match clause.kind() {
                        "for_in_clause" => {
                            if let Some(right) = clause.child_by_field_name("right") {
                                why = why.or_else(|| self.judge_header(right, arrays));
                            }
                        }
                        "if_clause" => body.push(clause),
                        _ => {}
                    }
                }
                body.extend(node.child_by_field_name("body"));
            }
        }
        let mut targets = BTreeSet::new();
        let heads: Vec<Node> = if node.kind() == "for_statement" {
            vec![node]
        } else {
            named(node)
                .into_iter()
                .filter(|c| c.kind() == "for_in_clause")
                .collect()
        };
        for head in heads {
            if let Some(left) = head.child_by_field_name("left") {
                self.bind_targets(left, &mut targets);
            }
        }
        let why = why.or_else(|| self.pulls_per_iteration(&body, arrays, &targets));
        if let Some(why) = why {
            self.report(node, &format!("Python loop {why}"));
        }
    }

    fn visit(&mut self, node: Node, arrays: &BTreeSet<String>) {
        let mut scope = arrays.clone();
        let mut inner = None;
        if node.kind() == "function_definition" {
            self.array_params(node, &mut scope);
            if let Some(body) = node.child_by_field_name("body") {
                self.array_names(body, &mut scope);
            }
            inner = Some(&scope);
        }
        let arrays = inner.unwrap_or(arrays);
        if LOOP_KINDS.contains(&node.kind()) {
            self.judge(node, arrays);
        }
        for child in named(node) {
            self.visit(child, arrays);
        }
    }
}

/// Findings for one Python source; `path` is only used to label them.
pub fn scan_source(path: &str, source_text: &str) -> Result<Vec<Finding>, String> {
    let tree =
        parse(source_text).ok_or_else(|| format!("{path}: could not be parsed as Python"))?;
    if tree.root_node().has_error() {
        return Err(format!("{path}: is not valid Python"));
    }
    let mut scan = Scan {
        src: source_text,
        lines: source_text.lines().collect(),
        path,
        out: Vec::new(),
    };
    let mut module_arrays = BTreeSet::new();
    scan.array_names(tree.root_node(), &mut module_arrays);
    scan.visit(tree.root_node(), &module_arrays);
    scan.out.sort();
    Ok(scan.out)
}

/// True when `path` is a `.py` file under one of the `folders`.
pub fn in_hot_path(path: &str, folders: &[String]) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    let test_code = path.split('/').any(|part| part == "tests")
        || file.starts_with("test_")
        || file == "conftest.py";
    path.ends_with(".py")
        && !test_code
        && folders.iter().any(|folder| {
            let folder = folder.trim_end_matches('/');
            path == folder
                || path
                    .strip_prefix(folder)
                    .is_some_and(|rest| rest.starts_with('/'))
        })
}

/// Scan the Python files among `paths` that sit under `hot_paths`.
///
/// `repo`, when given, is stripped from each path so folders and findings are
/// addressed the way the gate names files. A path outside every folder is not read.
#[pyfunction]
#[pyo3(signature = (paths, hot_paths, repo=None))]
pub fn hot_loop_scan_files(
    py: Python<'_>,
    paths: Vec<String>,
    hot_paths: Vec<String>,
    repo: Option<String>,
) -> PyResult<Py<PyList>> {
    let root = repo.map(PathBuf::from);
    let mut findings = Vec::new();
    for raw in paths {
        let path = PathBuf::from(&raw);
        let relative = match root.as_ref() {
            Some(root) => path
                .strip_prefix(root)
                .map_err(|_| {
                    PyValueError::new_err(format!(
                        "hot loop scan path {} is outside repository {}",
                        path.display(),
                        root.display()
                    ))
                })?
                .to_string_lossy()
                .replace('\\', "/"),
            None => path.to_string_lossy().replace('\\', "/"),
        };
        if !in_hot_path(&relative, &hot_paths) {
            continue;
        }
        let bytes = fs::read(&path).map_err(|error| {
            PyValueError::new_err(format!(
                "hot loop scan cannot read {}: {error}",
                path.display()
            ))
        })?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        findings.extend(scan_source(&relative, &text).map_err(PyValueError::new_err)?);
    }
    findings.sort();

    let out = PyList::empty(py);
    for finding in findings {
        let row = PyDict::new(py);
        row.set_item("path", finding.path)?;
        row.set_item("line", finding.line)?;
        row.set_item("rule", finding.rule)?;
        row.set_item("message", finding.message)?;
        out.append(row)?;
    }
    Ok(out.unbind())
}
