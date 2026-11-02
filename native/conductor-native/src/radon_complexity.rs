//! Exact block projection used by Radon6.0.1 cc_visit on CPython3.12.
//! Adapted from Radon ComplexityVisitor, Copyright(c)2012-2017 Michele Lacchia,
//! MIT license; full notice in ../licenses/radon-MIT.txt. No interpreter.
use crate::candidate_python::{Document, Lines};
use ast::token::TokenKind;
use ast::visitor::source_order::{walk_comprehension, walk_expr, walk_stmt, SourceOrderVisitor};
use ruff_python_ast as ast;
use ruff_text_size::Ranged;
use serde::Serialize;
use std::cell::RefCell;
pub const PROTOCOL: &str = "radon-6.0.1-cpython-3.12";
const MAX_BLOCKS: usize = 4096;
const MAX_OUTPUT: usize = 32 * 1024 * 1024;
#[derive(Clone, Debug, Serialize)]
pub struct Block {
    pub key: String,
    pub path: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub line: usize,
    pub complexity: usize,
    pub rank: &'static str,
}
#[derive(Debug, Serialize)]
pub struct Analysis {
    pub protocol: &'static str,
    pub module_complexity: usize,
    pub blocks: Vec<Block>,
}
#[derive(Clone, Copy, Default)]
pub struct Options {
    pub no_assert: bool,
}
/// Defaults are exactly cc_visit(source): assertions count; closures/inner
/// classes do not become rows; decorators/defaults/annotations are not visited.
pub fn analyze(source: &str, path: &str) -> Result<Analysis, String> {
    analyze_with_options(source, path, Options::default())
}
pub fn analyze_with_options(
    source: &str,
    path: &str,
    options: Options,
) -> Result<Analysis, String> {
    if path.len() > 4096 {
        return Err("complexity path label exceeds4096 bytes".into());
    }
    let document = Document::parse_python312(source)
        .map_err(|e| format!("{PROTOCOL}: unsupported or invalid source: {e}"))?;
    let context = Context::new(&document, path, options);
    let mut visitor = Visitor::new(&context, None, true, true, 1);
    for stmt in &document.module().body {
        visitor.visit_stmt(stmt);
    }
    if let Some(error) = &context.budget.borrow().error {
        return Err(error.clone());
    }
    let mut blocks = visitor.functions;
    for (class, methods) in visitor.classes {
        blocks.push(class);
        blocks.extend(methods);
    }
    Ok(Analysis {
        protocol: PROTOCOL,
        module_complexity: visitor.score,
        blocks,
    })
}
pub fn rank(score: usize) -> &'static str {
    match score {
        0..=5 => "A",
        6..=10 => "B",
        11..=20 => "C",
        21..=30 => "D",
        31..=40 => "E",
        _ => "F",
    }
}
#[derive(Default)]
struct Budget {
    blocks: usize,
    bytes: usize,
    error: Option<String>,
}
struct Context<'a> {
    path: &'a str,
    definitions: [Vec<(usize, usize)>; 3],
    options: Options,
    budget: RefCell<Budget>,
}
impl<'a> Context<'a> {
    fn new(document: &Document<'_>, path: &'a str, options: Options) -> Self {
        let lines = Lines::new(document.text());
        let mut definitions: [Vec<(usize, usize)>; 3] = std::array::from_fn(|_| Vec::new());
        for token in document.parsed().tokens() {
            let kind = match token.kind() {
                TokenKind::Def => 0,
                TokenKind::Async => 1,
                TokenKind::Class => 2,
                _ => continue,
            };
            let offset = token.range().start().to_usize();
            definitions[kind].push((offset, lines.number(offset)));
        }
        Self {
            path,
            definitions,
            options,
            budget: RefCell::new(Budget::default()),
        }
    }
    fn line(&self, kind: usize, start: usize, name: usize) -> usize {
        let rows = &self.definitions[kind];
        let index = rows.partition_point(|(offset, _)| *offset < name);
        match index
            .checked_sub(1)
            .and_then(|i| rows.get(i))
            .filter(|(offset, _)| *offset >= start)
        {
            Some((_, line)) => *line,
            None => {
                self.budget.borrow_mut().error =
                    Some("bound definition keyword line unavailable".into());
                0
            }
        }
    }
    fn block(
        &self,
        name: &str,
        class: Option<&str>,
        line: usize,
        score: usize,
        kind: &'static str,
    ) -> Option<Block> {
        let mut budget = self.budget.borrow_mut();
        if budget.error.is_some() {
            return None;
        }
        let width = self
            .path
            .len()
            .saturating_mul(12)
            .saturating_add(name.len().saturating_mul(12))
            .saturating_add(class.map_or(0, str::len).saturating_mul(6))
            .saturating_add(256);
        budget.blocks += 1;
        budget.bytes = budget.bytes.saturating_add(width);
        if budget.blocks > MAX_BLOCKS || budget.bytes > MAX_OUTPUT {
            budget.error =
                Some("Radon block/output amplification exceeds4096 blocks or32MiB".into());
            return None;
        }
        let full = class.map_or_else(|| name.to_owned(), |class| format!("{class}.{name}"));
        Some(Block {
            key: format!("{}::{full}", self.path),
            path: self.path.into(),
            name: name.into(),
            kind,
            line,
            complexity: score,
            rank: rank(score),
        })
    }
}
struct Visitor<'a> {
    context: &'a Context<'a>,
    class: Option<&'a str>,
    functions_enabled: bool,
    classes_enabled: bool,
    score: usize,
    functions: Vec<Block>,
    classes: Vec<(Block, Vec<Block>)>,
}
impl<'a> Visitor<'a> {
    fn new(
        context: &'a Context<'a>,
        class: Option<&'a str>,
        functions: bool,
        classes: bool,
        score: usize,
    ) -> Self {
        Self {
            context,
            class,
            functions_enabled: functions,
            classes_enabled: classes,
            score,
            functions: Vec::new(),
            classes: Vec::new(),
        }
    }
    fn function(&mut self, node: &'a ast::StmtFunctionDef) {
        if !self.functions_enabled {
            return;
        }
        let mut score = 1;
        for child in &node.body {
            let mut visitor = Self::new(self.context, None, false, false, 0);
            visitor.visit_stmt(child);
            score += visitor.score;
        }
        let start = node
            .decorator_list
            .last()
            .map_or(node.range.start(), |d| d.range().end())
            .to_usize();
        let line = self.context.line(
            usize::from(node.is_async),
            start,
            node.name.range().start().to_usize(),
        );
        if let Some(row) = self.context.block(
            &node.name,
            self.class,
            line,
            score,
            if self.class.is_some() { "M" } else { "F" },
        ) {
            self.functions.push(row);
        }
    }
    fn class(&mut self, node: &'a ast::StmtClassDef) {
        if !self.classes_enabled {
            return;
        }
        let mut real = 1;
        let mut methods = Vec::new();
        for child in &node.body {
            let mut visitor = Self::new(self.context, Some(&node.name), true, false, 0);
            visitor.visit_stmt(child);
            real += visitor.score
                + visitor
                    .functions
                    .iter()
                    .map(|m| m.complexity)
                    .sum::<usize>();
            methods.extend(visitor.functions);
        }
        let score = if methods.is_empty() {
            real
        } else {
            real / methods.len() + usize::from(methods.len() > 1)
        };
        let start = node
            .decorator_list
            .last()
            .map_or(node.range.start(), |d| d.range().end())
            .to_usize();
        let line = self
            .context
            .line(2, start, node.name.range().start().to_usize());
        if let Some(row) = self.context.block(&node.name, None, line, score, "C") {
            self.classes.push((row, methods));
        }
    }
}
impl<'a> SourceOrderVisitor<'a> for Visitor<'a> {
    fn visit_stmt(&mut self, node: &'a ast::Stmt) {
        if self.context.budget.borrow().error.is_some() {
            return;
        }
        match node {
            ast::Stmt::FunctionDef(value) => {
                self.function(value);
                return;
            }
            ast::Stmt::ClassDef(value) => {
                self.class(value);
                return;
            }
            ast::Stmt::Assert(_) => {
                self.score += usize::from(!self.context.options.no_assert);
                return;
            }
            ast::Stmt::If(value) => {
                self.score += 1 + value
                    .elif_else_clauses
                    .iter()
                    .filter(|c| c.test.is_some())
                    .count()
            }
            ast::Stmt::For(value) => self.score += 1 + usize::from(!value.orelse.is_empty()),
            ast::Stmt::While(value) => self.score += 1 + usize::from(!value.orelse.is_empty()),
            ast::Stmt::Try(value) if !value.is_star => {
                self.score += value.handlers.len() + usize::from(!value.orelse.is_empty())
            }
            ast::Stmt::Match(value) => {
                let capture = value.cases.iter().any(
                    |case| matches!(&case.pattern,ast::Pattern::MatchAs(p) if p.pattern.is_none()),
                );
                self.score += value.cases.len().saturating_sub(usize::from(capture));
            }
            _ => (),
        }
        walk_stmt(self, node);
    }
    fn visit_expr(&mut self, node: &'a ast::Expr) {
        match node {
            ast::Expr::BoolOp(value) => self.score += value.values.len().saturating_sub(1),
            ast::Expr::If(_) => self.score += 1,
            _ => (),
        }
        walk_expr(self, node);
    }
    fn visit_comprehension(&mut self, node: &'a ast::Comprehension) {
        self.score += 1 + node.ifs.len();
        walk_comprehension(self, node);
    }
}
#[cfg(test)]
#[path = "radon_complexity_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "radon_complexity_reference_tests.rs"]
mod reference_tests;
