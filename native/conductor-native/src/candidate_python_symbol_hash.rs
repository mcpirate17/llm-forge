//! Source-bound CPython 3.12 `ast.dump` symbol identities; no interpreter.
//! Unsupported syntax fails the entire inventory, never a partial hash map.
#[path = "candidate_python_symbol_expr.rs"]
mod expr;
#[path = "candidate_python_symbol_stmt.rs"]
mod stmt;
#[path = "candidate_python_symbol_surrogate.rs"]
mod surrogate;
#[cfg(test)]
#[path = "candidate_python_symbol_tests.rs"]
mod tests;
use ruff_python_ast as ast;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const PROTOCOL: &str = "cpython-ast-dump-3.12";
const MAX_WORK: usize = 32 * 1024 * 1024;
const MAX_DEPTH: usize = 128;
struct Dump<'a> {
    work: usize,
    source: &'a str,
    surrogate: surrogate::Markers,
}
impl Dump<'_> {
    fn account(&mut self, value: String, depth: usize) -> Result<String, String> {
        if depth > MAX_DEPTH {
            return Err("CPython dump nesting exceeds 128".into());
        }
        self.work = self
            .work
            .checked_add(value.len())
            .ok_or("CPython dump work overflow")?;
        if self.work > MAX_WORK {
            return Err("CPython dump exceeds 32 MiB work limit".into());
        }
        Ok(value)
    }
    fn form(
        &mut self,
        name: &str,
        fields: Vec<(&str, String)>,
        depth: usize,
    ) -> Result<String, String> {
        let body = fields
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ");
        self.account(format!("{name}({body})"), depth)
    }
    fn list(&mut self, values: Vec<String>, depth: usize) -> Result<String, String> {
        self.account(format!("[{}]", values.join(", ")), depth)
    }
    fn repr(&mut self, value: &str, depth: usize) -> Result<String, String> {
        if value.len() > MAX_WORK {
            return Err("string exceeds CPython dump limit".into());
        }
        let value = crate::candidate_unicode::python_repr_bounded(value);
        let value = if self.surrogate.is_empty() {
            value
        } else {
            surrogate::restore_repr(&value, &self.surrogate)
        };
        self.account(value, depth)
    }
    fn ident(&mut self, value: &str, depth: usize) -> Result<String, String> {
        self.repr(value, depth)
    }
    fn stmts(&mut self, values: &[ast::Stmt], depth: usize) -> Result<String, String> {
        let mut out = Vec::with_capacity(values.len());
        for value in values {
            out.push(self.stmt(value, depth + 1)?);
        }
        self.list(out, depth)
    }
    fn exprs(&mut self, values: &[ast::Expr], depth: usize) -> Result<String, String> {
        let mut out = Vec::with_capacity(values.len());
        for value in values {
            out.push(self.expr(value, depth + 1)?);
        }
        self.list(out, depth)
    }
    fn optional_expr(
        &mut self,
        fields: &mut Vec<(&'static str, String)>,
        name: &'static str,
        value: Option<&ast::Expr>,
        depth: usize,
    ) -> Result<(), String> {
        if let Some(value) = value {
            fields.push((name, self.expr(value, depth + 1)?));
        }
        Ok(())
    }
    fn check_depth(depth: usize) -> Result<(), String> {
        if depth > MAX_DEPTH {
            Err("CPython dump nesting exceeds 128".into())
        } else {
            Ok(())
        }
    }
}
/// Hash each module-level definition and each direct class method. Python 3.12
/// grammar admission is checked for the entire source, not only selected nodes.
pub fn hashes(source: &str) -> Result<BTreeMap<String, String>, String> {
    hashes_with_protocol(source, PROTOCOL)
}
/// Reject a protocol mismatch before parsing or producing any identity.
pub fn hashes_with_protocol(
    source: &str,
    protocol: &str,
) -> Result<BTreeMap<String, String>, String> {
    if protocol != PROTOCOL {
        return Err(format!(
            "unsupported Python symbol hash protocol: {protocol}"
        ));
    }
    // Admit the original before any rewrite; only escaped surrogate code points
    // in non-raw string tokens are replaced, with byte offsets preserved.
    crate::candidate_python::Document::parse(source).map_err(|e| e.to_string())?;
    let options = ruff_python_parser::ParseOptions::from(ruff_python_parser::Mode::Module)
        .with_target_version(ast::PythonVersion::PY312);
    let versioned = ruff_python_parser::parse_unchecked(source, options);
    if !versioned.unsupported_syntax_errors().is_empty() {
        return Err("source contains syntax unsupported by CPython 3.12".into());
    }
    use ruff_text_size::Ranged;
    for token in versioned.tokens() {
        if token.kind() == ast::token::TokenKind::Name {
            let range = token.range();
            let name = &source[range.start().to_usize()..range.end().to_usize()];
            if name.chars().any(|ch| {
                unicode_general_category::get_general_category(ch)
                    == unicode_general_category::GeneralCategory::Unassigned
            }) {
                return Err("identifier contains a code point unassigned in Unicode 15".into());
            }
        }
    }
    let (sanitized, surrogate) = surrogate::sanitize(source, versioned.tokens())?;
    let document =
        crate::candidate_python::Document::parse(&sanitized).map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    let mut dump = Dump {
        work: 0,
        source,
        surrogate,
    };
    for node in &document.module().body {
        match node {
            ast::Stmt::FunctionDef(value) => insert(&mut out, &value.name, dump.stmt(node, 0)?),
            ast::Stmt::ClassDef(value) => {
                insert(&mut out, &value.name, dump.stmt(node, 0)?);
                for child in &value.body {
                    if let ast::Stmt::FunctionDef(method) = child {
                        insert(
                            &mut out,
                            &format!("{}.{}", value.name, method.name),
                            dump.stmt(child, 0)?,
                        );
                    }
                }
            }
            _ => (),
        }
    }
    Ok(out)
}
fn insert(out: &mut BTreeMap<String, String>, name: &str, dump: String) {
    out.insert(
        name.to_owned(),
        format!("{:x}", Sha256::digest(dump.as_bytes())),
    );
}
