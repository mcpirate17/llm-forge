//! Batched structural quality facts for candidate review.
//!
//! Python retains snapshot traversal, finding construction, policy severity, and the
//! small amount of CPython expression normalization needed for stable user-facing text.
//! This module owns the repeated full-tree parse and AST traversal.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use ruff_python_ast as ast;
use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
use ruff_text_size::Ranged;
use serde::Serialize;

const GROWTH_METHODS: &[&str] = &["append", "extend", "add", "update", "setdefault", "insert"];
const EVICTION_METHODS: &[&str] = &["pop", "popitem", "clear", "remove", "discard", "popleft"];
const GROWABLE_FACTORIES: &[&str] = &[
    "dict",
    "list",
    "set",
    "defaultdict",
    "OrderedDict",
    "deque",
    "Counter",
];
const BOUND_KEYWORDS: &[&str] = &["maxlen", "maxsize"];
const LOCK_TOKENS: &[&str] = &["lock", "mutex", "semaphore"];

#[derive(Debug, Serialize)]
struct UnboundedFact {
    name: String,
    defined_line: usize,
    growth_line: usize,
}

#[derive(Debug, Serialize)]
struct LeakFact {
    function: String,
    resource: String,
    acquire: String,
    release: String,
    line: usize,
    owned: bool,
}

#[derive(Debug, Serialize)]
struct LockOrderFact {
    outer: String,
    inner: String,
    line: usize,
}

#[derive(Debug, Serialize)]
struct AbstractionFact {
    name: String,
    line: usize,
    methods: usize,
}

#[derive(Debug, Serialize)]
struct SubclassFact {
    base: String,
    implementation: String,
}

#[derive(Debug, Serialize)]
struct ConfigFact {
    key: String,
    line: usize,
    default: String,
}

#[derive(Debug, Serialize)]
struct FileFacts {
    path: String,
    unbounded: Vec<UnboundedFact>,
    leaks: Vec<LeakFact>,
    lock_orders: Vec<LockOrderFact>,
    abstractions: Vec<AbstractionFact>,
    subclasses: Vec<SubclassFact>,
    configs: Vec<ConfigFact>,
}

#[derive(Debug, Serialize)]
struct AuditFacts {
    modules_indexed: usize,
    files: Vec<FileFacts>,
}

struct Lines {
    starts: Vec<usize>,
}

impl Lines {
    fn new(source: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .as_bytes()
                .iter()
                .enumerate()
                .filter_map(|(index, byte)| (*byte == b'\n').then_some(index + 1)),
        );
        Self { starts }
    }

    fn number(&self, offset: usize) -> usize {
        self.starts.partition_point(|start| *start <= offset)
    }
}

fn expression_source(source: &str, expression: &ast::Expr) -> String {
    let range = expression.range();
    source
        .get(range.start().to_usize()..range.end().to_usize())
        .unwrap_or_default()
        .to_owned()
}

fn receiver(expression: &ast::Expr) -> Option<(&str, &str)> {
    let ast::Expr::Attribute(attribute) = expression else {
        return None;
    };
    let ast::Expr::Name(name) = attribute.value.as_ref() else {
        return None;
    };
    Some((name.id.as_str(), attribute.attr.as_str()))
}

fn expression_path(expression: &ast::Expr) -> Option<String> {
    match expression {
        ast::Expr::Name(name) => Some(name.id.as_str().to_owned()),
        ast::Expr::Attribute(attribute) => Some(format!(
            "{}.{}",
            expression_path(attribute.value.as_ref())?,
            attribute.attr.as_str()
        )),
        _ => None,
    }
}

fn subscript_name<'a>(expression: &'a ast::Expr, names: &BTreeSet<String>) -> Option<&'a str> {
    let ast::Expr::Subscript(subscript) = expression else {
        return None;
    };
    let ast::Expr::Name(name) = subscript.value.as_ref() else {
        return None;
    };
    names.contains(name.id.as_str()).then_some(name.id.as_str())
}

fn is_unbounded_container(expression: &ast::Expr) -> bool {
    match expression {
        ast::Expr::Dict(value) => value.items.is_empty(),
        ast::Expr::List(value) => value.elts.is_empty(),
        ast::Expr::Set(value) => value.elts.is_empty(),
        ast::Expr::Call(call) => {
            let name = match call.func.as_ref() {
                ast::Expr::Attribute(attribute) => attribute.attr.as_str(),
                ast::Expr::Name(name) => name.id.as_str(),
                _ => "",
            };
            GROWABLE_FACTORIES.contains(&name)
                && call.arguments.args.is_empty()
                && !call.arguments.keywords.iter().any(|keyword| {
                    keyword
                        .arg
                        .as_ref()
                        .is_some_and(|name| BOUND_KEYWORDS.contains(&name.as_str()))
                })
        }
        _ => false,
    }
}

