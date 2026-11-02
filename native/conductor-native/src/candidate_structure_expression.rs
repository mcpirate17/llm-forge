//! Bounded location-free expression identities; retained source is display only.
use ruff_python_ast::{comparable::ComparableExpr, Expr};
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    fmt::{self, Write},
};

const MAX_FORMAT_BYTES: usize = 32 * 1024 * 1024;
const MAX_FACTS: usize = 4096;
const MAX_VISITS: usize = 2_000_000;

#[derive(Default)]
pub(super) struct Work(Cell<usize>);
impl Work {
    pub(super) fn enter(&self) -> bool {
        let next = self.0.get().saturating_add(1);
        self.0.set(next);
        next <= MAX_VISITS
    }
    pub(super) fn check(&self) -> Result<(), String> {
        if self.0.get() > MAX_VISITS {
            Err("structure AST traversal exceeds work limit".into())
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
pub(super) struct Budget {
    pub(super) bytes: usize,
    facts: usize,
}
impl Budget {
    fn charge(&mut self, bytes: usize) -> Result<(), String> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|size| *size <= MAX_FORMAT_BYTES)
            .ok_or("structure expression formatting exceeds 32 MiB")?;
        Ok(())
    }
    pub(super) fn fact(&mut self) -> Result<(), String> {
        if self.facts >= MAX_FACTS {
            return Err("structure facts exceed 4096 per file".into());
        }
        self.facts += 1;
        Ok(())
    }
    pub(super) fn text(&mut self, text: &str) -> Result<String, String> {
        self.charge(text.len())?;
        Ok(text.to_owned())
    }
    pub(super) fn key(&mut self, expression: &Expr) -> Result<String, String> {
        let comparable = ComparableExpr::from(expression);
        let mut writer = HashWriter {
            budget: self,
            hash: Sha256::new(),
        };
        write!(&mut writer, "{comparable:?}")
            .map_err(|_| "structure expression formatting exceeds 32 MiB".to_owned())?;
        Ok(format!("{:x}", writer.hash.finalize()))
    }
}
struct HashWriter<'a> {
    budget: &'a mut Budget,
    hash: Sha256,
}
impl Write for HashWriter<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.budget.charge(text.len()).map_err(|_| fmt::Error)?;
        self.hash.update(text.as_bytes());
        Ok(())
    }
}

