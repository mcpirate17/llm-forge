//! Source-bound Python AST admission shared by native candidate analysis.
//! Lexical work limits run before the recursive parser. These are explicit
//! resource limits, not a second grammar; Ruff determines syntax acceptance.
use ruff_python_ast::visitor::source_order::{SourceOrderVisitor, TraversalSignal};
use ruff_python_ast::{self as ast, token::TokenKind};
use ruff_python_parser::{lexer::lex, Mode, Parsed};
use std::fmt;

pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_NODES: usize = 65_536;
pub const MAX_DEPTH: usize = 128;
const MAX_TOKENS: usize = 262_144;
const MAX_STATEMENT_OPERATIONS: usize = 256;
const MAX_INDENT: usize = 64;

/// Python physical line starts in original byte coordinates. CRLF is one
/// terminator; bare CR and LF are each terminators. No source bytes are rewritten.
pub struct Lines {
    starts: Vec<usize>,
}
impl Lines {
    pub fn new(source: &str) -> Self {
        let bytes = source.as_bytes();
        let mut starts = vec![0];
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\r' => {
                    i += 1;
                    if bytes.get(i) == Some(&b'\n') {
                        i += 1;
                    }
                    starts.push(i);
                }
                b'\n' => {
                    i += 1;
                    starts.push(i);
                }
                _ => i += 1,
            }
        }
        Self { starts }
    }
    pub fn number(&self, offset: usize) -> usize {
        self.starts.partition_point(|start| *start <= offset)
    }
    pub(crate) fn column(&self, offset: usize) -> usize {
        offset - self.starts[self.number(offset).saturating_sub(1)]
    }
    pub(crate) fn text<'a>(&self, source: &'a str) -> Vec<&'a str> {
        self.starts
            .iter()
            .enumerate()
            .filter_map(|(i, start)| {
                if *start == source.len() {
                    return None;
                }
                let end = self.starts.get(i + 1).copied().unwrap_or(source.len());
                Some(source[*start..end].trim_end_matches(['\r', '\n']))
            })
            .collect()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Syntax { offset: usize },
    Limit(&'static str),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { offset } => write!(f, "invalid Python syntax at byte {offset}"),
            Self::Limit(reason) => write!(f, "Python analysis admission: {reason}"),
        }
    }
}
impl std::error::Error for Error {}

fn recursive_operation(kind: TokenKind) -> bool {
    kind.is_arithmetic()
        || kind.is_bitwise_or_shift()
        || matches!(
            kind,
            TokenKind::Lpar
                | TokenKind::Percent
                | TokenKind::Lsqb
                | TokenKind::Lbrace
                | TokenKind::Dot
                | TokenKind::Lambda
                | TokenKind::If
                | TokenKind::Else
                | TokenKind::For
                | TokenKind::Not
                | TokenKind::And
                | TokenKind::Or
                | TokenKind::Await
                | TokenKind::Yield
                | TokenKind::ColonEqual
                | TokenKind::FStringStart
                | TokenKind::TStringStart
        )
}

fn preflight(source: &str) -> Result<(), Error> {
    if source.len() > MAX_BYTES {
        return Err(Error::Limit("source exceeds 1 MiB"));
    }
    if let Some(offset) = source.find('\0') {
        return Err(Error::Syntax { offset });
    }
    let mut lexer = lex(source, Mode::Module);
    let mut indentation = 0usize;
    let mut operations = 0usize;
    for _ in 0..MAX_TOKENS {
        let kind = lexer.next_token();
        match kind {
            TokenKind::EndOfFile => return Ok(()),
            TokenKind::Indent => indentation += 1,
            TokenKind::Dedent => indentation = indentation.saturating_sub(1),
            TokenKind::Newline | TokenKind::Semi => operations = 0,
            _ if recursive_operation(kind) => operations += 1,
            _ => {}
        }
        if indentation > MAX_INDENT {
            return Err(Error::Limit("indentation exceeds 64 levels"));
        }
        if operations > MAX_STATEMENT_OPERATIONS {
            return Err(Error::Limit(
                "logical statement exceeds 256 recursive operations",
            ));
        }
    }
    Err(Error::Limit("token count exceeds 262144"))
}

