//! Comment-heavy function detector for Python sources.
//!
//! A function whose own-line comments outnumber its code lines usually explains a
//! sequence the code should have made obvious, or narrates work that belongs in a
//! commit message. No linter reads comments, so ruff cannot say so.
//!
//! The docstring is neither comment nor code. Machine directives are not read.
//! Nested functions are measured on their own and excluded from the enclosing one.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use tree_sitter::Node;

use crate::engine::parse;
use crate::style_scan::{comment_body, DIRECTIVES};

const RULE: &str = "comment/comment-heavy-function";

/// Fewer own-line comment lines than this is never worth a reader's attention.
const MIN_COMMENT_LINES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub path: String,
    pub line: usize,
    pub end_line: usize,
    pub rule: &'static str,
    pub message: String,
}

fn named<'t>(node: Node<'t>) -> Vec<Node<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// The leading string statement of a function body, if it has one.
fn docstring<'t>(func: Node<'t>) -> Option<Node<'t>> {
    let body = func.child_by_field_name("body")?;
    let first = named(body).into_iter().find(|n| n.kind() != "comment")?;
    if first.kind() != "expression_statement" {
        return None;
    }
    named(first)
        .into_iter()
        .next()
        .filter(|n| matches!(n.kind(), "string" | "concatenated_string"))
}

fn is_directive(source: &str, node: Node) -> bool {
    let raw = &source[node.start_byte()..node.end_byte()];
    let lowered = comment_body(raw).to_lowercase();
    raw.starts_with("#!") || DIRECTIVES.iter().any(|d| lowered.starts_with(d))
}

struct Tally {
    code_rows: BTreeSet<usize>,
    comment_rows: BTreeSet<usize>,
}

/// Mark the rows of `node` that carry code or an own-line-candidate comment,
/// skipping the docstring and any nested function.
fn tally(node: Node, doc: Option<Node>, source: &str, out: &mut Tally, root: bool) {
    if !root && node.kind() == "function_definition" {
        return;
    }
    if doc.is_some_and(|d| d.id() == node.id()) {
        return;
    }
    match node.kind() {
        "comment" => {
            if !is_directive(source, node) {
                out.comment_rows.insert(node.start_position().row);
            }
            return;
        }
        // A multi-line literal is code on every row it covers.
        "string" => {
            out.code_rows
                .extend(node.start_position().row..=node.end_position().row);
            return;
        }
        _ => {}
    }
    if node.child_count() == 0 {
        out.code_rows.insert(node.start_position().row);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        tally(child, doc, source, out, false);
    }
}

fn function_name<'a>(func: Node, source: &'a str) -> &'a str {
    func.child_by_field_name("name")
        .map(|n| &source[n.start_byte()..n.end_byte()])
        .unwrap_or("?")
}

fn scan_function(func: Node, source: &str, path: &str, out: &mut Vec<Finding>) {
    let mut t = Tally {
        code_rows: BTreeSet::new(),
        comment_rows: BTreeSet::new(),
    };
    tally(func, docstring(func), source, &mut t, true);
    // A trailing comment shares its row with code; only own-line comments count.
    let comments = t.comment_rows.difference(&t.code_rows).count();
    let code = t.code_rows.len();
    if comments >= MIN_COMMENT_LINES && comments > code {
        let line = func.start_position().row + 1;
        out.push(Finding {
            path: path.to_string(),
            line,
            end_line: func.end_position().row + 1,
            rule: RULE,
            message: format!(
                "`{}` has {comments} comment lines against {code} code lines; \
                 let the code say it, or move the explanation to a docstring",
                function_name(func, source)
            ),
        });
    }
}

pub fn scan_source(path: &str, source: &str) -> Result<Vec<Finding>, String> {
    let tree = parse(source).ok_or_else(|| format!("{path}: could not be parsed as Python"))?;
    if tree.root_node().has_error() {
        return Err(format!("{path}: is not valid Python"));
    }
    let mut findings = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_definition" {
            scan_function(node, source, path, &mut findings);
        }
        stack.extend(named(node));
    }
    findings.sort();
    Ok(findings)
}

