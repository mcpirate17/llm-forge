//! Source-preserving Python context projection, deliberately not CPython unparse.
//! Rendered snippets are context data and must never be executed.
//! Module docstrings are intentionally retained (the retired adapter dropped
//! them). Class/function docstrings retain their literal source spelling,
//! including parentheses; f-strings and byte literals are never docstrings.
use crate::syntax::{self, Language};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use tree_sitter::Node;

const MAX_NODES: usize = 200_000;
const MAX_DEPTH: usize = 256;
const MAX_DEFINITIONS: usize = 10_000;
const MAX_ANCESTORS: usize = 200_000;
const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;

struct Definition<'a> {
    wrapper: Node<'a>,
    node: Node<'a>,
    name: String,
    qualified: String,
    ancestors: Vec<usize>,
    class: bool,
}
struct Projection<'a> {
    source: &'a str,
    definitions: Vec<Definition<'a>>,
    selected: BTreeSet<usize>,
    output: String,
    output_limit: usize,
}
fn text<'a>(source: &'a str, node: Node<'_>) -> &'a str {
    &source[node.byte_range()]
}
fn definition(node: Node<'_>) -> Option<Node<'_>> {
    let node = if node.kind() == "decorated_definition" {
        node.child_by_field_name("definition")?
    } else {
        node
    };
    matches!(node.kind(), "class_definition" | "function_definition").then_some(node)
}
fn admit(root: Node<'_>) -> Result<()> {
    let mut stack = vec![(root, 0_usize)];
    let mut count = 0;
    let deadline = Instant::now() + Duration::from_millis(250);
    while let Some((node, depth)) = stack.pop() {
        count += 1;
        ensure!(
            count <= MAX_NODES && depth <= MAX_DEPTH,
            "Python context node/depth limit exceeded"
        );
        if count % 256 == 0 {
            ensure!(
                Instant::now() < deadline,
                "Python context traversal deadline exceeded"
            );
        }
        ensure!(
            node.named_child_count() <= MAX_NODES.saturating_sub(count + stack.len()),
            "Python context node limit exceeded"
        );
        for index in (0..node.named_child_count()).rev() {
            stack.push((
                node.named_child(index as u32)
                    .context("missing Python AST child")?,
                depth + 1,
            ));
        }
    }
    Ok(())
}
fn collect<'a>(
    source: &str,
    container: Node<'a>,
    scope: &str,
    ancestors: &[usize],
    result: &mut Vec<Definition<'a>>,
    ancestor_count: &mut usize,
    symbol_bytes: &mut usize,
) -> Result<()> {
    for index in 0..container.named_child_count() {
        let wrapper = container
            .named_child(index as u32)
            .context("missing Python AST child")?;
        let Some(node) = definition(wrapper) else {
            continue;
        };
        let name = text(
            source,
            node.child_by_field_name("name")
                .context("Python definition has no name")?,
        )
        .to_owned();
        let qualified = if scope.is_empty() {
            name.clone()
        } else {
            format!("{scope}.{name}")
        };
        *symbol_bytes += name.len() + qualified.len();
        ensure!(
            qualified.len() <= 4096 && *symbol_bytes <= 8 * 1024 * 1024,
            "Python context symbol byte limit exceeded"
        );
        let class = node.kind() == "class_definition";
        ensure!(
            result.len() < MAX_DEFINITIONS,
            "Python context definition count exceeded"
        );
        *ancestor_count += ancestors.len();
        ensure!(
            *ancestor_count <= MAX_ANCESTORS,
            "Python context scope budget exceeded"
        );
        result.push(Definition {
            wrapper,
            node,
            name,
            qualified: qualified.clone(),
            ancestors: ancestors.to_vec(),
            class,
        });
        // Function bodies are never searched for nested implementation definitions.
        if class {
            let mut parents = ancestors.to_vec();
            parents.push(wrapper.start_byte());
            collect(
                source,
                node.child_by_field_name("body")
                    .context("Python class has no body")?,
                &qualified,
                &parents,
                result,
                ancestor_count,
                symbol_bytes,
            )?;
        }
    }
    Ok(())
}
fn selected(definitions: &[Definition<'_>], target: Option<&str>) -> Result<BTreeSet<usize>> {
    let Some(target) = target.filter(|value| !value.is_empty()) else {
        return Ok(definitions.iter().map(|d| d.wrapper.start_byte()).collect());
    };
    ensure!(
        target.len() <= 4096 && !target.chars().any(char::is_control),
        "invalid Python context selector"
    );
    let mut direct = BTreeSet::new();
    let mut full_classes = BTreeSet::new();
    for found in definitions
        .iter()
        .filter(|d| d.qualified == target || (!target.contains('.') && d.name == target))
    {
        direct.insert(found.wrapper.start_byte());
        if found.class {
            full_classes.insert(found.wrapper.start_byte());
        }
    }
    ensure!(!direct.is_empty(), "symbol {target:?} not found");
    let mut selected = BTreeSet::new();
    for found in definitions {
        if direct.contains(&found.wrapper.start_byte())
            || found.ancestors.iter().any(|p| full_classes.contains(p))
        {
            // Selecting a class includes its methods, unlike the retired adapter.
            selected.insert(found.wrapper.start_byte());
            selected.extend(&found.ancestors);
        }
    }
    Ok(selected)
}
fn indentation<'a>(source: &'a str, node: Node<'_>) -> &'a str {
    let start = source[..node.start_byte()].rfind('\n').map_or(0, |n| n + 1);
    let prefix = &source[start..node.start_byte()];
    if prefix
        .bytes()
        .all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\x0c'))
    {
        prefix
    } else {
        ""
    }
}
fn body_indentation<'a>(source: &'a str, node: Node<'_>, fallback: &'a str) -> &'a str {
    let original = indentation(source, node);
    if original.is_empty() {
        fallback
    } else {
        original
    }
}
fn docstring(source: &str, statement: Node<'_>) -> bool {
    if statement.kind() != "expression_statement" || statement.named_child_count() != 1 {
        return false;
    }
    let Some(mut value) = statement.named_child(0) else {
        return false;
    };
    for _ in 0..MAX_DEPTH {
        if value.kind() != "parenthesized_expression" {
            break;
        }
        if value.named_child_count() != 1 {
            return false;
        }
        let Some(child) = value.named_child(0) else {
            return false;
        };
        value = child;
    }
    if !matches!(value.kind(), "string" | "concatenated_string") {
        return false;
    }
    let mut cursor = value.walk();
    let strings: Vec<_> = if value.kind() == "string" {
        vec![value]
    } else {
        value.named_children(&mut cursor).collect()
    };
    strings.into_iter().all(|part| {
        let raw = text(source, part);
        let prefix = raw.split(['\'', '"']).next().unwrap_or(raw);
        !prefix
            .bytes()
            .any(|b| matches!(b, b'b' | b'B' | b'f' | b'F'))
    })
}
fn assignment(statement: Node<'_>) -> Option<Node<'_>> {
    if statement.kind() != "expression_statement" {
        return None;
    }
    let node = statement.named_child(0)?;
    (node.kind() == "assignment").then_some(node)
}
fn scalar(source: &str, node: Node<'_>) -> bool {
    match node.kind() {
        "integer" | "float" | "true" | "false" | "none" | "ellipsis" => true,
        "string" => !text(source, node)
            .split(['\'', '"'])
            .next()
            .unwrap_or("")
            .bytes()
            .any(|b| matches!(b, b'f' | b'F')),
        "concatenated_string" => {
            let mut cursor = node.walk();
            let all = node
                .named_children(&mut cursor)
                .all(|child| scalar(source, child));
            all
        }
        "parenthesized_expression" => {
            node.named_child_count() == 1
                && node
                    .named_child(0)
                    .is_some_and(|child| scalar(source, child))
        }
        "unary_operator" => node
            .named_child(0)
            .is_some_and(|n| matches!(n.kind(), "integer" | "float")),
        _ => false,
    }
}
impl Projection<'_> {
    fn append(&mut self, value: &str) -> Result<()> {
        ensure!(
            value.len() <= self.output_limit.saturating_sub(self.output.len()),
            "Python skeleton output byte limit exceeded"
        );
        self.output.push_str(value);
        Ok(())
    }
    fn line(&mut self, indent: &str, value: &str) -> Result<()> {
        self.append(indent)?;
        self.append(value)?;
        self.append("\n")
    }
    fn definition(&mut self, wrapper: Node<'_>) -> Result<()> {
        if !self.selected.contains(&wrapper.start_byte()) {
            return Ok(());
        }
        let node = definition(wrapper).context("invalid Python definition")?;
        let body = node
            .child_by_field_name("body")
            .context("Python definition has no body")?;
        let indent = indentation(self.source, wrapper).to_owned();
        let header = self.source[wrapper.start_byte()..body.start_byte()]
            .trim_end()
            .to_owned();
        self.line(&indent, &header)?;
        let default_indent = format!("{indent}    ");
        let body_indent = body_indentation(
            self.source,
            first_statement(body).unwrap_or(body),
            &default_indent,
        )
        .to_owned();
        if node.kind() == "class_definition" {
            let before = self.output.len();
            self.container(body, true, &body_indent)?;
            if self.output.len() == before {
                self.line(&body_indent, "...")?;
            }
        } else {
            if let Some(first) = first_statement(body) {
                if docstring(self.source, first) {
                    let doc = text(self.source, first).to_owned();
                    self.line(&body_indent, &doc)?;
                }
            }
            self.line(&body_indent, "...")?;
        }
        Ok(())
    }
    fn assignment(&mut self, statement: Node<'_>, class: bool, fallback: &str) -> Result<()> {
        let node = assignment(statement).context("invalid Python assignment")?;
        let indent = body_indentation(self.source, statement, fallback).to_owned();
        if class || node.child_by_field_name("type").is_some() {
            return self.line(&indent, text(self.source, statement));
        }
        let mut value = node
            .child_by_field_name("right")
            .context("Python assignment lacks a value")?;
        for _ in 0..MAX_DEPTH {
            if value.kind() != "assignment" {
                break;
            }
            value = value
                .child_by_field_name("right")
                .context("Python assignment lacks a value")?;
        }
        if scalar(self.source, value) {
            return self.line(&indent, text(self.source, statement));
        }
        let prefix = self.source[statement.start_byte()..value.start_byte()].to_owned();
        self.line(&indent, &format!("{prefix}..."))
    }
    fn container(&mut self, container: Node<'_>, class: bool, fallback: &str) -> Result<()> {
        let first = first_statement(container).map(|n| n.start_byte());
        for index in 0..container.named_child_count() {
            let node = container
                .named_child(index as u32)
                .context("missing Python AST child")?;
            if definition(node).is_some() {
                self.definition(node)?;
            } else if matches!(
                node.kind(),
                "import_statement" | "import_from_statement" | "future_import_statement"
            ) || (Some(node.start_byte()) == first && docstring(self.source, node))
            {
                let indent = body_indentation(self.source, node, fallback).to_owned();
                self.line(&indent, text(self.source, node))?;
            } else if assignment(node).is_some() {
                self.assignment(node, class, fallback)?;
            }
        }
        Ok(())
    }
}
fn first_statement(container: Node<'_>) -> Option<Node<'_>> {
    (0..container.named_child_count())
        .filter_map(|i| container.named_child(i as u32))
        .find(|node| node.kind() != "comment")
}
/// Python signatures retain original syntax, quotes and decorator layout.
/// Symbols enumerate projectable definitions in source order before selection.
pub fn python(source: &str, target: Option<&str>) -> Result<Value, String> {
    project(source, target).map_err(|error| error.to_string())
}
fn project(source: &str, target: Option<&str>) -> Result<Value> {
    project_with_limit(source, target, MAX_OUTPUT)
}
fn project_with_limit(source: &str, target: Option<&str>, output_limit: usize) -> Result<Value> {
    ensure!(
        (1..=MAX_OUTPUT).contains(&output_limit),
        "invalid Python context output limit"
    );
    let tree = syntax::parse(source, Language::Python)?;
    admit(tree.root_node())?;
    let mut definitions = Vec::new();
    collect(
        source,
        tree.root_node(),
        "",
        &[],
        &mut definitions,
        &mut 0,
        &mut 0,
    )?;
    let selected = selected(&definitions, target)?;
    let mut projection = Projection {
        source,
        definitions,
        selected,
        output: String::new(),
        output_limit,
    };
    projection.container(tree.root_node(), false, "")?;
    let definitions: Vec<_> = projection
        .definitions
        .iter()
        .map(|d| {
            json!({"name":d.name,
        "qualified_name":d.qualified,"kind":if d.class {"Class"} else {"Function"},
        "line_start":d.wrapper.start_position().row+1,
        "line_end":d.node.end_position().row+usize::from(d.node.end_position().column>0)})
        })
        .collect();
    let result = json!({"language":"python","format":"source-preserving-v1",
        "skeleton":projection.output.trim_end_matches('\n'),
        "symbols":projection.definitions.iter().map(|d|&d.name).collect::<Vec<_>>(),
        "definitions":definitions});
    ensure!(
        serde_json::to_vec(&result)?.len() <= MAX_RESULT_BYTES,
        "Python context result byte limit exceeded"
    );
    Ok(result)
}

#[cfg(test)]
#[path = "graph_context_python_tests.rs"]
mod tests;
