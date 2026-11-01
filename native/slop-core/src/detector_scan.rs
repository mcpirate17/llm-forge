//! Native Python guardrails. Ruff parses source; no CPython AST or runtime is used.
use crate::style_scan::Finding;
use ruff_python_ast as ast;
use ruff_python_ast::visitor::source_order::{
    walk_except_handler, walk_expr, walk_stmt, SourceOrderVisitor,
};
use ruff_text_size::Ranged;
use serde::Serialize;
use std::collections::BTreeSet;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const ALLOW_FALLBACK: &str = "guardrail: allow-fallback";

#[derive(Debug, Serialize)]
pub struct Fallback {
    pub relative: String,
    pub line: usize,
    pub end_line: usize,
    pub exception: String,
    pub severity: &'static str,
    pub confidence: f64,
    pub value: usize,
}

#[derive(Default, Debug, Serialize)]
pub struct Scan {
    pub god_files: Vec<String>,
    pub god_functions: Vec<String>,
    pub fallbacks: Vec<Fallback>,
    pub unparsable: Vec<String>,
}

pub struct Policy<'a> {
    pub allowed_files: &'a BTreeSet<String>,
    pub allowed_functions: &'a BTreeSet<String>,
    pub god_file_lines: usize,
    pub god_func_lines: usize,
}

/// Caller-selected regular UTF-8 source, bounded before parsing. Missing files are errors.
pub fn read_source(path: &Path) -> Result<String, String> {
    let read = || -> std::io::Result<String> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() > MAX_SOURCE_BYTES as u64 {
            return Err(std::io::Error::other(
                "source must be a regular file <= 1 MiB",
            ));
        }
        let mut source = String::new();
        file.take(MAX_SOURCE_BYTES as u64 + 1)
            .read_to_string(&mut source)?;
        if source.len() > MAX_SOURCE_BYTES || source.contains('\0') {
            return Err(std::io::Error::other("source exceeds size/NUL bound"));
        }
        Ok(source)
    };
    read().map_err(|e| format!("detector cannot read {}: {e}", path.display()))
}

fn line_starts(source: &str) -> Vec<usize> {
    let bytes = source.as_bytes();
    let mut starts = vec![0];
    starts.extend(bytes.iter().enumerate().filter_map(|(i, &b)| {
        (b == b'\n' || (b == b'\r' && bytes.get(i + 1) != Some(&b'\n'))).then_some(i + 1)
    }));
    starts
}

fn source_lines(source: &str) -> Vec<&str> {
    let starts = line_starts(source);
    starts
        .iter()
        .enumerate()
        .map(|(i, &start)| &source[start..starts.get(i + 1).copied().unwrap_or(source.len())])
        .collect()
}

struct Facts<'a> {
    source: &'a str,
    relative: &'a str,
    line_starts: Vec<usize>,
    functions: Vec<(String, usize, usize, bool)>,
    fallbacks: Vec<Fallback>,
}

impl Facts<'_> {
    fn line(&self, byte: usize) -> usize {
        self.line_starts.partition_point(|start| *start <= byte)
    }

    fn end_line(&self, node: &impl Ranged) -> usize {
        self.line(node.end().to_usize().saturating_sub(1))
    }
}

#[derive(Default)]
struct BodyFacts {
    calls: bool,
    broad: bool,
}

impl<'a> SourceOrderVisitor<'a> for BodyFacts {
    fn visit_expr(&mut self, expression: &'a ast::Expr) {
        match expression {
            ast::Expr::Call(_) => self.calls = true,
            ast::Expr::Name(name) if matches!(name.id.as_str(), "Exception" | "BaseException") => {
                self.broad = true;
            }
            _ => (),
        }
        walk_expr(self, expression);
    }
}

fn control(statement: &ast::Stmt) -> bool {
    matches!(
        statement,
        ast::Stmt::Pass(_) | ast::Stmt::Break(_) | ast::Stmt::Continue(_)
    )
}

fn exception_text(expression: &ast::Expr, source: &str) -> String {
    match expression {
        ast::Expr::Name(name) => name.id.to_string(),
        ast::Expr::Attribute(value) => {
            format!("{}.{}", exception_text(&value.value, source), value.attr)
        }
        ast::Expr::Tuple(tuple) => {
            let mut text = tuple
                .elts
                .iter()
                .map(|e| exception_text(e, source))
                .collect::<Vec<_>>()
                .join(", ");
            if tuple.elts.len() == 1 {
                text.push(',');
            }
            format!("({text})")
        }
        _ => source[expression.range()].to_owned(),
    }
}

impl Facts<'_> {
    fn fallback(&self, handler: &ast::ExceptHandlerExceptHandler) -> Option<Fallback> {
        let mut facts = BodyFacts::default();
        if let Some(kind) = &handler.type_ {
            facts.visit_expr(kind);
        }
        let bare = handler.type_.is_none();
        let broad = bare || facts.broad;
        facts.calls = false;
        for statement in &handler.body {
            facts.visit_stmt(statement);
        }
        let control_only = handler.body.iter().all(control);
        let silent_default = broad
            && !facts.calls
            && handler
                .body
                .iter()
                .all(|s| control(s) || matches!(s, ast::Stmt::Return(_) | ast::Stmt::Assign(_)));
        if !control_only && !silent_default {
            return None;
        }
        let (confidence, value, severity) = match (bare, broad, control_only) {
            (true, _, _) => (0.98, 100, "critical"),
            (_, true, true) => (0.90, 80, "high"),
            (_, true, _) => (0.75, 45, "medium"),
            _ => (0.65, 25, "medium"),
        };
        Some(Fallback {
            relative: self.relative.to_owned(),
            line: self.line(handler.start().to_usize()),
            end_line: self.end_line(handler),
            exception: handler
                .type_
                .as_ref()
                .map(|e| exception_text(e, self.source))
                .unwrap_or_else(|| "bare except".to_owned()),
            severity,
            confidence,
            value,
        })
    }
}

