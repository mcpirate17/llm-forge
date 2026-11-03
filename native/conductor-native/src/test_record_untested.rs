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
use anyhow::{ensure, Context};
#[cfg(feature = "python")]
use pyo3::exceptions::PyRuntimeError;
#[cfg(feature = "python")]
use pyo3::prelude::*;
use ruff_python_ast as ast;
use ruff_python_ast::visitor::source_order::{walk_stmt, SourceOrderVisitor};
use ruff_text_size::{Ranged, TextSize};
use rusqlite::{limits::Limit, Connection, OpenFlags};
use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::Path;

pub(crate) struct Function {
    pub(crate) qualname: String,
    /// The `def` line.
    pub(crate) line: usize,
    /// The first decorator's line.
    start: usize,
    pub(crate) end: usize,
    docstring: Option<(usize, usize)>,
    /// The first body statement's line, docstring included.
    body: usize,
    /// The first line a call runs, or `None` for nothing to test.
    pub(crate) runs: Option<usize>,
}

impl Function {
    /// Whether `line` is this function's own code: inside it, outside its docstring
    /// and outside the bodies of the functions nested in it.
    pub(crate) fn owns(&self, line: usize, nested: &[Function]) -> bool {
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
pub(crate) fn functions(source: &str) -> anyhow::Result<Vec<Function>> {
    let document = crate::candidate_python::Document::parse(source)?;
    let module = document.module();
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
pub(crate) fn nested(functions: &[Function], index: usize) -> &[Function] {
    let end = functions[index].end;
    let after = &functions[index + 1..];
    &after[..after.partition_point(|inner| inner.start <= end)]
}

/// Only the recorder's concrete windows table is accepted. Candidate-controlled
/// SQLite views, generated columns and virtual tables cannot execute as evidence.
fn open_owned_record(path: &Path, deadline: std::time::Instant) -> anyhow::Result<Connection> {
    let metadata = std::fs::metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= 4 * 1024 * 1024,
        "owned record database exceeds limit"
    );
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, 4 * 1024 * 1024);
    connection.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, 64 * 1024);
    connection.set_limit(Limit::SQLITE_LIMIT_COLUMN, 64);
    connection.set_limit(Limit::SQLITE_LIMIT_EXPR_DEPTH, 100);
    connection.set_limit(Limit::SQLITE_LIMIT_VDBE_OP, 100_000);
    connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0);
    connection.set_limit(Limit::SQLITE_LIMIT_WORKER_THREADS, 0);
    connection.busy_timeout(std::time::Duration::from_millis(250))?;
    connection.progress_handler(10_000, Some(move || std::time::Instant::now() >= deadline));
    connection.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON;")?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    ensure!(
        version == crate::test_record_store::SCHEMA_VERSION,
        "unexpected record schema version"
    );
    let (kind, sql): (String, String) = connection.query_row(
        "SELECT type,sql FROM sqlite_schema WHERE name='windows'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let expected = concat!(
        "CREATE TABLE windows (\n",
        "    digest TEXT PRIMARY KEY, kind TEXT NOT NULL, key TEXT NOT NULL, opaque TEXT,\n",
        "    deps BLOB NOT NULL, lines BLOB NOT NULL)"
    );
    ensure!(
        kind == "table" && sql == expected,
        "record windows schema is not the native recorder schema"
    );
    Ok(connection)
}

