//! Tests that cannot fail: a `test_*` function that asserts nothing, and an
//! assert whose condition is a constant.
//!
//! A test with no assertion passes whatever the code under test does, so it
//! counts toward coverage and toward "N tests passed" while checking nothing.
//! Mutation testing finds these one campaign at a time; this reads the parse
//! tree and names them at the commit that writes them.
//!
//! Findings carry the function's line span (`line`..=`end_line`), so the CLI can
//! hold a candidate to a test it edited anywhere inside, not only on its `def`.

use std::fs;
use std::path::PathBuf;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use tree_sitter::Node;

use crate::engine::parse;
use crate::style_scan::is_test_path;

/// Escape hatches, spelled like the `guardrail: allow-*` ones style_scan honours.
const ALLOW_NO_ASSERT: &str = "guardrail: allow-no-assert";
const ALLOW_ALWAYS_TRUE: &str = "guardrail: allow-always-true";

/// Helper-name prefixes that mean "this call fails the test when it is wrong".
const HELPER_PREFIXES: &[&str] = &["check", "expect", "verify", "require"];

/// Calls that assert by raising, matched on the whole dotted name.
const RAISING_CALLS: &[&str] = &[
    "pytest.raises",
    "pytest.warns",
    "pytest.deprecated_call",
    "pytest.fail",
    "raises",
    "warns",
    "deprecated_call",
    "fail",
    "self.fail",
];

/// A line-span rule violation. `end_line` is inclusive and one-based.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub path: String,
    pub line: usize,
    pub end_line: usize,
    pub rule: &'static str,
    pub message: String,
}

struct Source<'a> {
    src: &'a str,
    lines: Vec<&'a str>,
}

impl<'a> Source<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            lines: src.lines().collect(),
        }
    }

    fn text(&self, node: Node) -> &'a str {
        &self.src[node.byte_range()]
    }

    /// True when `token` sits on any of the one-based `rows` or the line above it.
    fn hatched(&self, token: &str, rows: &[usize]) -> bool {
        rows.iter().any(|&row| {
            [row, row.saturating_sub(1)]
                .into_iter()
                .filter(|&n| n > 0)
                .any(|n| self.lines.get(n - 1).is_some_and(|l| l.contains(token)))
        })
    }
}

fn kids(node: Node) -> Vec<Node> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn start_line(node: Node) -> usize {
    node.start_position().row + 1
}

fn end_line(node: Node) -> usize {
    node.end_position().row + 1
}

/// True when a call named `callee` fails the surrounding test on a bad result.
fn asserting_callee(callee: &str) -> bool {
    if RAISING_CALLS.contains(&callee) {
        return true;
    }
    asserting_name(callee.rsplit('.').next().unwrap_or(callee))
}

/// True when `last` is named like an assertion helper. Used for calls and for
/// bare references, because `run_cases(_check_a, _check_b)` hands the checkers to
/// a runner that calls them.
fn asserting_name(last: &str) -> bool {
    let name = last.trim_start_matches('_').to_lowercase();
    if name.starts_with("assert") {
        return true;
    }
    // `check` must be a word of its own: `checkpoint(...)` and `expected_x(...)`
    // are not assertion helpers, `check_close(...)` and `expectEqual(...)` are.
    HELPER_PREFIXES.iter().any(|prefix| {
        name.strip_prefix(prefix).is_some_and(|rest| {
            rest.is_empty()
                || rest.starts_with('_')
                || last
                    .trim_start_matches('_')
                    .chars()
                    .nth(prefix.len())
                    .is_some_and(|c| c.is_uppercase())
        })
    })
}

/// True when anything under `body` can fail the test.
fn asserts(source: &Source, body: Node) -> bool {
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        match node.kind() {
            // An explicit `raise` is a deliberate failure path.
            "assert_statement" | "raise_statement" => return true,
            "call" => {
                if let Some(callee) = node.child_by_field_name("function") {
                    if asserting_callee(source.text(callee)) {
                        return true;
                    }
                }
            }
            "identifier" if asserting_name(source.text(node)) => return true,
            _ => {}
        }
        stack.extend(kids(node));
    }
    false
}

fn contains_call(node: Node) -> bool {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "call" | "await" | "yield") {
            return true;
        }
        stack.extend(kids(n));
    }
    false
}

