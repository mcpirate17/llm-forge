//! Changed functions no recorded test executed.
//!
//! A changed function is tested when a fixture or test window of any recorded run,
//! under any environment, executed one of its own lines in the file's current
//! content: its body after the docstring, less the bodies of the functions nested in
//! it. Lines run at import or collection do not count, so a `def` line never makes
//! its function tested; a function whose body shares its `def` line counts as tested
//! once that line runs inside a test, which a lazy import there also does. A function
//! changed only in its docstring is not changed, and a body of only `...` or `pass`
//! has nothing to test.

use crate::candidate_structure::Lines;
use crate::test_record_digest::blob_oid;
use crate::test_record_store::open_read_only;
use anyhow::Context;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use ruff_python_ast as ast;
use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
use ruff_text_size::{Ranged, TextSize};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

struct Function {
    qualname: String,
    /// The `def` line.
    line: usize,
    /// The first decorator's line.
    start: usize,
    end: usize,
    docstring: Option<(usize, usize)>,
    /// The first body statement's line, docstring included.
    body: usize,
    /// The first line a call runs, or `None` for nothing to test.
    runs: Option<usize>,
}

impl Function {
    /// Whether `line` is this function's own code: inside it, outside its docstring
    /// and outside the bodies of the functions nested in it.
    fn owns(&self, line: usize, nested: &[Function]) -> bool {
        (self.start..=self.end).contains(&line)
            && !self
                .docstring
                .is_some_and(|(first, last)| (first..=last).contains(&line))
            && !nested
                .iter()
                .any(|inner| (inner.body..=inner.end).contains(&line))
    }
}

struct Collector<'l> {
    lines: &'l Lines,
    /// Enclosing definitions, innermost last; `true` for a function.
    scope: Vec<(String, bool)>,
    functions: Vec<Function>,
}

impl Collector<'_> {
    fn line(&self, offset: TextSize) -> usize {
        self.lines.number(offset.to_usize())
    }

    fn last_line(&self, node: &impl Ranged) -> usize {
        self.lines.number(node.end().to_usize().saturating_sub(1))
    }

    fn function(&self, function: &ast::StmtFunctionDef) -> Function {
        let mut qualname = String::new();
        for (name, is_function) in &self.scope {
            qualname.push_str(name);
            qualname.push_str(if *is_function { ".<locals>." } else { "." });
        }
        qualname.push_str(function.name.as_str());
        let docstring = function.body.first().filter(|statement| {
            matches!(statement, ast::Stmt::Expr(expr) if expr.value.is_string_literal_expr())
        });
        let rest = &function.body[usize::from(docstring.is_some())..];
        let trivial = |statement: &ast::Stmt| match statement {
            ast::Stmt::Pass(_) => true,
            ast::Stmt::Expr(expr) => expr.value.is_ellipsis_literal_expr(),
            _ => false,
        };
        Function {
            qualname,
            line: self.line(function.name.start()),
            start: self.line(function.start()),
            end: self.last_line(function),
            docstring: docstring
                .map(|statement| (self.line(statement.start()), self.last_line(statement))),
            body: self.line(function.body[0].start()),
            runs: (!rest.iter().all(trivial)).then(|| self.line(rest[0].start())),
        }
    }
}

impl<'a> SourceOrderVisitor<'a> for Collector<'_> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        let entered = match statement {
            ast::Stmt::FunctionDef(function) => {
                let found = self.function(function);
                self.functions.push(found);
                Some((function.name.to_string(), true))
            }
            ast::Stmt::ClassDef(class) => Some((class.name.to_string(), false)),
            _ => None,
        };
        let pushed = entered.is_some();
        self.scope.extend(entered);
        walk_stmt(self, statement);
        if pushed {
            self.scope.pop();
        }
    }
}

/// Every function in `source`, nested ones and methods included, in source order.
fn functions(source: &str) -> anyhow::Result<Vec<Function>> {
    let module = ruff_python_parser::parse_module(source)?.into_syntax();
    let lines = Lines::new(source);
    let mut collector = Collector {
        lines: &lines,
        scope: Vec::new(),
        functions: Vec::new(),
    };
    collector.visit_body(&module.body);
    Ok(collector.functions)
}

/// The functions nested in `functions[index]`: those after it that start inside it.
fn nested(functions: &[Function], index: usize) -> &[Function] {
    let end = functions[index].end;
    let after = &functions[index + 1..];
    &after[..after.partition_point(|inner| inner.start <= end)]
}

/// Per repository path, the lines fixture and test windows executed while the file
/// had the blob oid `wanted` gives it.
fn executed(
    store: &Path,
    wanted: &HashMap<&str, String>,
) -> anyhow::Result<HashMap<String, BTreeSet<usize>>> {
    let mut hits: HashMap<String, BTreeSet<usize>> = HashMap::new();
    let Some(connection) = open_read_only(store)? else {
        return Ok(hits);
    };
    let mut windows =
        connection.prepare("SELECT lines FROM windows WHERE kind IN ('fixture', 'test')")?;
    let mut rows = windows.query([])?;
    while let Some(row) = rows.next()? {
        let blob: Vec<u8> = row.get(0)?;
        let entries: Vec<(String, String, Vec<usize>)> =
            serde_json::from_slice(&zstd::decode_all(blob.as_slice())?)?;
        for (label, oid, lines) in entries {
            if wanted.get(label.as_str()) == Some(&oid) {
                hits.entry(label).or_default().extend(lines);
            }
        }
    }
    Ok(hits)
}