/// Per repository path, the lines fixture and test windows executed while the file
/// had the blob oid `wanted` gives it.
fn executed_owned(
    store: &Path,
    wanted: &HashMap<&str, String>,
    deadline: std::time::Instant,
) -> anyhow::Result<HashMap<String, BTreeSet<usize>>> {
    let mut hits: HashMap<String, BTreeSet<usize>> = HashMap::new();
    let connection = open_owned_record(store, deadline)?;
    let mut windows =
        connection.prepare("SELECT lines FROM windows WHERE kind IN ('fixture', 'test')")?;
    let mut rows = windows.query([])?;
    let mut windows_seen = 0usize;
    let mut decoded_total = 0usize;
    let mut line_total = 0usize;
    while let Some(row) = rows.next()? {
        ensure!(
            std::time::Instant::now() < deadline,
            "owned record evaluation deadline exceeded"
        );
        windows_seen += 1;
        ensure!(windows_seen <= 100_000, "record windows exceed limit");
        let blob: Vec<u8> = row.get(0)?;
        ensure!(blob.len() <= 4 * 1024 * 1024, "record window exceeds limit");
        let mut decoded = Vec::new();
        zstd::stream::read::Decoder::new(blob.as_slice())?
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut decoded)?;
        ensure!(
            decoded.len() <= 16 * 1024 * 1024,
            "record decoded window exceeds limit"
        );
        decoded_total = decoded_total
            .checked_add(decoded.len())
            .context("record byte overflow")?;
        ensure!(
            decoded_total <= 64 * 1024 * 1024,
            "record decoded total exceeds limit"
        );
        ensure!(
            std::time::Instant::now() < deadline,
            "owned record evaluation deadline exceeded"
        );
        let entries: Vec<(String, String, Vec<usize>)> = serde_json::from_slice(&decoded)?;
        ensure!(
            std::time::Instant::now() < deadline,
            "owned record evaluation deadline exceeded"
        );
        ensure!(
            entries.len() <= 4096,
            "record window path count exceeds limit"
        );
        for (label, oid, lines) in entries {
            line_total = line_total
                .checked_add(lines.len())
                .context("record line overflow")?;
            ensure!(
                line_total <= 2_000_000 && label.len() <= 4096 && oid.len() <= 128,
                "record line or identity budget exceeded"
            );
            if wanted.get(label.as_str()) == Some(&oid) {
                hits.entry(label).or_default().extend(lines);
            }
        }
    }
    Ok(hits)
}

/// A changed function no recorded test executed: `(path, qualname, def line)`.
pub type Untested = (String, String, usize);

/// How many functions with something to test the changes touch, and which of them
/// no recorded test executed. Each change is a path relative to `root` and its
/// changed line numbers in the file there.
pub fn untested_owned(
    root: &Path,
    store: &Path,
    changes: &[(String, Vec<usize>)],
) -> anyhow::Result<(usize, Vec<Untested>)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    ensure!(changes.len() <= 4096, "changed source count exceeds limit");
    let mut files = Vec::with_capacity(changes.len());
    let mut wanted = HashMap::new();
    let mut source_bytes = 0usize;
    let mut changed_lines = 0usize;
    for (path, changed) in changes {
        ensure!(
            std::time::Instant::now() < deadline,
            "owned record evaluation deadline exceeded"
        );
        ensure!(
            !path.is_empty()
                && path.len() <= 4096
                && Path::new(path)
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))),
            "invalid changed source path"
        );
        changed_lines = changed_lines
            .checked_add(changed.len())
            .context("changed line overflow")?;
        ensure!(changed_lines <= 1_000_000, "changed lines exceed limit");
        let mut bytes = Vec::new();
        std::fs::File::open(root.join(path))
            .with_context(|| path.clone())?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        source_bytes = source_bytes
            .checked_add(bytes.len())
            .context("source size overflow")?;
        ensure!(
            bytes.len() <= 1024 * 1024 && source_bytes <= 32 * 1024 * 1024,
            "changed source bytes exceed limit"
        );
        wanted.insert(path.as_str(), blob_oid(&bytes));
        let source = String::from_utf8(bytes).with_context(|| path.clone())?;
        files.push((
            path,
            changed,
            functions(&source).with_context(|| path.clone())?,
        ));
    }
    let hits = executed_owned(store, &wanted, deadline)?;
    evaluate_functions(files, &hits, Some(deadline))
}

// Legacy Python API retains its existing store and source bounds.
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
    evaluate_functions(files, &hits, None)
}

fn evaluate_functions(
    files: Vec<(&String, &Vec<usize>, Vec<Function>)>,
    hits: &HashMap<String, BTreeSet<usize>>,
    deadline: Option<std::time::Instant>,
) -> anyhow::Result<(usize, Vec<Untested>)> {
    let none = BTreeSet::new();
    let mut count = 0;
    let mut out = Vec::new();
    for (path, changed, functions) in files {
        let hit = hits.get(path.as_str()).unwrap_or(&none);
        for (index, function) in functions.iter().enumerate() {
            if let Some(deadline) = deadline {
                ensure!(
                    std::time::Instant::now() < deadline,
                    "owned record evaluation deadline exceeded"
                );
            }
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
                if deadline.is_some() {
                    ensure!(out.len() < 4096, "untested function findings exceed limit");
                }
                out.push((path.clone(), function.qualname.clone(), function.line));
            }
        }
    }
    if let Some(deadline) = deadline {
        ensure!(
            std::time::Instant::now() < deadline,
            "owned record evaluation deadline exceeded"
        );
    }
    Ok((count, out))
}

