//! Language-aware syntax projections shared by CLI and Python graph adapters.

use super::store::Symbol;
use crate::graph_index::extract;
use serde_json::{json, Value};

pub fn symbols(source: &str, language: &str) -> Result<Vec<Symbol>, String> {
    let facts = extract(source, language).map_err(|error| error.to_string())?;
    Ok(facts
        .definitions
        .into_iter()
        .map(|item| Symbol {
            qualified_name: item.qualified,
            name: item.name,
            kind: item.kind.to_owned(),
            line_start: item.line_start as i64,
            line_end: item.line_end as i64,
            signature: Some(item.signature),
        })
        .collect())
}

// Rust projection uses native byte spans; it never rescans a line prefix and
// never parses or visits a recursive syn AST. symbols() above is unchanged.
use crate::syntax::{self, Language};
use anyhow::{ensure, Context, Result};
use std::io::{self, Write};
use tree_sitter::Node;

const FORMAT: &str = "rust-signatures-v1";
#[derive(Clone, Copy)]
struct Limits {
    nodes: usize,
    depth: usize,
    definitions: usize,
    work: usize,
    output: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            nodes: 200_000,
            depth: 128,
            definitions: 10_000,
            work: 16 * 1024 * 1024,
            output: 1024 * 1024,
        }
    }
}
#[derive(Default, Debug)]
struct Stats {
    #[cfg(test)]
    nodes: usize,
    definitions: usize,
    work: usize,
}
fn admit(root: Node<'_>, limits: Limits) -> Result<usize> {
    let mut stack = vec![(root, 0)];
    let mut visited = 0;
    while let Some((node, depth)) = stack.pop() {
        visited += 1;
        ensure!(
            visited <= limits.nodes && depth <= limits.depth,
            "Rust skeleton node/depth limit exceeded"
        );
        ensure!(
            node.named_child_count() <= limits.nodes.saturating_sub(visited + stack.len()),
            "Rust skeleton node limit exceeded"
        );
        for index in (0..node.named_child_count()).rev() {
            stack.push((
                node.named_child(index as u32)
                    .context("missing native Rust node")?,
                depth + 1,
            ));
        }
    }
    Ok(visited)
}
struct RustProjection<'a> {
    source: &'a str,
    target: Option<&'a str>,
    scope: Vec<String>,
    output: String,
    symbols: Vec<String>,
    matched: bool,
    limits: Limits,
    stats: Stats,
}
impl RustProjection<'_> {
    fn charge(&mut self, work: usize) -> Result<()> {
        self.stats.work = self
            .stats
            .work
            .checked_add(work)
            .context("Rust projection work overflow")?;
        ensure!(
            self.stats.work <= self.limits.work,
            "Rust skeleton work limit exceeded"
        );
        Ok(())
    }
    fn span(&mut self, start: usize, end: usize) -> Result<&str> {
        ensure!(
            start <= end
                && end <= self.source.len()
                && self.source.is_char_boundary(start)
                && self.source.is_char_boundary(end),
            "invalid native Rust source span"
        );
        self.charge(end - start)?;
        Ok(&self.source[start..end])
    }
    fn append(&mut self, text: &str) -> Result<()> {
        self.charge(text.len())?;
        ensure!(
            text.len() <= self.limits.output.saturating_sub(self.output.len()),
            "Rust skeleton output byte limit exceeded"
        );
        self.output.push_str(text);
        Ok(())
    }
    fn line(&mut self, text: &str) -> Result<()> {
        for _ in 0..self.scope.len() {
            self.append("    ")?;
        }
        self.append(text)?;
        self.append("\n")
    }
    fn attributes(&mut self, attrs: &[Node<'_>]) -> Result<()> {
        for attr in attrs {
            let text = self.span(attr.start_byte(), attr.end_byte())?.to_owned();
            self.line(&text)?;
        }
        Ok(())
    }
    fn named(&mut self, name: &str) -> Result<bool> {
        self.stats.definitions += 1;
        ensure!(
            self.stats.definitions <= self.limits.definitions,
            "Rust skeleton definition count exceeded"
        );
        let qualified = if self.scope.is_empty() {
            name.to_owned()
        } else {
            format!("{}::{name}", self.scope.join("::"))
        };
        ensure!(
            qualified.len() <= 4096,
            "Rust skeleton qualified name byte limit exceeded"
        );
        self.charge(name.len() + qualified.len() + self.target.map_or(0, str::len))?;
        self.symbols.push(name.to_owned());
        let matches = self
            .target
            .is_none_or(|target| target == name || target == qualified);
        self.matched |= matches;
        Ok(matches)
    }
    fn function(&mut self, node: Node<'_>, attrs: &[Node<'_>]) -> Result<()> {
        let name = node
            .child_by_field_name("name")
            .context("Rust function lacks a name")?;
        let name = self.span(name.start_byte(), name.end_byte())?.to_owned();
        if !self.named(&name)? {
            return Ok(());
        }
        self.attributes(attrs)?;
        let body = node.child_by_field_name("body");
        let end = body.map_or(node.end_byte(), |body| body.start_byte());
        let header = self.span(node.start_byte(), end)?.trim_end().to_owned();
        if body.is_some() {
            self.line(&format!("{header} {{ ... }}"))?;
        } else {
            self.line(&header)?;
        }
        Ok(())
    }
    fn item(&mut self, node: Node<'_>, attrs: &[Node<'_>]) -> Result<()> {
        if matches!(node.kind(), "struct_item" | "enum_item") {
            let name = node
                .child_by_field_name("name")
                .context("Rust item lacks a name")?;
            let name = self.span(name.start_byte(), name.end_byte())?.to_owned();
            if !self.named(&name)? {
                return Ok(());
            }
        } else if node.kind() == "mod_item" && self.target.is_some() {
            return Ok(());
        }
        self.attributes(attrs)?;
        let text = self.span(node.start_byte(), node.end_byte())?.to_owned();
        self.line(&text)
    }
    fn container(&mut self, node: Node<'_>, attrs: &[Node<'_>]) -> Result<()> {
        self.stats.definitions += 1;
        ensure!(
            self.stats.definitions <= self.limits.definitions,
            "Rust skeleton definition count exceeded"
        );
        let body = node
            .child_by_field_name("body")
            .context("Rust container lacks body")?;
        let name = if node.kind() == "impl_item" {
            node.child_by_field_name("type")
        } else {
            node.child_by_field_name("name")
        }
        .context("Rust container lacks scope")?;
        let name = self.span(name.start_byte(), name.end_byte())?.to_owned();
        ensure!(
            name.len() <= 4096,
            "Rust skeleton scope byte limit exceeded"
        );
        let before = self.output.len();
        self.attributes(attrs)?;
        let header = self
            .span(node.start_byte(), body.start_byte())?
            .trim_end()
            .to_owned();
        self.line(&format!("{header} {{"))?;
        self.scope.push(name);
        let content_start = self.output.len();
        self.visit(body)?;
        self.scope.pop();
        if self.output.len() == content_start {
            self.output.truncate(before);
        } else {
            self.line("}")?;
        }
        Ok(())
    }
    fn visit(&mut self, container: Node<'_>) -> Result<()> {
        let mut attrs = Vec::new();
        for index in 0..container.named_child_count() {
            let node = container
                .named_child(index as u32)
                .context("missing Rust container child")?;
            match node.kind() {
                "attribute_item" | "inner_attribute_item" => {
                    attrs.push(node);
                    continue;
                }
                "line_comment" | "block_comment" => {
                    let text = &self.source[node.byte_range()];
                    if ["///", "//!", "/**", "/*!"]
                        .into_iter()
                        .any(|prefix| text.starts_with(prefix))
                    {
                        attrs.push(node);
                    }
                    continue;
                }
                "function_item" | "function_signature_item" => self.function(node, &attrs)?,
                "impl_item" | "trait_item" => self.container(node, &attrs)?,
                "mod_item" if node.child_by_field_name("body").is_some() => {
                    self.container(node, &attrs)?
                }
                "use_declaration" | "struct_item" | "enum_item" | "mod_item" => {
                    self.item(node, &attrs)?
                }
                _ => {}
            }
            attrs.clear();
        }
        Ok(())
    }
}
struct ResultBudget(usize);
impl Write for ResultBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > (4 * 1024 * 1024usize).saturating_sub(self.0) {
            return Err(io::Error::other("Rust skeleton result byte limit exceeded"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn project(source: &str, target: Option<&str>, limits: Limits) -> Result<(Value, Stats)> {
    ensure!(
        target.is_none_or(|target| target.len() <= 4096 && !target.chars().any(char::is_control)),
        "invalid Rust skeleton selector"
    );
    let tree = syntax::parse(source, Language::Rust)?;
    let nodes = admit(tree.root_node(), limits)?;
    ensure!(nodes <= limits.work, "Rust skeleton work limit exceeded");
    let mut projection = RustProjection {
        source,
        target: target.filter(|v| !v.is_empty()),
        scope: Vec::new(),
        output: String::new(),
        symbols: Vec::new(),
        matched: false,
        limits,
        stats: Stats {
            #[cfg(test)]
            nodes,
            definitions: 0,
            work: nodes,
        },
    };
    projection.visit(tree.root_node())?;
    ensure!(
        projection.target.is_none() || projection.matched,
        "symbol {:?} not found",
        projection.target.unwrap_or_default()
    );
    let result = json!({"skeleton":projection.output.trim_end_matches('\n'),
        "symbols":projection.symbols,"language":"rust","format":FORMAT});
    serde_json::to_writer(&mut ResultBudget(0), &result)?;
    Ok((result, projection.stats))
}
pub fn rust(source: &str, target: Option<&str>) -> Result<Value, String> {
    project(source, target, Limits::default())
        .map(|(value, _)| value)
        .map_err(|error| error.to_string())
}
#[cfg(test)]
#[path = "graph_context_rust_tests.rs"]
mod tests;