#[derive(Default)]
struct Admission {
    nodes: usize,
    depth: usize,
    exceeded: bool,
}
impl<'a> SourceOrderVisitor<'a> for Admission {
    fn enter_node(&mut self, _: ast::AnyNodeRef<'a>) -> TraversalSignal {
        self.nodes += 1;
        self.depth += 1;
        self.exceeded |= self.nodes > MAX_NODES || self.depth > MAX_DEPTH;
        if self.exceeded {
            TraversalSignal::Skip
        } else {
            TraversalSignal::Traverse
        }
    }
    fn leave_node(&mut self, _: ast::AnyNodeRef<'a>) {
        self.depth -= 1;
    }
}

pub struct Document<'a> {
    source: &'a str,
    parsed: Parsed<ast::ModModule>,
    nodes: usize,
}
impl<'a> Document<'a> {
    pub fn parse(source: &'a str) -> Result<Self, Error> {
        preflight(source)?;
        let parsed = ruff_python_parser::parse_module(source).map_err(|error| Error::Syntax {
            offset: error.location.start().to_usize(),
        })?;
        Self::admit(source, parsed)
    }
    /// One source-bound parse pinned to CPython3.12 grammar. Version errors
    /// refuse rather than analyze a different Python language.
    pub fn parse_python312(source: &'a str) -> Result<Self, Error> {
        // CPython ast.parse(str) rejects a BOM; Ruff accepts it as a file marker.
        // This path models the legacy UTF-8 text reader, not encoding detection.
        if source.starts_with('\u{feff}') {
            return Err(Error::Syntax { offset: 0 });
        }
        preflight(source)?;
        let options = ruff_python_parser::ParseOptions::from(Mode::Module)
            .with_target_version(ast::PythonVersion::PY312);
        let parsed = ruff_python_parser::parse(source, options).map_err(|error| Error::Syntax {
            offset: error.location.start().to_usize(),
        })?;
        if let Some(error) = parsed.unsupported_syntax_errors().first() {
            return Err(Error::Syntax {
                offset: error.range.start().to_usize(),
            });
        }
        for token in parsed.tokens() {
            if token.kind() == TokenKind::Name {
                use ruff_text_size::Ranged;
                let range = token.range();
                if source[range.start().to_usize()..range.end().to_usize()]
                    .chars()
                    .any(|ch| {
                        unicode_general_category::get_general_category(ch)
                            == unicode_general_category::GeneralCategory::Unassigned
                    })
                {
                    return Err(Error::Syntax {
                        offset: range.start().to_usize(),
                    });
                }
            }
        }
        Self::admit(
            source,
            parsed
                .try_into_module()
                .ok_or(Error::Limit("expected a module"))?,
        )
    }
    fn admit(source: &'a str, parsed: Parsed<ast::ModModule>) -> Result<Self, Error> {
        let mut admission = Admission::default();
        for statement in parsed.suite() {
            admission.visit_stmt(statement);
            if admission.exceeded {
                return Err(Error::Limit("AST exceeds 65536 nodes or 128 levels"));
            }
        }
        Ok(Self {
            source,
            parsed,
            nodes: admission.nodes,
        })
    }
    pub fn text(&self) -> &'a str {
        self.source
    }
    pub fn parsed(&self) -> &Parsed<ast::ModModule> {
        &self.parsed
    }
    pub fn module(&self) -> &ast::ModModule {
        self.parsed.syntax()
    }
    pub fn nodes(&self) -> usize {
        self.nodes
    }
}

#[cfg(test)]
#[path = "candidate_python_tests.rs"]
mod tests;
