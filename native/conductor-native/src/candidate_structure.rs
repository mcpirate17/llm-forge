//! Batched structural quality facts for candidate review.
//!
//! Reuse one admitted source-bound document; display text is not expression identity.

#[path = "candidate_structure_expression.rs"]
mod expression;
#[path = "candidate_structure_policy.rs"]
mod policy;
use crate::candidate_python::Document;
use expression::{Budget, Work};
pub use policy::evaluate;

use std::collections::{BTreeMap, BTreeSet, HashSet};

use ruff_python_ast as ast;
use ruff_python_ast::visitor::source_order::{
    walk_expr, walk_node, walk_stmt, SourceOrderVisitor, TraversalSignal,
};
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
pub struct UnboundedFact {
    pub name: String,
    pub defined_line: usize,
    pub growth_line: usize,
}

#[derive(Debug, Serialize)]
pub struct LeakFact {
    pub function: String,
    pub resource: String,
    pub acquire: String,
    pub release: String,
    pub line: usize,
    pub owned: bool,
}

#[derive(Debug, Serialize)]
pub struct LockOrderFact {
    pub outer: String,
    pub inner: String,
    pub outer_key: String,
    pub inner_key: String,
    pub line: usize,
}

#[derive(Debug, Serialize)]
pub struct AbstractionFact {
    pub name: String,
    pub line: usize,
    pub methods: usize,
}

#[derive(Debug, Serialize)]
pub struct SubclassFact {
    pub base: String,
    pub implementation: String,
}

#[derive(Debug, Serialize)]
pub struct ConfigFact {
    pub key: String,
    pub line: usize,
    pub default: String,
    pub default_key: String,
}

#[derive(Debug, Serialize)]
pub struct FileFacts {
    pub path: String,
    pub unbounded: Vec<UnboundedFact>,
    pub leaks: Vec<LeakFact>,
    pub lock_orders: Vec<LockOrderFact>,
    pub abstractions: Vec<AbstractionFact>,
    pub subclasses: Vec<SubclassFact>,
    pub configs: Vec<ConfigFact>,
    #[serde(skip)]
    pub canonical_bytes: usize,
}

pub(crate) use crate::candidate_python::Lines;

fn expression_source<'a>(source: &'a str, expression: &ast::Expr) -> Result<&'a str, String> {
    let range = expression.range();
    source
        .get(range.start().to_usize()..range.end().to_usize())
        .ok_or_else(|| "structure expression range is outside its bound source".into())
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

fn config_call(expression: &ast::Expr) -> bool {
    let Some((name, method)) = receiver(expression) else {
        return matches!(expression, ast::Expr::Attribute(attribute)
            if attribute.attr.as_str() == "get"
            && matches!(attribute.value.as_ref(), ast::Expr::Attribute(inner)
                if inner.attr.as_str() == "environ"
                && matches!(inner.value.as_ref(), ast::Expr::Name(name) if name.id.as_str() == "os")));
    };
    name == "os" && method == "getenv"
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
    work: &'a Work,
}

impl<'a> SourceOrderVisitor<'a> for FunctionCollector<'a> {
    fn enter_node(&mut self, _: ast::AnyNodeRef<'a>) -> TraversalSignal {
        if self.work.enter() {
            TraversalSignal::Traverse
        } else {
            TraversalSignal::Skip
        }
    }
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if !self.work.enter() {
            return;
        }
        if let ast::Stmt::FunctionDef(function) = statement {
            self.functions.push(function);
        }
        walk_stmt(self, statement);
    }
}

struct CallStartCollector<'a> {
    starts: HashSet<usize>,
    work: &'a Work,
}

impl<'a> SourceOrderVisitor<'a> for CallStartCollector<'a> {
    fn enter_node(&mut self, _: ast::AnyNodeRef<'a>) -> TraversalSignal {
        if self.work.enter() {
            TraversalSignal::Traverse
        } else {
            TraversalSignal::Skip
        }
    }
    fn visit_expr(&mut self, expression: &'a ast::Expr) {
        if !self.work.enter() {
            return;
        }
        if let ast::Expr::Call(call) = expression {
            self.starts.insert(call.range.start().to_usize());
        }
        walk_expr(self, expression);
    }
}

struct FinallyCollector<'a> {
    guarded_calls: HashSet<usize>,
    work: &'a Work,
}