fn module_containers(module: &ast::ModModule, lines: &Lines) -> BTreeMap<String, usize> {
    let mut found = BTreeMap::new();
    for statement in &module.body {
        let (targets, value, offset) = match statement {
            ast::Stmt::Assign(assign) => (
                assign.targets.as_slice(),
                Some(assign.value.as_ref()),
                assign.range.start().to_usize(),
            ),
            ast::Stmt::AnnAssign(assign) => (
                std::slice::from_ref(assign.target.as_ref()),
                assign.value.as_deref(),
                assign.range.start().to_usize(),
            ),
            _ => continue,
        };
        let Some(value) = value else { continue };
        if !is_unbounded_container(value) {
            continue;
        }
        for target in targets {
            if let ast::Expr::Name(name) = target {
                found.insert(name.id.as_str().to_owned(), lines.number(offset));
            }
        }
    }
    found
}

struct FunctionCollector<'a> {
    functions: Vec<&'a ast::StmtFunctionDef>,
}

impl<'a> SourceOrderVisitor<'a> for FunctionCollector<'a> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if let ast::Stmt::FunctionDef(function) = statement {
            self.functions.push(function);
        }
        walk_stmt(self, statement);
    }
}

#[derive(Default)]
struct CallStartCollector {
    starts: HashSet<usize>,
}

impl<'a> SourceOrderVisitor<'a> for CallStartCollector {
    fn visit_expr(&mut self, expression: &'a ast::Expr) {
        if let ast::Expr::Call(call) = expression {
            self.starts.insert(call.range.start().to_usize());
        }
        walk_expr(self, expression);
    }
}

#[derive(Default)]
struct FinallyCollector {
    guarded_calls: HashSet<usize>,
}

impl<'a> SourceOrderVisitor<'a> for FinallyCollector {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if let ast::Stmt::Try(node) = statement {
            if !node.is_star {
                let mut calls = CallStartCollector::default();
                for child in &node.finalbody {
                    calls.visit_stmt(child);
                }
                self.guarded_calls.extend(calls.starts);
            }
        }
        walk_stmt(self, statement);
    }
}

struct FunctionFactsVisitor<'a> {
    lines: &'a Lines,
    containers: &'a BTreeSet<String>,
    guarded_calls: &'a HashSet<usize>,
    grown: BTreeMap<String, usize>,
    evicted: BTreeSet<String>,
    acquired: BTreeMap<String, (String, usize)>,
    released: BTreeMap<String, Vec<(String, bool)>>,
    bound: BTreeSet<String>,
}

impl FunctionFactsVisitor<'_> {
    fn record_call(&mut self, call: &ast::ExprCall) {
        if let Some((name, method)) = receiver(call.func.as_ref()) {
            if self.containers.contains(name) {
                if GROWTH_METHODS.contains(&method) {
                    self.grown
                        .entry(name.to_owned())
                        .or_insert_with(|| self.lines.number(call.range.start().to_usize()));
                } else if EVICTION_METHODS.contains(&method) {
                    self.evicted.insert(name.to_owned());
                }
            }
            if method == "acquire" {
                self.acquired.entry(name.to_owned()).or_insert_with(|| {
                    (
                        "acquire".to_owned(),
                        self.lines.number(call.range.start().to_usize()),
                    )
                });
            }
        }
        let verb = match call.func.as_ref() {
            ast::Expr::Attribute(attribute) => attribute.attr.as_str(),
            ast::Expr::Name(name) => name.id.as_str(),
            _ => "",
        };
        if !matches!(verb, "release" | "close") {
            return;
        }
        let guarded = self.guarded_calls.contains(&call.range.start().to_usize());
        if let Some((name, _)) = receiver(call.func.as_ref()) {
            self.released
                .entry(name.to_owned())
                .or_default()
                .push((verb.to_owned(), guarded));
        }
        for argument in &call.arguments.args {
            if let ast::Expr::Name(name) = argument {
                self.released
                    .entry(name.id.as_str().to_owned())
                    .or_default()
                    .push((verb.to_owned(), guarded));
            }
        }
    }
}

