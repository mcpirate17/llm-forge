//! `.unwrap()` and stub macros in production Rust.
//!
//! CLAUDE.md's "fail loud" rule had a detector for Python exception handlers
//! and nothing at all for Rust, which is where this repository's compute lives.
//! `.unwrap()` is the Rust spelling of a swallowed failure inverted: it does not
//! hide the error, it discards the reason and aborts the process, and the
//! backtrace names `core::option` rather than the call that was wrong.
//! `todo!()` and `unimplemented!()` are scaffolding that type-checks, so nothing
//! else in the toolchain notices them.
//!
//! `.expect("why")` is deliberately allowed: it fails just as loudly and carries
//! the reason, which is the whole of what is being asked for.
//!
//! Test code is exempt, because a panic *is* the assertion there. Exemption is
//! by `#[cfg(test)]` and `#[test]` scoping rather than by path, since 39 of the
//! 117 non-test-path files in this tree carry an inline `#[cfg(test)] mod`.

use std::fs;
use std::path::PathBuf;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use tree_sitter::{Node, Parser, Tree};

/// Escape hatches, spelled like the ones style_scan.rs and detector_scan.rs
/// already honour.
const ALLOW_UNWRAP: &str = "guardrail: allow-unwrap";
const ALLOW_STUB: &str = "guardrail: allow-stub";

/// Methods that abort on the branch they do not want. `unwrap_err` is the same
/// defect read from the other side: it panics on the value the caller expected
/// to be an error, and prints the success value instead of a reason.
const PANICKING_METHODS: &[&str] = &["unwrap", "unwrap_err"];

/// Macros that compile to a panic and mean the code was never written.
const STUB_MACROS: &[&str] = &["todo", "unimplemented"];

pub const RULES: &[&str] = &["failure/rust-unwrap", "dead/rust-stub"];

/// A line-addressed rule violation, ordered the way the gate prints it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RustFinding {
    pub path: String,
    pub line: usize,
    pub rule: &'static str,
    pub message: String,
}

/// A source file plus the line index the opt-out lookup walks.
struct Source<'a> {
    src: &'a str,
    lines: Vec<&'a str>,
}

impl<'a> Source<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            lines: src.split('\n').collect(),
        }
    }

    fn text(&self, node: Node) -> &'a str {
        &self.src[node.byte_range()]
    }

    /// True when `marker` appears on `line` or in the comment block directly
    /// above it. Both spellings are wanted: the reason fits on the line for a
    /// one-liner, and above it when it takes a sentence.
    fn opted_out(&self, line: usize, marker: &str) -> bool {
        if line == 0 || line > self.lines.len() {
            return false;
        }
        if self.lines[line - 1].contains(marker) {
            return true;
        }
        let mut above = line - 1;
        while above > 0 {
            let text = self.lines[above - 1].trim_start();
            if !text.starts_with("//") {
                return false;
            }
            if text.contains(marker) {
                return true;
            }
            above -= 1;
        }
        false
    }
}

/// A file whose every item is test code, which `#[cfg(test)]` scoping cannot
/// see: the `mod` declaration that gates it lives in another file.
fn is_test_file(relative: &str) -> bool {
    let name = match relative.rsplit('/').next() {
        Some(name) => name,
        None => relative,
    };
    name.ends_with("_tests.rs")
        || name.ends_with("_test.rs")
        || relative.starts_with("tests/")
        || relative.starts_with("benches/")
        || relative.contains("/tests/")
        || relative.contains("/benches/")
}

/// True when `attribute` puts the item it precedes under `cfg(test)`.
///
/// Whitespace is squashed first because `#[cfg(all(test, unix))]` is written
/// with and without spaces and both mean the same thing.
fn marks_test(attribute: &str) -> bool {
    let squashed: String = attribute.chars().filter(|c| !c.is_whitespace()).collect();
    if squashed.contains("cfg(test)") || squashed.contains("(test,") || squashed.contains(",test)")
    {
        return true;
    }
    // `#[test]`, plus the harness attributes that stand in for it.
    matches!(squashed.as_str(), "#[test]" | "#[rstest]" | "#[proptest]")
        || squashed.ends_with("::test]")
}

fn parse_rust(source: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_rust::language()).ok()?;
    parser.parse(source, None)
}