impl<'a> SourceOrderVisitor<'a> for FinallyCollector<'a> {
    fn enter_node(&mut self, _: ast::AnyNodeRef<'a>) -> TraversalSignal {
        if self.work.enter() {
            TraversalSignal::Traverse
        } else {
            TraversalSignal::Skip
        }
    }
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if !self.work.enter() {
            return;
        }
        if let ast::Stmt::Try(node) = statement {
            if !node.is_star {
                let mut calls = CallStartCollector {
                    starts: HashSet::new(),
                    work: self.work,
                };
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
    work: &'a Work,
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
    fn enter_node(&mut self, _: ast::AnyNodeRef<'a>) -> TraversalSignal {
        if self.work.enter() {
            TraversalSignal::Traverse
        } else {
            TraversalSignal::Skip
        }
    }
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if !self.work.enter() {
            return;
        }
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
        if !self.work.enter() {
            return;
        }
        if let ast::Expr::Call(call) = expression {
            self.record_call(call);
        }
        walk_expr(self, expression);
    }
}

struct LockOrderVisitor<'a> {
    source: &'a str,
    lines: &'a Lines,
    held: Vec<(String, String)>,
    orders: Vec<LockOrderFact>,
    budget: &'a mut Budget,
    work: &'a Work,
    error: Option<String>,
}
impl LockOrderVisitor<'_> {
    fn take(&mut self, node: &ast::StmtWith) -> Result<Vec<(String, String)>, String> {
        let mut taken = Vec::new();
        for item in &node.items {
            let text = self
                .budget
                .text(expression_source(self.source, &item.context_expr)?)?;
            let lowered = text.to_lowercase();
            if LOCK_TOKENS.iter().any(|token| lowered.contains(token)) {
                taken.push((text, self.budget.key(&item.context_expr)?));
            }
        }
        for (outer, outer_key) in &self.held {
            for (inner, inner_key) in &taken {
                if !self.work.enter() {
                    self.work.check()?;
                }
                if outer_key != inner_key {
                    self.budget.fact()?;
                    self.orders.push(LockOrderFact {
                        outer: self.budget.text(outer)?,
                        inner: self.budget.text(inner)?,
                        outer_key: outer_key.clone(),
                        inner_key: inner_key.clone(),
                        line: self.lines.number(node.range.start().to_usize()),
                    });
                }
            }
        }
        Ok(taken)
    }
}
impl<'a> SourceOrderVisitor<'a> for LockOrderVisitor<'a> {
    fn enter_node(&mut self, _: ast::AnyNodeRef<'a>) -> TraversalSignal {
        if self.error.is_none() && self.work.enter() {
            TraversalSignal::Traverse
        } else {
            TraversalSignal::Skip
        }
    }
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if !self.work.enter() {
            return;
        }
        if self.error.is_some() {
            return;
        }
        if let ast::Stmt::With(node) = statement {
            let taken = match self.take(node) {
                Ok(taken) => taken,
                Err(error) => {
                    self.error = Some(error);
                    return;
                }
            };
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
    budget: &'a mut Budget,
    work: &'a Work,
    error: Option<String>,
}

fn base_name(source: &str, expression: &ast::Expr) -> Result<String, String> {
    Ok(match expression {
        ast::Expr::Name(name) => name.id.as_str().to_owned(),
        ast::Expr::Attribute(attribute) => attribute.attr.as_str().to_owned(),
        _ => expression_source(source, expression)?
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned(),
    })
}

fn is_abstract_method(source: &str, function: &ast::StmtFunctionDef) -> Result<bool, String> {
    for decorator in &function.decorator_list {
        if expression_source(source, &decorator.expression)?.contains("abstractmethod") {
            return Ok(true);
        }
    }
    Ok(function.body.len() == 1
        && match &function.body[0] {
            ast::Stmt::Raise(raise) => match raise.exc.as_deref() {
                Some(exception) => {
                    expression_source(source, exception)?.contains("NotImplementedError")
                }
                None => false,
            },
            _ => false,
        })
}