fn nonzero_number(text: &str) -> bool {
    let lower = text.to_lowercase();
    let digits = match lower.get(..2) {
        Some("0x" | "0b" | "0o") => &lower[2..],
        _ => lower.split('e').next().unwrap_or(&lower),
    };
    digits
        .chars()
        .any(|c| !matches!(c, '0' | '_' | '.' | 'j' | 'l'))
}

fn has_interpolation(node: Node) -> bool {
    kids(node).iter().any(|c| c.kind() == "interpolation")
}

/// Why `expr` is true whatever the program does, or None.
fn constant_truth(source: &Source, expr: Node) -> Option<String> {
    let mut expr = expr;
    while expr.kind() == "parenthesized_expression" {
        expr = kids(expr).first().copied()?;
    }
    let text = source.text(expr);
    match expr.kind() {
        "true" => Some("`True`".to_string()),
        "integer" | "float" if nonzero_number(text) => Some(format!("the number `{text}`")),
        "string"
            if !has_interpolation(expr)
                && kids(expr).iter().any(|c| c.kind() == "string_content") =>
        {
            Some("a non-empty string".to_string())
        }
        "tuple" | "list" if expr.named_child_count() > 0 => {
            Some(format!("a non-empty {}", expr.kind()))
        }
        "comparison_operator" => identical_operands(source, expr),
        _ => None,
    }
}

/// `x == x` and `x is x`, where the operand is a plain expression: a call may
/// return a fresh value each time, so `f() == f()` is a real determinism check.
fn identical_operands(source: &Source, cmp: Node) -> Option<String> {
    let mut cursor = cmp.walk();
    let children: Vec<Node> = cmp.children(&mut cursor).collect();
    if children.len() != 3 || !matches!(children[1].kind(), "==" | "is") {
        return None;
    }
    let (left, right) = (children[0], children[2]);
    if contains_call(left) || source.text(left) != source.text(right) {
        return None;
    }
    Some(format!(
        "`{}` compared with itself",
        source
            .text(left)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    ))
}

fn scan_assert(source: &Source, node: Node, path: &str, out: &mut Vec<Finding>) {
    let Some(condition) = kids(node).first().copied() else {
        return;
    };
    let Some(why) = constant_truth(source, condition) else {
        return;
    };
    let line = start_line(node);
    if source.hatched(ALLOW_ALWAYS_TRUE, &[line]) {
        return;
    }
    out.push(Finding {
        path: path.to_string(),
        line,
        end_line: end_line(node),
        rule: "test/always-true-assert",
        message: format!(
            "this assert is true whatever the code does ({why}): assert the value the \
             code under test produced -- or write down why with `# {ALLOW_ALWAYS_TRUE}`"
        ),
    });
}

fn scan_test(source: &Source, def: Node, outer: Node, path: &str, out: &mut Vec<Finding>) {
    let Some(name) = def.child_by_field_name("name").map(|n| source.text(n)) else {
        return;
    };
    let Some(body) = def.child_by_field_name("body") else {
        return;
    };
    if !name.starts_with("test_") || asserts(source, body) {
        return;
    }
    // The hatch may sit above the decorators as well as above the `def`.
    if source.hatched(ALLOW_NO_ASSERT, &[start_line(def), start_line(outer)]) {
        return;
    }
    out.push(Finding {
        path: path.to_string(),
        line: start_line(def),
        end_line: end_line(def),
        rule: "test/no-assertion",
        message: format!(
            "`{name}` has no assert, no pytest.raises/warns/fail, no self.assert* and no \
             assert/check/expect/verify/require helper call, so it passes whatever the code \
             does: assert the result -- or write down why with `# {ALLOW_NO_ASSERT}`"
        ),
    });
}

/// Module-level `test_*` functions and those of `Test*` classes, decorators peeled.
fn collect_tests<'t>(
    source: &Source,
    parent: Node<'t>,
    in_class: bool,
    out: &mut Vec<(Node<'t>, Node<'t>)>,
) {
    for node in kids(parent) {
        let inner = if node.kind() == "decorated_definition" {
            node.child_by_field_name("definition").unwrap_or(node)
        } else {
            node
        };
        match inner.kind() {
            "function_definition" => out.push((inner, node)),
            "class_definition" if !in_class => {
                let named_test = inner
                    .child_by_field_name("name")
                    .is_some_and(|n| source.text(n).starts_with("Test"));
                if let (true, Some(body)) = (named_test, inner.child_by_field_name("body")) {
                    collect_tests(source, body, true, out);
                }
            }
            _ => {}
        }
    }
}

