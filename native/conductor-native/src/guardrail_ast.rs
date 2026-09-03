//! Batched structural metrics for the Python guardrail audit.
//!
//! Git selection, snapshot reads, decoding, and file-level policy remain in Python.
//! This module owns function parsing, metrics, allowlists, thresholds, and issue rows.

#![allow(clippy::useless_conversion)]

use std::collections::HashSet;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use ruff_python_ast as ast;
use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
    #[serde(skip_serializing_if = "Option::is_none")]
    issues: Option<Vec<PolicyIssue>>,
}

#[derive(Debug, Deserialize)]
struct FunctionPolicy {
    god_functions: HashSet<String>,
    complexity: HashSet<String>,
}

#[derive(Debug, Serialize)]
struct PolicyIssue {
    kind: &'static str,
    severity: &'static str,
    path: String,
    symbol: Option<String>,
    message: String,
    recommendation: &'static str,
    metric: Value,
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

fn split_python_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        if !matches!(
            character,
            '\n' | '\r'
                | '\u{000b}'
                | '\u{000c}'
                | '\u{001c}'
                | '\u{001d}'
                | '\u{001e}'
                | '\u{0085}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            continue;
        }
        lines.push(&text[start..index]);
        let mut end = index + character.len_utf8();
        if character == '\r' {
            if let Some(&(next_index, '\n')) = chars.peek() {
                chars.next();
                end = next_index + 1;
            }
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn function_has_marker(
    lineno: usize,
    end_lineno: usize,
    source_lines: &[&str],
    marker: &str,
) -> bool {
    let needle = format!("# guardrail: {marker}");
    source_lines
        [lineno.saturating_sub(1).min(source_lines.len())..end_lineno.min(source_lines.len())]
        .iter()
        .any(|line| line.contains(&needle))
}

fn policy_issues(
    metrics: &FunctionMetrics,
    source_lines: &[&str],
    rel_path: &str,
    policy: &FunctionPolicy,
) -> Vec<PolicyIssue> {
    let length = metrics
        .end_lineno
        .saturating_sub(metrics.lineno)
        .saturating_add(1);
    let fn_key = if rel_path.is_empty() {
        metrics.symbol.clone()
    } else {
        format!("{rel_path}::{}", metrics.symbol)
    };
    let allow_god_fn = policy.god_functions.contains(&fn_key)
        || function_has_marker(
            metrics.lineno,
            metrics.end_lineno,
            source_lines,
            "allow-god-function",
        );
    let allow_complexity = policy.complexity.contains(&fn_key)
        || function_has_marker(
            metrics.lineno,
            metrics.end_lineno,
            source_lines,
            "allow-complexity",
        );
    let mut issues = Vec::new();
    if length > 100 && !metrics.is_route_registration && !allow_god_fn {
        issues.push(PolicyIssue {
            kind: "god_function",
            severity: "critical",
            path: rel_path.to_owned(),
            symbol: Some(metrics.symbol.clone()),
            message: format!("Function is {length} lines (>100)."),
            recommendation: "Split by decision blocks and side-effect boundaries.",
            metric: json!({"lines": length, "lineno": metrics.lineno}),
        });
    }
    if (metrics.branches > 20 || metrics.max_nesting > 5)
        && !metrics.is_route_registration
        && !allow_complexity
    {
        issues.push(PolicyIssue {
            kind: "complexity",
            severity: "high",
            path: rel_path.to_owned(),
            symbol: Some(metrics.symbol.clone()),
            message: format!(
                "Function complexity is high (branches={}, nesting={}).",
                metrics.branches, metrics.max_nesting
            ),
            recommendation: "Flatten control flow and extract pure helpers.",
            metric: json!({
                "branches": metrics.branches,
                "max_nesting": metrics.max_nesting,
                "lineno": metrics.lineno,
            }),
        });
    }
    if metrics.hot_loop && !allow_complexity {
        issues.push(PolicyIssue {
            kind: "native_hotspot_candidate",
            severity: "high",
            path: rel_path.to_owned(),
            symbol: Some(metrics.symbol.clone()),
            message: "Python loop heuristic suggests a numeric hot path.".to_owned(),
            recommendation: "Vectorize with NumPy/PyTorch or move the hotspot into C/C++/Rust/Cython if profiling confirms it.",
            metric: json!({"lineno": metrics.lineno}),
        });
    }
    issues
}

fn analyze_file(path: String, source: String, policy: Option<&FunctionPolicy>) -> FileMetrics {
    let Ok(module) = ruff_python_parser::parse_module(&source) else {
        return FileMetrics {
            path,
            parse_error: true,
            functions: Vec::new(),
            issues: policy.map(|_| Vec::new()),
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
    let issues = policy.map(|policy| {
        let source_lines = split_python_lines(&source);
        collector
            .functions
            .iter()
            .flat_map(|metrics| policy_issues(metrics, &source_lines, &path, policy))
            .collect()
    });
    FileMetrics {
        path,
        parse_error: false,
        functions: collector.functions,
        issues,
    }
}

#[pyfunction]
#[pyo3(signature = (records, policy_json=None))]
fn guardrail_ast_metrics_native(
    py: Python<'_>,
    records: Vec<(String, String)>,
    policy_json: Option<&str>,
) -> PyResult<String> {
    let policy = policy_json
        .map(serde_json::from_str::<FunctionPolicy>)
        .transpose()
        .map_err(|error| PyValueError::new_err(format!("invalid guardrail policy: {error}")))?;
    let metrics = py.detach(move || {
        records
            .into_iter()
            .map(|(path, source)| analyze_file(path, source, policy.as_ref()))
            .collect::<Vec<_>>()
    });
    serde_json::to_string(&metrics).map_err(|error| PyValueError::new_err(error.to_string()))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(guardrail_ast_metrics_native, module)?)?;
    Ok(())
}