impl<'a> SourceOrderVisitor<'a> for Facts<'_> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if let ast::Stmt::FunctionDef(function) = statement {
            let registration = function.name.starts_with("register_")
                && function
                    .body
                    .iter()
                    .any(|s| matches!(s, ast::Stmt::FunctionDef(_)));
            self.functions.push((
                function.name.to_string(),
                self.line(function.start().to_usize()),
                self.end_line(function),
                registration,
            ));
        }
        walk_stmt(self, statement);
    }

    fn visit_except_handler(&mut self, handler: &'a ast::ExceptHandler) {
        let ast::ExceptHandler::ExceptHandler(handler_node) = handler;
        if let Some(finding) = self.fallback(handler_node) {
            self.fallbacks.push(finding);
        }
        walk_except_handler(self, handler);
    }
}

fn facts<'a>(relative: &'a str, source: &'a str) -> Result<Facts<'a>, String> {
    if source.len() > MAX_SOURCE_BYTES || source.contains('\0') {
        return Err("detector source exceeds size/NUL bound".to_owned());
    }
    let module = ruff_python_parser::parse_module(source)
        .map_err(|e| format!("{relative}: invalid Python: {e}"))?;
    Ok(facts_from_ast(relative, source, module.syntax()))
}

fn facts_from_ast<'a>(relative: &'a str, source: &'a str, module: &ast::ModModule) -> Facts<'a> {
    let mut facts = Facts {
        source,
        relative,
        line_starts: line_starts(source),
        functions: Vec::new(),
        fallbacks: Vec::new(),
    };
    for statement in &module.body {
        facts.visit_stmt(statement);
    }
    facts
}

/// Audit syntax failures are explicit coverage gaps; file-level findings remain available.
pub fn inspect(relative: &str, source: &str, policy: &Policy<'_>) -> Result<Scan, String> {
    if source.len() > MAX_SOURCE_BYTES || source.contains('\0') {
        return Err("detector source exceeds size/NUL bound".to_owned());
    }
    let mut result = Scan::default();
    let lines = source_lines(source);
    if lines.len() > policy.god_file_lines
        && !policy.allowed_files.contains(relative)
        && !source.contains("# guardrail: allow-god-file")
    {
        result
            .god_files
            .push(format!("{relative} ({})", lines.len()));
    }
    if !relative.ends_with(".py") {
        return Ok(result);
    }
    let facts = match facts(relative, source) {
        Ok(facts) => facts,
        Err(_) => {
            result.unparsable.push(relative.to_owned());
            return Ok(result);
        }
    };
    for (name, line, end, registration) in facts.functions {
        let span = end - line + 1;
        if span > policy.god_func_lines
            && !registration
            && !policy
                .allowed_functions
                .contains(&format!("{relative}::{name}"))
            && !lines[line - 1..end]
                .iter()
                .any(|l| l.contains("# guardrail: allow-god-function"))
        {
            result
                .god_functions
                .push(format!("{relative}:{line} {name} ({span})"));
        }
    }
    result.fallbacks = facts.fallbacks;
    Ok(result)
}

/// Strict gate scan. A documented waiver covers the handler's own lines only.
pub fn scan_source(relative: &str, source: &str) -> Result<Vec<Finding>, String> {
    let facts = facts(relative, source)?;
    Ok(finding_rows(facts))
}

pub fn scan_parsed(relative: &str, parsed: &crate::source::PythonSource<'_>) -> Vec<Finding> {
    finding_rows(facts_from_ast(relative, parsed.text(), parsed.module()))
}

fn finding_rows(facts: Facts<'_>) -> Vec<Finding> {
    let lines = source_lines(facts.source);
    let mut rows = Vec::new();
    for finding in facts.fallbacks {
        if lines[finding.line - 1..finding.end_line]
            .iter()
            .any(|line| line.contains(ALLOW_FALLBACK))
        {
            continue;
        }
        rows.push(Finding { path: finding.relative, line: finding.line,
            rule: "failure/silent-fallback", message: format!(
                "except {} swallows the error: re-raise it, log it at error level, or write down why with `# guardrail: allow-fallback`", finding.exception) });
    }
    rows.sort();
    rows
}

pub fn scan_paths(paths: &[String]) -> Result<Vec<Finding>, String> {
    if paths.len() > 256 {
        return Err("detector accepts at most 256 files".to_owned());
    }
    let mut rows = Vec::new();
    let mut bytes = 0;
    for path in paths {
        let source = read_source(Path::new(path))?;
        bytes += source.len();
        if bytes > 16 * MAX_SOURCE_BYTES {
            return Err("detector exceeds 16 MiB input budget".to_owned());
        }
        rows.extend(scan_source(&path.replace('\\', "/"), &source)?);
    }
    rows.sort();
    Ok(rows)
}

#[cfg(feature = "python")]
#[path = "detector_bindings.rs"]
mod bindings;
#[cfg(feature = "python")]
pub use bindings::{audit_detector_scan, fallback_scan_files};