/// `(changed, untested)` for `changes`, each a path relative to `root` and its
/// changed line numbers: how many changed functions have something to test, and the
/// `(path, qualname, def line)` of each one no run recorded in `store` executed.
#[cfg(feature = "python")]
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
    fn owned_shards_combine_only_real_fixture_and_test_windows() {
        let root = std::env::temp_dir().join(format!("owned-window-shards-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let source = "def fixture_hit():\n    return 1\n\ndef test_hit():\n    return 2\n\ndef collect_only():\n    return 3\n\ndef stale_only():\n    return 4\n";
        std::fs::write(root.join("m.py"), source).unwrap();
        let oid = blob_oid(source.as_bytes());
        let session = SessionRow {
            started: 0.0,
            finished: 1.0,
            root: "/r".into(),
            env_digest: "env".into(),
            env_data: json!({}),
            argv: "[]".into(),
            unstable: 0,
        };
        let first = root.join("first.sqlite");
        let second = root.join("second.sqlite");
        store::write(
            &first,
            &session,
            &[
                window("fixture", "fixture", json!([["m.py", oid, [2]]])),
                window("collect", "collect", json!([["m.py", oid, [8]]])),
            ],
            &[],
        )
        .unwrap();
        store::write(
            &second,
            &session,
            &[
                window("test", "test", json!([["m.py", oid, [5]]])),
                window("stale", "test", json!([["m.py", blob_oid(b"stale"), [11]]])),
            ],
            &[],
        )
        .unwrap();
        let changed = [("m.py".to_owned(), vec![2, 5, 8, 11])];
        let (count1, missing1) = untested_owned(&root, &first, &changed).unwrap();
        let (count2, missing2) = untested_owned(&root, &second, &changed).unwrap();
        assert_eq!((count1, count2), (4, 4));
        let a: BTreeSet<_> = missing1.into_iter().collect();
        let b: BTreeSet<_> = missing2.into_iter().collect();
        let combined: Vec<_> = a.intersection(&b).cloned().collect();
        assert_eq!(
            combined,
            vec![
                ("m.py".into(), "collect_only".into(), 7),
                ("m.py".into(), "stale_only".into(), 10)
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn untested_api_refuses_escaping_and_excessive_inputs() {
        let root = std::env::temp_dir();
        assert!(untested_owned(
            &root,
            &root.join("absent-forge-test.sqlite"),
            &[("../escape.py".into(), vec![1])]
        )
        .is_err());
        let too_many = vec![("x.py".to_owned(), Vec::new()); 4097];
        assert!(untested_owned(&root, &root.join("absent-forge-test.sqlite"), &too_many).is_err());
    }

    #[test]
    fn owned_record_refuses_recursive_view_before_reading_windows() {
        let root = std::env::temp_dir().join(format!("record-view-refusal-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("m.py"), "def f():\n    return 1\n").unwrap();
        let db = root.join("record.sqlite");
        let connection = Connection::open(&db).unwrap();
        connection.execute_batch("PRAGMA user_version=1; CREATE VIEW windows AS WITH RECURSIVE x(v) AS (VALUES(1) UNION ALL SELECT v+1 FROM x) SELECT 'test' AS kind, zeroblob(5000000) AS lines FROM x;").unwrap();
        drop(connection);
        let error = untested_owned(&root, &db, &[("m.py".into(), vec![2])]).unwrap_err();
        assert!(format!("{error:#}").contains("native recorder schema"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn historical_python_api_does_not_inherit_owned_artifact_cap() {
        let root = std::env::temp_dir().join(format!("record-legacy-large-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("m.py"), "def f():\n    return 1\n").unwrap();
        let db = root.join("record.sqlite");
        let session = SessionRow {
            started: 0.0,
            finished: 1.0,
            root: "/r".into(),
            env_digest: "env".into(),
            env_data: json!({}),
            argv: "[]".into(),
            unstable: 0,
        };
        store::write(&db, &session, &[], &[]).unwrap();
        let connection = Connection::open(&db).unwrap();
        connection.execute_batch("CREATE TABLE padding (data BLOB); INSERT INTO padding VALUES (zeroblob(5242880)); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        drop(connection);
        let changed = [("m.py".into(), vec![2])];
        assert_eq!(untested(&root, &db, &changed).unwrap().0, 1);
        assert!(untested_owned(&root, &db, &changed).is_err());
        std::fs::remove_dir_all(root).unwrap();
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