/// The `.unwrap()` or stub macro `node` is, if it is one.
fn violation(node: Node, source: &Source) -> Option<(usize, &'static str, String)> {
    match node.kind() {
        "call_expression" => {
            let function = node.child_by_field_name("function")?;
            if function.kind() != "field_expression" {
                return None;
            }
            let field = function.child_by_field_name("field")?;
            let name = source.text(field);
            if !PANICKING_METHODS.contains(&name) {
                return None;
            }
            // A method taking arguments is somebody's own `unwrap`, not the one
            // in core that discards the reason.
            let arguments = node.child_by_field_name("arguments")?;
            if arguments.named_child_count() > 0 {
                return None;
            }
            Some((
                field.start_position().row + 1,
                "failure/rust-unwrap",
                format!(
                    "`.{name}()` aborts with no reason attached: match on it, propagate \
                     it with `?`, or say what cannot fail with `.expect(\"why\")` -- or \
                     write down why with `// {ALLOW_UNWRAP}`"
                ),
            ))
        }
        "macro_invocation" => {
            let name = source.text(node.child_by_field_name("macro")?);
            if !STUB_MACROS.contains(&name) {
                return None;
            }
            Some((
                node.start_position().row + 1,
                "dead/rust-stub",
                format!(
                    "`{name}!()` is scaffolding that type-checks: finish it, or write \
                     down why it stands with `// {ALLOW_STUB}`"
                ),
            ))
        }
        _ => None,
    }
}

/// Walk `node`, reporting only what is reachable outside test scope.
///
/// Attributes are siblings of the item they decorate in this grammar, so the
/// walk carries the run of attributes it has just passed and hands them to the
/// next `fn` or `mod` it reaches.
fn walk(node: Node, source: &Source, relative: &str, in_test: bool, out: &mut Vec<RustFinding>) {
    if !in_test {
        if let Some((line, rule, message)) = violation(node, source) {
            let marker = if rule == "failure/rust-unwrap" {
                ALLOW_UNWRAP
            } else {
                ALLOW_STUB
            };
            if !source.opted_out(line, marker) {
                out.push(RustFinding {
                    path: relative.to_string(),
                    line,
                    rule,
                    message,
                });
            }
        }
    }
    let mut pending = false;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "attribute_item" | "inner_attribute_item" => {
                pending = pending || marks_test(source.text(child));
                continue;
            }
            // A comment between an attribute and its item does not end the run.
            "line_comment" | "block_comment" => continue,
            _ => {}
        }
        let gated = matches!(child.kind(), "function_item" | "mod_item") && pending;
        pending = false;
        walk(child, source, relative, in_test || gated, out);
    }
}

/// Findings for one source, in tree order.
///
/// A source the grammar cannot parse is an error rather than an empty result: a
/// scanner that silently reports nothing for the file most likely to be broken
/// is worse than no scanner at all.
pub fn scan_source(text: &str, relative: &str) -> Result<Vec<RustFinding>, String> {
    let source = Source::new(text);
    let tree = parse_rust(text).ok_or_else(|| format!("rust scan cannot parse {relative}"))?;
    if tree.root_node().has_error() {
        return Err(format!(
            "rust scan cannot parse {relative}: the grammar reported a syntax error"
        ));
    }
    let mut out = Vec::new();
    walk(tree.root_node(), &source, relative, false, &mut out);
    Ok(out)
}

/// Findings for `paths`, sorted, so a caller merging several scanners can print
/// one pass down each file rather than one pass per scanner.
pub fn scan_paths(paths: &[String]) -> Result<Vec<RustFinding>, String> {
    let mut rows: Vec<RustFinding> = Vec::new();
    for raw in paths {
        let relative = raw.replace('\\', "/");
        if is_test_file(&relative) {
            continue;
        }
        let path = PathBuf::from(raw);
        let bytes = fs::read(&path)
            .map_err(|error| format!("rust scan cannot read {}: {error}", path.display()))?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        rows.extend(scan_source(&text, &relative)?);
    }
    rows.sort();
    Ok(rows)
}

/// `scan_paths` as the gate's Rust style check calls it.
#[pyfunction]
pub fn rust_scan_files(py: Python<'_>, paths: Vec<String>) -> PyResult<Py<PyList>> {
    let rows = scan_paths(&paths).map_err(PyValueError::new_err)?;
    let out = PyList::empty(py);
    for finding in rows {
        let row = PyDict::new(py);
        row.set_item("path", finding.path)?;
        row.set_item("line", finding.line)?;
        row.set_item("rule", finding.rule)?;
        row.set_item("message", finding.message)?;
        out.append(row)?;
    }
    Ok(out.unbind())
}

/// The rule names this scanner can emit, so the caller's published list and the
/// scanner cannot drift apart unnoticed.
#[pyfunction]
pub fn rust_scan_rules() -> Vec<String> {
    RULES.iter().map(|rule| (*rule).to_string()).collect()
}
