//! Batched structural metrics for the Python guardrail audit.
//!
//! Git selection, snapshot reads, decoding, policy thresholds, allowlists, and issue
//! wording remain in Python. This module owns only the repeated parse-and-walk work.

#![allow(clippy::useless_conversion)]

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use ruff_python_ast as ast;
use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct FunctionMetrics {
    symbol: String,
    lineno: usize,
    end_lineno: usize,
    branches: usize,
    max_nesting: usize,
    is_route_registration: bool,
    hot_loop: bool,
}

#[derive(Debug, Serialize)]
struct FileMetrics {
    path: String,
    parse_error: bool,
    functions: Vec<FunctionMetrics>,
}

#[derive(Default)]
struct StructuralFacts {
    branches: usize,
    depth: usize,
    max_nesting: usize,
    hot_loop: bool,
}

#[derive(Default)]
struct LoopFacts {
    has_append_call: bool,
    has_numeric_op: bool,
}

impl<'a> SourceOrderVisitor<'a> for LoopFacts {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if matches!(statement, ast::Stmt::AugAssign(_)) {
            self.has_numeric_op = true;
        }
        walk_stmt(self, statement);
    }

    fn visit_expr(&mut self, expression: &'a ast::Expr) {
        match expression {
            ast::Expr::BinOp(_) => self.has_numeric_op = true,
            ast::Expr::Call(call) => {
                if matches!(call.func.as_ref(), ast::Expr::Attribute(attribute) if attribute.attr.as_str() == "append")
                {
                    self.has_append_call = true;
                }
            }
            _ => {}
        }
        walk_expr(self, expression);
    }
}

impl<'a> SourceOrderVisitor<'a> for StructuralFacts {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if let ast::Stmt::If(node) = statement {
            let elif_count = node
                .elif_else_clauses
                .iter()
                .filter(|clause| clause.test.is_some())
                .count();
            self.branches += 1 + elif_count;
            self.depth += 1;
            self.max_nesting = self.max_nesting.max(self.depth);
            self.visit_expr(&node.test);
            for child in &node.body {
                self.visit_stmt(child);
            }
            for clause in &node.elif_else_clauses {
                if let Some(test) = &clause.test {
                    self.depth += 1;
                    self.max_nesting = self.max_nesting.max(self.depth);
                    self.visit_expr(test);
                }
                for child in &clause.body {
                    self.visit_stmt(child);
                }
            }
            self.depth -= 1 + elif_count;
            return;
        }
        let is_branch = matches!(
            statement,
            ast::Stmt::For(_) | ast::Stmt::While(_) | ast::Stmt::Match(_)
        ) || matches!(statement, ast::Stmt::Try(node) if !node.is_star);
        let is_nesting = is_branch || matches!(statement, ast::Stmt::With(_));
        if is_branch {
            self.branches += 1;
        }
        if let ast::Stmt::For(node) = statement {
            let mut loop_facts = LoopFacts::default();
            walk_stmt(&mut loop_facts, statement);
            let iter_name = match node.iter.as_ref() {
                ast::Expr::Name(name) => name.id.as_str(),
                _ => "",
            };
            self.hot_loop |= loop_facts.has_numeric_op
                && (loop_facts.has_append_call
                    || matches!(
                        iter_name,
                        "x" | "xs" | "arr" | "array" | "tensor" | "values"
                    ));
        }
        if is_nesting {
            self.depth += 1;
            self.max_nesting = self.max_nesting.max(self.depth);
        }
        walk_stmt(self, statement);
        if is_nesting {
            self.depth -= 1;
        }
    }

    fn visit_expr(&mut self, expression: &'a ast::Expr) {
        if matches!(expression, ast::Expr::If(_)) {
            self.branches += 1;
        }
        walk_expr(self, expression);
    }
}

struct FunctionCollector<'a> {
    line_starts: Vec<usize>,
    functions: Vec<FunctionMetrics>,
    marker: std::marker::PhantomData<&'a str>,
}

impl<'a> FunctionCollector<'a> {
    fn line_number(&self, byte_offset: usize) -> usize {
        self.line_starts
            .partition_point(|start| *start <= byte_offset)
    }

    fn function_metrics(
        &self,
        statement: &'a ast::Stmt,
        function: &ast::StmtFunctionDef,
    ) -> FunctionMetrics {
        let mut facts = StructuralFacts::default();
        facts.visit_stmt(statement);
        let lineno = self.line_number(function.name.range.start().to_usize());
        let end_lineno = self.line_number(function.range.end().to_usize());
        let symbol = function.name.as_str().to_owned();
        let is_route_registration = symbol.starts_with("register_")
            && function
                .body
                .iter()
                .any(|statement| matches!(statement, ast::Stmt::FunctionDef(_)));
        FunctionMetrics {
            symbol,
            lineno,
            end_lineno,
            branches: facts.branches,
            max_nesting: facts.max_nesting,
            is_route_registration,
            hot_loop: facts.hot_loop,
        }
    }
}

impl<'a> SourceOrderVisitor<'a> for FunctionCollector<'a> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if let ast::Stmt::FunctionDef(function) = statement {
            self.functions
                .push(self.function_metrics(statement, function));
        }
        walk_stmt(self, statement);
    }
}

fn analyze_file(path: String, source: String) -> FileMetrics {
    let Ok(module) = ruff_python_parser::parse_module(&source) else {
        return FileMetrics {
            path,
            parse_error: true,
            functions: Vec::new(),
        };
    };
    let syntax = module.into_syntax();
    let mut line_starts = vec![0];
    line_starts.extend(
        source
            .as_bytes()
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte == b'\n').then_some(index + 1)),
    );
    let mut collector = FunctionCollector {
        line_starts,
        functions: Vec::new(),
        marker: std::marker::PhantomData,
    };
    for statement in &syntax.body {
        collector.visit_stmt(statement);
    }
    FileMetrics {
        path,
        parse_error: false,
        functions: collector.functions,
    }
}

#[pyfunction]
fn guardrail_ast_metrics_native(
    py: Python<'_>,
    records: Vec<(String, String)>,
) -> PyResult<String> {
    let metrics = py.detach(|| {
        records
            .into_iter()
            .map(|(path, source)| analyze_file(path, source))
            .collect::<Vec<_>>()
    });
    serde_json::to_string(&metrics).map_err(|error| PyValueError::new_err(error.to_string()))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(guardrail_ast_metrics_native, module)?)?;
    Ok(())
}
