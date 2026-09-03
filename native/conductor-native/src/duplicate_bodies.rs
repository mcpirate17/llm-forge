//! Batched Python function-body fingerprints for governance duplicate detection.
//!
//! Git selection, snapshot reads, changed-line attribution, move detection, and
//! finding wording remain in Python. This module owns the repeated parse and
//! location-free structural canonicalization shared by the two policy callers.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use ruff_python_ast as ast;
use ruff_python_ast::comparable::{ComparableExpr, ComparableParameters, ComparableStmt};
use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
enum FingerprintPolicy {
    Candidate,
    Standalone,
}

impl FingerprintPolicy {
    fn parse(value: &str) -> PyResult<Self> {
        match value {
            "candidate" => Ok(Self::Candidate),
            "standalone" => Ok(Self::Standalone),
            _ => Err(PyValueError::new_err(format!(
                "unknown duplicate-body policy: {value}"
            ))),
        }
    }
}

#[derive(Debug, Serialize)]
struct FunctionFingerprint {
    name: String,
    lineno: usize,
    end_lineno: usize,
    digest: String,
}

#[derive(Debug, Serialize)]
struct FileFingerprints {
    path: String,
    parse_error: bool,
    functions: Vec<FunctionFingerprint>,
}

struct FunctionCollector<'a> {
    line_starts: Vec<usize>,
    policy: FingerprintPolicy,
    statement_depth: usize,
    functions: Vec<(usize, usize, FunctionFingerprint)>,
    marker: std::marker::PhantomData<&'a str>,
}

impl FunctionCollector<'_> {
    fn line_number(&self, byte_offset: usize) -> usize {
        self.line_starts
            .partition_point(|start| *start <= byte_offset)
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

fn analyze_file(path: String, source: String, policy: FingerprintPolicy) -> FileFingerprints {
    let Ok(module) = ruff_python_parser::parse_module(&source) else {
        return FileFingerprints {
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

#[pyfunction]
fn duplicate_body_fingerprints_native(
    py: Python<'_>,
    records: Vec<(String, String)>,
    policy: &str,
) -> PyResult<String> {
    let policy = FingerprintPolicy::parse(policy)?;
    let fingerprints = py.detach(|| {
        records
            .into_iter()
            .map(|(path, source)| analyze_file(path, source, policy))
            .collect::<Vec<_>>()
    });
    serde_json::to_string(&fingerprints).map_err(|error| PyValueError::new_err(error.to_string()))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(
        duplicate_body_fingerprints_native,
        module
    )?)?;
    Ok(())
}