/// A changed function no recorded test executed: `(path, qualname, def line)`.
type Untested = (String, String, usize);

/// How many functions with something to test the changes touch, and which of them
/// no recorded test executed. Each change is a path relative to `root` and its
/// changed line numbers in the file there.
fn untested(
    root: &Path,
    store: &Path,
    changes: &[(String, Vec<usize>)],
) -> anyhow::Result<(usize, Vec<Untested>)> {
    let mut files = Vec::with_capacity(changes.len());
    let mut wanted = HashMap::new();
    for (path, changed) in changes {
        let bytes = std::fs::read(root.join(path)).with_context(|| path.clone())?;
        wanted.insert(path.as_str(), blob_oid(&bytes));
        let source = String::from_utf8(bytes).with_context(|| path.clone())?;
        files.push((
            path,
            changed,
            functions(&source).with_context(|| path.clone())?,
        ));
    }
    let hits = executed(store, &wanted)?;
    let none = BTreeSet::new();
    let mut count = 0;
    let mut out = Vec::new();
    for (path, changed, functions) in files {
        let hit = hits.get(path.as_str()).unwrap_or(&none);
        for (index, function) in functions.iter().enumerate() {
            let Some(runs) = function.runs else { continue };
            let inner = nested(&functions, index);
            if !changed.iter().any(|line| function.owns(*line, inner)) {
                continue;
            }
            count += 1;
            if !hit
                .range(runs..=function.end)
                .any(|line| function.owns(*line, inner))
            {
                out.push((path.clone(), function.qualname.clone(), function.line));
            }
        }
    }
    Ok((count, out))
}

/// `(changed, untested)` for `changes`, each a path relative to `root` and its
/// changed line numbers: how many changed functions have something to test, and the
/// `(path, qualname, def line)` of each one no run recorded in `store` executed.
#[pyfunction]
pub(crate) fn test_record_untested_native(
    py: Python<'_>,
    root: &str,
    store: &str,
    changes: Vec<(String, Vec<usize>)>,
) -> PyResult<(usize, Vec<Untested>)> {
    py.detach(|| untested(Path::new(root), Path::new(store), &changes))
        .map_err(|error| PyRuntimeError::new_err(format!("{error:#}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_record_store::{self as store, SessionRow, WindowRow};
    use serde_json::{json, Value};

    const SOURCE: &str = r#"import functools


def plain(x):
    """Doc."""
    return x + 1


@functools.cache
def decorated():
    return 2


def outer():
    def inner():
        return 3

    return inner


class C:
    def method(self):
        return 4

    def stub(self):
        ...


def untouched():
    return 5
"#;

    fn window(digest: &str, kind: &'static str, lines: Value) -> WindowRow {
        WindowRow {
            digest: digest.to_owned(),
            kind,
            key: String::new(),
            opaque: None,
            deps: json!([]),
            lines,
        }
    }

    #[test]
    fn a_changed_function_is_tested_only_by_its_own_lines_in_a_test() {
        let root =
            std::env::temp_dir().join(format!("test-record-untested-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("m.py"), SOURCE).unwrap();
        let db = root.join("record.sqlite");
        let oid = blob_oid(SOURCE.as_bytes());
        let session = SessionRow {
            started: 0.0,
            finished: 1.0,
            root: "/r".to_owned(),
            env_digest: "env".to_owned(),
            env_data: json!({}),
            argv: "[]".to_owned(),
            unstable: 0,
        };
        store::write(
            &db,
            &session,
            &[
                // Line 16 is inner's body, not outer's.
                window("t", "test", json!([["m.py", oid, [6, 16]]])),
                window("c", "collect", json!([["m.py", oid, [9, 10, 14, 23]]])),
                window("old", "fixture", json!([["m.py", blob_oid(b"old"), [11]]])),
            ],
            &[],
        )
        .unwrap();
        // The docstring (5), decorator (9), inner's and outer's bodies (16, 18), the
        // method (23) and the stub (26).
        let changes = [("m.py".to_owned(), vec![5, 6, 9, 16, 18, 23, 26])];
        let at = |name: &str, line| ("m.py".to_owned(), name.to_owned(), line);
        assert_eq!(
            untested(&root, &db, &changes).unwrap(),
            (
                5,
                vec![at("decorated", 10), at("outer", 14), at("C.method", 22)]
            )
        );
        // A docstring-only change changes nothing.
        let docstring = [("m.py".to_owned(), vec![5])];
        assert_eq!(untested(&root, &db, &docstring).unwrap(), (0, vec![]));
        // With no store, every changed function is untested.
        assert_eq!(
            untested(&root, &root.join("none.sqlite"), &changes).unwrap(),
            (
                5,
                vec![
                    at("plain", 4),
                    at("decorated", 10),
                    at("outer", 14),
                    at("outer.<locals>.inner", 15),
                    at("C.method", 22),
                ]
            )
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