impl ClassConfigVisitor<'_> {
    fn class(&mut self, class: &ast::StmtClassDef) -> Result<(), String> {
        let bases = class
            .arguments
            .as_ref()
            .map(|arguments| {
                arguments
                    .args
                    .iter()
                    .map(|base| base_name(self.source, base))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        for base in &bases {
            self.budget.fact()?;
            self.subclasses.push(SubclassFact {
                base: self.budget.text(base)?,
                implementation: self.budget.text(&format!(
                    "{}::{}",
                    self.path,
                    class.name.as_str()
                ))?,
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
            let mut abstract_method = false;
            for method in &methods {
                abstract_method |= is_abstract_method(self.source, method)?;
            }
            if bases.iter().any(|base| base == "ABC") || abstract_method {
                self.budget.fact()?;
                self.abstractions.push(AbstractionFact {
                    name: self.budget.text(class.name.as_str())?,
                    line: self.lines.number(class.name.range.start().to_usize()),
                    methods: methods.len(),
                });
            }
        }
        Ok(())
    }
    fn call(&mut self, call: &ast::ExprCall) -> Result<(), String> {
        if call.arguments.args.len() >= 2 && config_call(call.func.as_ref()) {
            if let ast::Expr::StringLiteral(key) = &call.arguments.args[0] {
                self.budget.fact()?;
                self.configs.push(ConfigFact {
                    key: self.budget.text(&key.value.to_string())?,
                    line: self.lines.number(call.range.start().to_usize()),
                    default: self
                        .budget
                        .text(expression_source(self.source, &call.arguments.args[1])?)?,
                    default_key: self.budget.key(&call.arguments.args[1])?,
                });
            }
        }
        Ok(())
    }
}
impl<'a> SourceOrderVisitor<'a> for ClassConfigVisitor<'a> {
    fn enter_node(&mut self, _: ast::AnyNodeRef<'a>) -> TraversalSignal {
        if self.error.is_none() && self.work.enter() {
            TraversalSignal::Traverse
        } else {
            TraversalSignal::Skip
        }
    }
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if !self.work.enter() {
            return;
        }
        if self.error.is_some() {
            return;
        }
        if let ast::Stmt::ClassDef(class) = statement {
            if let Err(error) = self.class(class) {
                self.error = Some(error);
                return;
            }
        }
        walk_stmt(self, statement);
    }
    fn visit_expr(&mut self, expression: &'a ast::Expr) {
        if !self.work.enter() {
            return;
        }
        if self.error.is_some() {
            return;
        }
        if let ast::Expr::Call(call) = expression {
            if let Err(error) = self.call(call) {
                self.error = Some(error);
                return;
            }
        }
        walk_expr(self, expression);
    }
}

fn function_facts<'a>(
    function: &'a ast::StmtFunctionDef,
    lines: &'a Lines,
    containers: &'a BTreeSet<String>,
    finally: &'a HashSet<usize>,
    work: &'a Work,
) -> Result<FunctionFactsVisitor<'a>, String> {
    let mut facts = FunctionFactsVisitor {
        lines,
        containers,
        guarded_calls: finally,
        grown: BTreeMap::new(),
        evicted: BTreeSet::new(),
        acquired: BTreeMap::new(),
        released: BTreeMap::new(),
        bound: BTreeSet::new(),
        work,
    };
    walk_node(&mut facts, function.into());
    work.check()?;
    Ok(facts)
}
fn leak_facts(
    function: &ast::StmtFunctionDef,
    facts: &FunctionFactsVisitor<'_>,
    budget: &mut Budget,
    output: &mut Vec<LeakFact>,
) -> Result<(), String> {
    for (resource, (acquire, line)) in &facts.acquired {
        if facts.bound.contains(resource) {
            continue;
        }
        let release = if acquire == "acquire" {
            "release"
        } else {
            "close"
        };
        let mut matching = false;
        let mut guarded = false;
        for (verb, protected) in facts.released.get(resource).into_iter().flatten() {
            if verb == release {
                matching = true;
                guarded |= protected;
            }
        }
        if !matching || guarded {
            continue;
        }
        budget.fact()?;
        output.push(LeakFact {
            function: budget.text(function.name.as_str())?,
            resource: budget.text(resource)?,
            owned: matches!(acquire.as_str(), "acquire" | "open"),
            acquire: acquire.clone(),
            release: release.to_owned(),
            line: *line,
        });
    }
    Ok(())
}
fn lifecycle(
    module: &ast::ModModule,
    lines: &Lines,
    budget: &mut Budget,
    work: &Work,
) -> Result<(Vec<UnboundedFact>, Vec<LeakFact>), String> {
    let containers = module_containers(module, lines);
    let names = containers.keys().cloned().collect::<BTreeSet<_>>();
    let mut functions = FunctionCollector {
        functions: Vec::new(),
        work,
    };
    for statement in &module.body {
        functions.visit_stmt(statement);
    }
    work.check()?;
    let mut grown = BTreeMap::new();
    let mut evicted = BTreeSet::new();
    let mut leaks = Vec::new();
    for function in functions.functions {
        let mut finally = FinallyCollector {
            guarded_calls: HashSet::new(),
            work,
        };
        walk_node(&mut finally, function.into());
        work.check()?;
        let facts = function_facts(function, lines, &names, &finally.guarded_calls, work)?;
        leak_facts(function, &facts, budget, &mut leaks)?;
        for (name, line) in facts.grown {
            grown.entry(name).or_insert(line);
        }
        evicted.extend(facts.evicted);
    }
    let selected = grown
        .keys()
        .filter(|name| !evicted.contains(*name))
        .cloned()
        .collect();
    let correction = expression::growth_lines(module, &selected, lines, work)?;
    let mut unbounded = Vec::new();
    for name in selected {
        let defined_line = *containers
            .get(&name)
            .ok_or("missing structure container definition")?;
        let growth_line = *correction
            .get(&name)
            .ok_or("missing structure growth-line evidence")?;
        budget.fact()?;
        unbounded.push(UnboundedFact {
            name: budget.text(&name)?,
            defined_line,
            growth_line,
        });
    }
    Ok((unbounded, leaks))
}
fn lock_facts(
    source: &str,
    module: &ast::ModModule,
    lines: &Lines,
    budget: &mut Budget,
    work: &Work,
) -> Result<Vec<LockOrderFact>, String> {
    let mut visitor = LockOrderVisitor {
        source,
        lines,
        held: Vec::new(),
        orders: Vec::new(),
        budget,
        work,
        error: None,
    };
    for statement in &module.body {
        visitor.visit_stmt(statement);
    }
    if let Some(error) = visitor.error {
        return Err(error);
    }
    work.check()?;
    Ok(visitor.orders)
}
fn class_facts<'a>(
    path: &'a str,
    source: &'a str,
    module: &'a ast::ModModule,
    lines: &'a Lines,
    budget: &'a mut Budget,
    work: &'a Work,
) -> Result<ClassConfigVisitor<'a>, String> {
    let mut visitor = ClassConfigVisitor {
        path,
        source,
        lines,
        abstractions: Vec::new(),
        subclasses: Vec::new(),
        configs: Vec::new(),
        budget,
        work,
        error: None,
    };
    for statement in &module.body {
        visitor.visit_stmt(statement);
    }
    if let Some(error) = visitor.error.take() {
        return Err(error);
    }
    work.check()?;
    Ok(visitor)
}
fn path_admission(path: &str) -> Result<(), String> {
    if path.len() > 4096
        || path.is_empty()
        || path.chars().any(char::is_control)
        || path.split('/').count() > 128
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err("structure path must be bounded normalized relative UTF8".into());
    }
    Ok(())
}
/// Reuse an admitted AST and its bound source, without parsing or cloning functions.
pub fn analyze_document(
    path: String,
    document: &Document<'_>,
    changed: bool,
) -> Result<FileFacts, String> {
    path_admission(&path)?;
    let source = document.text();
    let module = document.module();
    let lines = Lines::new(source);
    let work = Work::default();
    let mut budget = Budget::default();
    let lock_orders = lock_facts(source, module, &lines, &mut budget, &work)?;
    let ClassConfigVisitor {
        abstractions,
        subclasses,
        configs,
        ..
    } = class_facts(&path, source, module, &lines, &mut budget, &work)?;
    let (unbounded, leaks) = if changed {
        lifecycle(module, &lines, &mut budget, &work)?
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(FileFacts {
        path,
        unbounded,
        leaks,
        lock_orders,
        abstractions,
        subclasses,
        configs,
        canonical_bytes: budget.bytes,
    })
}
#[cfg(test)]
#[path = "candidate_structure_tests.rs"]
mod tests;
