//! Shared location-free body fingerprints. No interpreter or filesystem access.
//! Candidate and standalone semantics intentionally remain distinct.
use crate::candidate_python::{Document, Error, Lines};
use ruff_python_ast as ast;
use ruff_python_ast::comparable::{ComparableExpr, ComparableParameters, ComparableStmt};
use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
pub enum FingerprintPolicy {
    Candidate,
    Standalone,
}

#[derive(Debug, Serialize)]
pub struct FunctionFingerprint {
    pub name: String,
    pub lineno: usize,
    pub end_lineno: usize,
    pub digest: String,
}

#[derive(Debug, Serialize)]
pub struct FileFingerprints {
    pub path: String,
    pub parse_error: bool,
    pub functions: Vec<FunctionFingerprint>,
}

struct FunctionCollector<'a> {
    lines: Lines,
    policy: FingerprintPolicy,
    statement_depth: usize,
    functions: Vec<(usize, usize, FunctionFingerprint)>,
    marker: std::marker::PhantomData<&'a str>,
}

impl FunctionCollector<'_> {
    fn line_number(&self, byte_offset: usize) -> usize {
        self.lines.number(byte_offset)
    }

    fn digest(value: &impl std::fmt::Debug) -> String {
        format!("{:x}", Sha256::digest(format!("{value:?}").as_bytes()))
    }

    fn is_docstring(statement: &ast::Stmt) -> bool {
        matches!(
            statement,
            ast::Stmt::Expr(expr)
                if matches!(expr.value.as_ref(), ast::Expr::StringLiteral(_))
        )
    }

    fn candidate_digest(&self, function: &ast::StmtFunctionDef, length: usize) -> Option<String> {
        let body = if function.body.first().is_some_and(Self::is_docstring) {
            &function.body[1..]
        } else {
            &function.body
        };
        if length < 10 || body.is_empty() {
            return None;
        }
        let body = body.iter().map(ComparableStmt::from).collect::<Vec<_>>();
        Some(Self::digest(&("Module", body)))
    }

    fn standalone_digest(&self, function: &ast::StmtFunctionDef, length: usize) -> Option<String> {
        if length < 8 {
            return None;
        }
        let parameters = ComparableParameters::from(&function.parameters);
        let body = function
            .body
            .iter()
            .map(ComparableStmt::from)
            .collect::<Vec<_>>();
        let returns: Option<ComparableExpr<'_>> = function.returns.as_ref().map(Into::into);
        Some(Self::digest(&("FunctionDef", parameters, body, returns)))
    }

    fn fingerprint(&self, function: &ast::StmtFunctionDef) -> Option<FunctionFingerprint> {
        let lineno = self.line_number(function.name.range.start().to_usize());
        let end_lineno = self.line_number(function.range.end().to_usize());
        let length = end_lineno.saturating_sub(lineno) + 1;
        let digest = match self.policy {
            FingerprintPolicy::Candidate => self.candidate_digest(function, length),
            FingerprintPolicy::Standalone => self.standalone_digest(function, length),
        }?;
        Some(FunctionFingerprint {
            name: function.name.as_str().to_owned(),
            lineno,
            end_lineno,
            digest,
        })
    }
}

impl<'a> SourceOrderVisitor<'a> for FunctionCollector<'a> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        if let ast::Stmt::FunctionDef(function) = statement {
            if let Some(fingerprint) = self.fingerprint(function) {
                self.functions.push((
                    self.statement_depth,
                    function.name.range.start().to_usize(),
                    fingerprint,
                ));
            }
        }
        self.statement_depth += 1;
        walk_stmt(self, statement);
        self.statement_depth -= 1;
    }
}

/// Compatibility batch record: malformed syntax retains parse_error; resource
/// admission errors remain explicit errors and cannot masquerade as parse failures.
pub fn analyze_file(
    path: String,
    source: String,
    policy: FingerprintPolicy,
) -> Result<FileFingerprints, Error> {
    match Document::parse(&source) {
        Ok(document) => Ok(analyze_document(path, &document, policy)),
        Err(Error::Syntax { .. }) => Ok(FileFingerprints {
            path,
            parse_error: true,
            functions: Vec::new(),
        }),
        Err(error) => Err(error),
    }
}

/// Reuse the admitted source-bound AST; callers cannot pair an AST with other text.
pub fn analyze_document(
    path: String,
    document: &Document<'_>,
    policy: FingerprintPolicy,
) -> FileFingerprints {
    let source = document.text();
    let syntax = document.module();
    let mut collector = FunctionCollector {
        lines: Lines::new(source),
        policy,
        statement_depth: 0,
        functions: Vec::new(),
        marker: std::marker::PhantomData,
    };
    for statement in &syntax.body {
        collector.visit_stmt(statement);
    }
    collector
        .functions
        .sort_by_key(|(depth, byte_offset, _)| (*depth, *byte_offset));
    FileFingerprints {
        path,
        parse_error: false,
        functions: collector
            .functions
            .into_iter()
            .map(|(_, _, fingerprint)| fingerprint)
            .collect(),
    }
}