impl<'a> SourceOrderVisitor<'a> for FunctionFactsVisitor<'a> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        match statement {
            ast::Stmt::Assign(assign) => {
                for target in &assign.targets {
                    if let Some(name) = subscript_name(target, self.containers) {
                        self.grown
                            .entry(name.to_owned())
                            .or_insert_with(|| self.lines.number(assign.range.start().to_usize()));
                    }
                }
                if assign.targets.len() == 1 {
                    if let (ast::Expr::Name(owner), ast::Expr::Call(call)) =
                        (&assign.targets[0], assign.value.as_ref())
                    {
                        let verb = match call.func.as_ref() {
                            ast::Expr::Attribute(attribute) => attribute.attr.as_str(),
                            ast::Expr::Name(name) => name.id.as_str(),
                            _ => "",
                        };
                        if matches!(verb, "open" | "connect") {
                            self.acquired
                                .entry(owner.id.as_str().to_owned())
                                .or_insert_with(|| {
                                    (
                                        verb.to_owned(),
                                        self.lines.number(assign.range.start().to_usize()),
                                    )
                                });
                        }
                    }
                }
            }
            ast::Stmt::Delete(delete) => {
                for target in &delete.targets {
                    if let Some(name) = subscript_name(target, self.containers) {
                        self.evicted.insert(name.to_owned());
                    }
                }
            }
            ast::Stmt::With(node) => {
                for item in &node.items {
                    match &item.context_expr {
                        ast::Expr::Name(name) => {
                            self.bound.insert(name.id.as_str().to_owned());
                        }
                        ast::Expr::Call(call) => {
                            if let Some((name, _)) = receiver(call.func.as_ref()) {
                                self.bound.insert(name.to_owned());
                            }
                        }
                        _ => {}
                    }
                    if let Some(optional) = &item.optional_vars {
                        if let ast::Expr::Name(name) = optional.as_ref() {
                            self.bound.insert(name.id.as_str().to_owned());
                        }
                    }
                }
            }
            _ => {}
        }
        walk_stmt(self, statement);
    }

    fn visit_expr(&mut self, expression: &'a ast::Expr) {
        if let ast::Expr::Call(call) = expression {
            self.record_call(call);
        }
        walk_expr(self, expression);
    }
}

struct LockOrderVisitor<'a> {
    source: &'a str,
    lines: &'a Lines,
    held: Vec<String>,
    orders: Vec<LockOrderFact>,
}

impl<'a> SourceOrderVisitor<'a> for LockOrderVisitor<'a> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if let ast::Stmt::With(node) = statement {
            let taken = node
                .items
                .iter()
                .map(|item| expression_source(self.source, &item.context_expr))
                .filter(|text| {
                    let lowered = text.to_lowercase();
                    LOCK_TOKENS.iter().any(|token| lowered.contains(token))
                })
                .collect::<Vec<_>>();
            for outer in &self.held {
                for inner in &taken {
                    if outer != inner {
                        self.orders.push(LockOrderFact {
                            outer: outer.clone(),
                            inner: inner.clone(),
                            line: self.lines.number(node.range.start().to_usize()),
                        });
                    }
                }
            }
            let original = self.held.len();
            self.held.extend(taken);
            walk_stmt(self, statement);
            self.held.truncate(original);
            return;
        }
        walk_stmt(self, statement);
    }
}

struct ClassConfigVisitor<'a> {
    path: &'a str,
    source: &'a str,
    lines: &'a Lines,
    abstractions: Vec<AbstractionFact>,
    subclasses: Vec<SubclassFact>,
    configs: Vec<ConfigFact>,
}

fn base_name(source: &str, expression: &ast::Expr) -> String {
    match expression {
        ast::Expr::Name(name) => name.id.as_str().to_owned(),
        ast::Expr::Attribute(attribute) => attribute.attr.as_str().to_owned(),
        _ => expression_source(source, expression)
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned(),
    }
}

fn is_abstract_method(source: &str, function: &ast::StmtFunctionDef) -> bool {
    if function.decorator_list.iter().any(|decorator| {
        expression_source(source, &decorator.expression).contains("abstractmethod")
    }) {
        return true;
    }
    function.body.len() == 1
        && match &function.body[0] {
            ast::Stmt::Raise(raise) => raise.exc.as_deref().is_some_and(|exception| {
                expression_source(source, exception).contains("NotImplementedError")
            }),
            _ => false,
        }
}