// CPython AST breadth-first child shape, used only for legacy growth-line ties.
// Ruff helper wrappers are not all Python AST nodes; the explicit cases below
// preserve Python field order rather than assuming source order is equivalent.
use ast::visitor::source_order::{walk_node, SourceOrderVisitor, TraversalSignal};
use ruff_python_ast as ast;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Copy)]
enum BfsNode<'a> {
    Node(ast::AnyNodeRef<'a>),
    Tail(&'a [ast::ElifElseClause]),
}
fn node(value: ast::AnyNodeRef<'_>) -> BfsNode<'_> {
    BfsNode::Node(value)
}
fn body<'a>(out: &mut Vec<BfsNode<'a>>, statements: &'a [ast::Stmt]) {
    out.extend(statements.iter().map(|s| node(s.into())));
}
fn tail<'a>(out: &mut Vec<BfsNode<'a>>, clauses: &'a [ast::ElifElseClause]) {
    if let Some(first) = clauses.first() {
        if first.test.is_some() {
            out.push(BfsNode::Tail(clauses));
        } else {
            body(out, &first.body);
        }
    }
}
fn parameters(parameters: &ast::Parameters) -> Vec<BfsNode<'_>> {
    let mut out = Vec::new();
    out.extend(
        parameters
            .posonlyargs
            .iter()
            .chain(&parameters.args)
            .map(|p| node((&p.parameter).into())),
    );
    if let Some(p) = &parameters.vararg {
        out.push(node(p.as_ref().into()));
    }
    out.extend(
        parameters
            .kwonlyargs
            .iter()
            .map(|p| node((&p.parameter).into())),
    );
    out.extend(
        parameters
            .kwonlyargs
            .iter()
            .filter_map(|p| p.default.as_deref())
            .map(|p| node(p.into())),
    );
    if let Some(p) = &parameters.kwarg {
        out.push(node(p.as_ref().into()));
    }
    out.extend(
        parameters
            .posonlyargs
            .iter()
            .chain(&parameters.args)
            .filter_map(|p| p.default.as_deref())
            .map(|p| node(p.into())),
    );
    out
}
fn function(function: &ast::StmtFunctionDef) -> Vec<BfsNode<'_>> {
    let mut out = vec![node(function.parameters.as_ref().into())];
    body(&mut out, &function.body);
    out.extend(
        function
            .decorator_list
            .iter()
            .map(|d| node((&d.expression).into())),
    );
    if let Some(returns) = &function.returns {
        out.push(node(returns.as_ref().into()));
    }
    if let Some(types) = &function.type_params {
        out.extend(types.type_params.iter().map(|p| node(p.into())));
    }
    out
}
fn class(class: &ast::StmtClassDef) -> Vec<BfsNode<'_>> {
    let mut out = Vec::new();
    if let Some(arguments) = &class.arguments {
        out.extend(arguments.args.iter().map(|p| node(p.into())));
        out.extend(arguments.keywords.iter().map(|p| node(p.into())));
    }
    body(&mut out, &class.body);
    out.extend(
        class
            .decorator_list
            .iter()
            .map(|d| node((&d.expression).into())),
    );
    if let Some(types) = &class.type_params {
        out.extend(types.type_params.iter().map(|p| node(p.into())));
    }
    out
}
struct Children<'a> {
    root: bool,
    nodes: Vec<BfsNode<'a>>,
    work: &'a Work,
}
impl<'a> SourceOrderVisitor<'a> for Children<'a> {
    fn enter_node(&mut self, node: ast::AnyNodeRef<'a>) -> TraversalSignal {
        if !self.work.enter() {
            return TraversalSignal::Skip;
        }
        if self.root {
            self.root = false;
            return TraversalSignal::Traverse;
        }
        if matches!(
            node,
            ast::AnyNodeRef::Arguments(_)
                | ast::AnyNodeRef::Decorator(_)
                | ast::AnyNodeRef::TypeParams(_)
                | ast::AnyNodeRef::FString(_)
                | ast::AnyNodeRef::TString(_)
        ) {
            return TraversalSignal::Traverse;
        }
        self.nodes.push(BfsNode::Node(node));
        TraversalSignal::Skip
    }
}
fn children<'a>(value: BfsNode<'a>, work: &'a Work) -> Result<Vec<BfsNode<'a>>, String> {
    use ast::AnyNodeRef as N;
    let out = match value {
        BfsNode::Tail(clauses) => {
            let mut out = vec![node(
                clauses[0].test.as_ref().ok_or("invalid elif tail")?.into(),
            )];
            body(&mut out, &clauses[0].body);
            tail(&mut out, &clauses[1..]);
            out
        }
        BfsNode::Node(N::StmtFunctionDef(f)) => function(f),
        BfsNode::Node(N::StmtClassDef(c)) => class(c),
        BfsNode::Node(N::Parameters(p)) => parameters(p),
        BfsNode::Node(N::StmtIf(s)) => {
            let mut out = vec![node(s.test.as_ref().into())];
            body(&mut out, &s.body);
            tail(&mut out, &s.elif_else_clauses);
            out
        }
        BfsNode::Node(N::ExprCall(c)) => {
            let mut out = vec![node(c.func.as_ref().into())];
            out.extend(c.arguments.args.iter().map(|p| node(p.into())));
            out.extend(c.arguments.keywords.iter().map(|p| node(p.into())));
            out
        }
        BfsNode::Node(N::ExprIf(e)) => vec![
            node(e.test.as_ref().into()),
            node(e.body.as_ref().into()),
            node(e.orelse.as_ref().into()),
        ],
        BfsNode::Node(N::ExprDict(e)) => {
            let mut out = e
                .items
                .iter()
                .filter_map(|i| i.key.as_ref())
                .map(|k| node(k.into()))
                .collect::<Vec<_>>();
            out.extend(e.items.iter().map(|i| node((&i.value).into())));
            out
        }
        BfsNode::Node(root) => {
            let mut visitor = Children {
                root: true,
                nodes: Vec::new(),
                work,
            };
            walk_node(&mut visitor, root);
            visitor.nodes
        }
    };
    work.check()?;
    Ok(out)
}
fn bfs<'a>(
    root: ast::AnyNodeRef<'a>,
    work: &'a Work,
    mut visit: impl FnMut(ast::AnyNodeRef<'a>) -> Result<bool, String>,
) -> Result<(), String> {
    let mut queue = VecDeque::from([BfsNode::Node(root)]);
    while let Some(item) = queue.pop_front() {
        if !work.enter() {
            work.check()?;
        }
        if let BfsNode::Node(node) = item {
            if !visit(node)? {
                return Ok(());
            }
        }
        let next = children(item, work)?;
        if queue
            .len()
            .checked_add(next.len())
            .is_none_or(|n| n > 131_072)
        {
            return Err("structure BFS queue exceeds 131072 nodes".into());
        }
        queue.extend(next);
    }
    Ok(())
}
fn growth(
    node: ast::AnyNodeRef<'_>,
    names: &BTreeSet<String>,
    lines: &super::Lines,
    grown: &mut BTreeMap<String, usize>,
) {
    match node {
        ast::AnyNodeRef::ExprCall(call) => {
            if let Some((name, method)) = super::receiver(call.func.as_ref()) {
                if names.contains(name) && super::GROWTH_METHODS.contains(&method) {
                    grown
                        .entry(name.into())
                        .or_insert_with(|| lines.number(call.range.start().to_usize()));
                }
            }
        }
        ast::AnyNodeRef::StmtAssign(assign) => {
            for target in &assign.targets {
                if let Some(name) = super::subscript_name(target, names) {
                    grown
                        .entry(name.into())
                        .or_insert_with(|| lines.number(assign.range.start().to_usize()));
                }
            }
        }
        _ => {}
    }
}
pub(super) fn growth_lines(
    module: &ast::ModModule,
    names: &BTreeSet<String>,
    lines: &super::Lines,
    work: &Work,
) -> Result<BTreeMap<String, usize>, String> {
    let mut grown = BTreeMap::new();
    if names.is_empty() {
        return Ok(grown);
    }
    bfs(module.into(), work, |node| {
        if let ast::AnyNodeRef::StmtFunctionDef(function) = node {
            bfs(function.into(), work, |node| {
                growth(node, names, lines, &mut grown);
                Ok(grown.len() < names.len())
            })?;
        }
        Ok(grown.len() < names.len())
    })?;
    Ok(grown)
}