/// Scan one file. Non-test paths produce no findings.
pub fn scan_source(path: &str, source_text: &str) -> Result<Vec<Finding>, String> {
    if !is_test_path(path) {
        return Ok(Vec::new());
    }
    let tree =
        parse(source_text).ok_or_else(|| format!("{path}: could not be parsed as Python"))?;
    // An error-tolerant tree would report nothing for the file most likely broken.
    if tree.root_node().has_error() {
        return Err(format!("{path}: is not valid Python"));
    }
    let source = Source::new(source_text);
    let mut findings = Vec::new();

    let mut tests = Vec::new();
    collect_tests(&source, tree.root_node(), false, &mut tests);
    for (def, outer) in tests {
        scan_test(&source, def, outer, path, &mut findings);
    }

    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.kind() == "assert_statement" {
            scan_assert(&source, node, path, &mut findings);
        }
        stack.extend(kids(node));
    }
    findings.sort();
    Ok(findings)
}

/// Scan Python files and return one dict per finding, with `end_line`.
///
/// `repo`, when given, is stripped from each path so findings are addressed the
/// way the gate and a reviewer both name files.
#[pyfunction]
#[pyo3(signature = (paths, repo=None))]
pub fn test_value_scan_files(
    py: Python<'_>,
    paths: Vec<String>,
    repo: Option<String>,
) -> PyResult<Py<PyList>> {
    let root = repo.map(PathBuf::from);
    let mut findings = Vec::new();
    for raw in paths {
        let path = PathBuf::from(&raw);
        let shown = match root.as_ref() {
            Some(root) => path.strip_prefix(root).map_err(|_| {
                PyValueError::new_err(format!(
                    "test value scan path {} is outside repository {}",
                    path.display(),
                    root.display()
                ))
            })?,
            None => path.as_path(),
        };
        let relative = shown.to_string_lossy().replace('\\', "/");
        let bytes = fs::read(&path).map_err(|error| {
            PyValueError::new_err(format!(
                "test value scan cannot read {}: {error}",
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

/// Every rule this module can emit.
#[pyfunction]
pub fn test_value_scan_rules() -> Vec<&'static str> {
    vec!["test/always-true-assert", "test/no-assertion"]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(source: &str) -> Vec<String> {
        scan_source("tests/test_probe.py", source)
            .expect("probe parses")
            .into_iter()
            .map(|f| format!("{}:{}-{}", f.rule, f.line, f.end_line))
            .collect()
    }

    fn fires(source: &str, rule: &str) -> bool {
        rules(source).iter().any(|r| r.starts_with(rule))
    }

    fn clean(source: &str) -> bool {
        rules(source).is_empty()
    }

    #[test]
    fn a_test_with_no_assertion_is_flagged_with_its_span() {
        let found = rules("def test_a():\n    x = 1\n    run(x)\n");
        assert_eq!(found, vec!["test/no-assertion:1-3"]);
    }

    #[test]
    fn a_test_method_in_a_test_class_is_flagged() {
        assert!(fires(
            "class TestA:\n    def test_a(self):\n        run()\n",
            "test/no-assertion"
        ));
    }

    #[test]
    fn a_method_of_a_non_test_class_or_a_non_test_name_is_ignored() {
        assert!(clean(
            "class Helper:\n    def test_a(self):\n        run()\n"
        ));
        assert!(clean("def helper():\n    run()\n"));
    }

    #[test]
    fn every_assertion_form_clears_the_rule() {
        for body in [
            "assert run()",
            "with pytest.raises(ValueError):\n        run()",
            "with pytest.warns(UserWarning):\n        run()",
            "with pytest.deprecated_call():\n        run()",
            "pytest.fail('no')",
            "self.assertEqual(run(), 1)",
            "self.assertTrue(run())",
            "self.fail('no')",
            "assert_close(run(), 1)",
            "torch.testing.assert_close(run(), 1)",
            "_assert_ok(run())",
            "check_result(run())",
            "self.checkOutput(run())",
            "expect_equal(run(), 1)",
            "verify_all(run())",
            "require_gpu()",
            "mock.assert_called_once()",
            "raise AssertionError('no')",
            "def inner():\n        assert run()\n    inner()",
            "run_cases(_check_a, verify_b)",
        ] {
            let src = format!("def test_a(self):\n    {body}\n");
            assert!(
                !fires(&src, "test/no-assertion"),
                "body {body:?} should count"
            );
        }
    }

    #[test]
    fn a_look_alike_helper_name_does_not_count() {
        for body in [
            "torch.utils.checkpoint(run)",
            "expected_value(run())",
            "required = run()",
        ] {
            let src = format!("def test_a():\n    {body}\n");
            assert!(
                fires(&src, "test/no-assertion"),
                "body {body:?} should not count"
            );
        }
    }

    #[test]
    fn the_hatch_on_the_def_or_above_it_or_above_a_decorator_waives() {
        assert!(clean(
            "def test_a():  # guardrail: allow-no-assert\n    run()\n"
        ));
        assert!(clean(
            "# guardrail: allow-no-assert\ndef test_a():\n    run()\n"
        ));
        assert!(clean(
            "# guardrail: allow-no-assert\n@mark\ndef test_a():\n    run()\n"
        ));
        assert!(fires(
            "# guardrail: allow-no-assert\n\n\ndef test_a():\n    run()\n",
            "test/no-assertion"
        ));
    }

    #[test]
    fn a_decorated_test_is_still_checked() {
        assert!(fires(
            "@pytest.mark.parametrize('x', [1])\ndef test_a(x):\n    run(x)\n",
            "test/no-assertion"
        ));
    }

    #[test]
    fn constant_truths_are_always_true_asserts() {
        for cond in [
            "True",
            "(True)",
            "1",
            "2.5",
            "0x10",
            "'ok'",
            "(a, 'msg')",
            "[1]",
            "x == x",
            "x is x",
            "a.b[0] == a.b[0]",
        ] {
            let src = format!("def test_a():\n    assert {cond}\n");
            assert!(
                fires(&src, "test/always-true-assert"),
                "assert {cond} should fire"
            );
        }
        assert!(fires(
            "def test_a():\n    assert True, 'msg'\n",
            "test/always-true-assert"
        ));
    }

    #[test]
    fn a_real_condition_is_not_an_always_true_assert() {
        for cond in [
            "False",
            "0",
            "0.0",
            "0x0",
            "''",
            "()",
            "[]",
            "None",
            "x",
            "x == y",
            "f() == f()",
            "x != x",
            "f'{x}'",
            "x is not x",
            "run()",
        ] {
            let src = format!("def test_a():\n    assert {cond}\n");
            assert!(
                !fires(&src, "test/always-true-assert"),
                "assert {cond} should not fire"
            );
        }
    }

    #[test]
    fn the_always_true_hatch_waives_and_the_assert_still_counts_as_one() {
        let src = "def test_a():\n    assert True  # guardrail: allow-always-true\n";
        assert!(clean(src));
        assert_eq!(
            rules("def test_a():\n    assert True\n"),
            vec!["test/always-true-assert:2-2"]
        );
    }

    #[test]
    fn an_always_true_assert_outside_a_test_function_is_found() {
        assert!(fires(
            "def helper():\n    assert 1\n",
            "test/always-true-assert"
        ));
    }

    #[test]
    fn non_test_paths_and_broken_files_are_handled() {
        assert!(scan_source("src/mod.py", "def test_a():\n    run()\n")
            .unwrap()
            .is_empty());
        assert!(scan_source("tests/test_x.py", "def test_a(:\n").is_err());
        assert!(
            scan_source("pkg/foo_test.py", "def test_a():\n    run()\n")
                .unwrap()
                .len()
                == 1
        );
    }

    #[test]
    fn rules_match_the_implementation() {
        let listed = test_value_scan_rules();
        let src = "def test_a():\n    assert True\n";
        for found in scan_source("tests/test_x.py", src).unwrap() {
            assert!(listed.contains(&found.rule));
        }
        assert!(listed.contains(&"test/no-assertion"));
    }
}