/// Scan Python files and return one dict per finding, with the function's span.
#[pyfunction]
#[pyo3(signature = (paths, repo=None))]
pub fn verbosity_scan_files(
    py: Python<'_>,
    paths: Vec<String>,
    repo: Option<String>,
) -> PyResult<Py<PyList>> {
    let root = repo.map(PathBuf::from);
    let mut findings = Vec::new();
    for raw in paths {
        let path = PathBuf::from(&raw);
        let relative = match root.as_ref() {
            Some(root) => path
                .strip_prefix(root)
                .map_err(|_| {
                    PyValueError::new_err(format!(
                        "verbosity scan path {} is outside repository {}",
                        path.display(),
                        root.display()
                    ))
                })?
                .to_string_lossy()
                .replace('\\', "/"),
            None => path.to_string_lossy().replace('\\', "/"),
        };
        let bytes = fs::read(&path).map_err(|error| {
            PyValueError::new_err(format!(
                "verbosity scan cannot read {}: {error}",
                path.display()
            ))
        })?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        findings.extend(scan_source(&relative, &text).map_err(PyValueError::new_err)?);
    }
    findings.sort();

    let out = PyList::empty(py);
    for finding in findings {
        let row = PyDict::new(py);
        row.set_item("path", finding.path)?;
        row.set_item("line", finding.line)?;
        row.set_item("end_line", finding.end_line)?;
        row.set_item("rule", finding.rule)?;
        row.set_item("message", finding.message)?;
        out.append(row)?;
    }
    Ok(out.unbind())
}

#[pyfunction]
pub fn verbosity_scan_rules() -> Vec<&'static str> {
    vec![RULE]
}

#[cfg(test)]
mod tests {
    use super::scan_source;

    fn flagged(source: &str) -> usize {
        scan_source("probe.py", source).expect("probe parses").len()
    }

    fn comments(n: usize) -> String {
        (0..n)
            .map(|i| format!("    # step {i} of the plan\n"))
            .collect()
    }

    #[test]
    fn a_function_with_more_comment_lines_than_code_is_flagged() {
        let src = format!("def f(x):\n{}    return x\n", comments(9));
        let found = scan_source("probe.py", &src).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].line, found[0].end_line), (1, 11));
    }

    #[test]
    fn a_long_docstring_is_neither_comment_nor_code() {
        let doc: String = (0..30).map(|i| format!("    line {i}\n")).collect();
        let src = format!("def f(x):\n    \"\"\"Doc.\n{doc}    \"\"\"\n    return x\n");
        assert_eq!(flagged(&src), 0);
    }

    #[test]
    fn seven_comment_lines_is_below_the_floor() {
        let src = format!("def f(x):\n{}    return x\n", comments(7));
        assert_eq!(flagged(&src), 0);
    }

    #[test]
    fn directives_are_not_counted() {
        let tags: String = (0..9).map(|_| "    # noqa: E501\n".to_string()).collect();
        let src = format!("def f(x):\n{tags}    return x\n");
        assert_eq!(flagged(&src), 0);
    }

    #[test]
    fn commented_function_with_enough_code_is_not_flagged() {
        let code: String = (0..10).map(|i| format!("    y{i} = x\n")).collect();
        let src = format!("def f(x):\n{}{code}    return x\n", comments(9));
        assert_eq!(flagged(&src), 0);
    }

    #[test]
    fn a_nested_function_is_measured_apart_from_its_parent() {
        let src = format!(
            "def outer(x):\n    def inner(y):\n{}        return y\n    return inner\n",
            comments(9).replace("    #", "        #")
        );
        let found = scan_source("probe.py", &src).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 2);
    }
}