impl<'a> SourceOrderVisitor<'a> for ClassConfigVisitor<'a> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if let ast::Stmt::ClassDef(class) = statement {
            let bases = class
                .arguments
                .as_ref()
                .map(|arguments| {
                    arguments
                        .args
                        .iter()
                        .map(|base| base_name(self.source, base))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            for base in &bases {
                self.subclasses.push(SubclassFact {
                    base: base.clone(),
                    implementation: format!("{}::{}", self.path, class.name.as_str()),
                });
            }
            if !bases.iter().any(|base| base == "Protocol") {
                let methods = class
                    .body
                    .iter()
                    .filter_map(|member| match member {
                        ast::Stmt::FunctionDef(function) => Some(function),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                if bases.iter().any(|base| base == "ABC")
                    || methods
                        .iter()
                        .any(|method| is_abstract_method(self.source, method))
                {
                    self.abstractions.push(AbstractionFact {
                        name: class.name.as_str().to_owned(),
                        line: self.lines.number(class.name.range.start().to_usize()),
                        methods: methods.len(),
                    });
                }
            }
        }
        walk_stmt(self, statement);
    }

    fn visit_expr(&mut self, expression: &'a ast::Expr) {
        if let ast::Expr::Call(call) = expression {
            let is_config_call = matches!(
                expression_path(call.func.as_ref()).as_deref(),
                Some("os.getenv") | Some("os.environ.get")
            );
            if call.arguments.args.len() >= 2 && is_config_call {
                if let ast::Expr::StringLiteral(key) = &call.arguments.args[0] {
                    self.configs.push(ConfigFact {
                        key: key.value.to_string(),
                        line: self.lines.number(call.range.start().to_usize()),
                        default: expression_source(self.source, &call.arguments.args[1]),
                    });
                }
            }
        }
        walk_expr(self, expression);
    }
}

fn analyze_file(path: String, source: String, changed: bool) -> Option<FileFacts> {
    let module = ruff_python_parser::parse_module(&source)
        .ok()?
        .into_syntax();
    let lines = Lines::new(&source);
    let containers = module_containers(&module, &lines);
    let container_names = containers.keys().cloned().collect::<BTreeSet<_>>();

    let mut lock_visitor = LockOrderVisitor {
        source: &source,
        lines: &lines,
        held: Vec::new(),
        orders: Vec::new(),
    };
    for statement in &module.body {
        lock_visitor.visit_stmt(statement);
    }

    let mut class_config = ClassConfigVisitor {
        path: &path,
        source: &source,
        lines: &lines,
        abstractions: Vec::new(),
        subclasses: Vec::new(),
        configs: Vec::new(),
    };
    for statement in &module.body {
        class_config.visit_stmt(statement);
    }

    let mut unbounded = Vec::new();
    let mut leaks = Vec::new();
    if changed {
        let mut functions = FunctionCollector {
            functions: Vec::new(),
        };
        for statement in &module.body {
            functions.visit_stmt(statement);
        }
        let mut grown = BTreeMap::new();
        let mut evicted = BTreeSet::new();
        for function in functions.functions {
            let function_statement = ast::Stmt::FunctionDef(function.clone());
            let mut finally = FinallyCollector::default();
            finally.visit_stmt(&function_statement);
            let mut facts = FunctionFactsVisitor {
                lines: &lines,
                containers: &container_names,
                guarded_calls: &finally.guarded_calls,
                grown: BTreeMap::new(),
                evicted: BTreeSet::new(),
                acquired: BTreeMap::new(),
                released: BTreeMap::new(),
                bound: BTreeSet::new(),
            };
            facts.visit_stmt(&function_statement);
            for (name, line) in facts.grown {
                grown.entry(name).or_insert(line);
            }
            evicted.extend(facts.evicted);
            for (resource, (acquire, line)) in facts.acquired {
                if facts.bound.contains(&resource) {
                    continue;
                }
                let release = if acquire == "acquire" {
                    "release"
                } else {
                    "close"
                };
                let matching = facts
                    .released
                    .get(&resource)
                    .into_iter()
                    .flatten()
                    .filter(|(verb, _)| verb == release)
                    .collect::<Vec<_>>();
                if matching.is_empty() || matching.iter().any(|(_, guarded)| *guarded) {
                    continue;
                }
                leaks.push(LeakFact {
                    function: function.name.as_str().to_owned(),
                    resource,
                    owned: matches!(acquire.as_str(), "acquire" | "open"),
                    acquire,
                    release: release.to_owned(),
                    line,
                });
            }
        }
        unbounded = grown
            .into_iter()
            .filter(|(name, _)| !evicted.contains(name))
            .filter_map(|(name, growth_line)| {
                containers.get(&name).map(|defined_line| UnboundedFact {
                    name,
                    defined_line: *defined_line,
                    growth_line,
                })
            })
            .collect();
    }

    let ClassConfigVisitor {
        abstractions,
        subclasses,
        configs,
        ..
    } = class_config;
    Some(FileFacts {
        path,
        unbounded,
        leaks,
        lock_orders: lock_visitor.orders,
        abstractions,
        subclasses,
        configs,
    })
}

#[pyfunction]
fn candidate_structure_facts_native(
    py: Python<'_>,
    records: Vec<(String, String)>,
    changed_paths: Vec<String>,
) -> PyResult<String> {
    let changed = changed_paths.into_iter().collect::<HashSet<_>>();
    let files = py.detach(|| {
        records
            .into_iter()
            .filter_map(|(path, source)| {
                let is_changed = changed.contains(&path);
                analyze_file(path, source, is_changed)
            })
            .collect::<Vec<_>>()
    });
    serde_json::to_string(&AuditFacts {
        modules_indexed: files.len(),
        files,
    })
    .map_err(|error| PyValueError::new_err(error.to_string()))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(candidate_structure_facts_native, module)?)?;
    Ok(())
}
