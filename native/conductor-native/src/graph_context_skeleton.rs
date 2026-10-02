//! Language-aware syntax projections shared by CLI and Python graph adapters.

use super::store::Symbol;
use crate::graph_index::{extract_python, extract_rust};
use serde_json::{json, Value};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};

pub fn symbols(source: &str, language: &str) -> Result<Vec<Symbol>, String> {
    let facts = match language {
        "python" => extract_python(source),
        "rust" => extract_rust(source),
        _ => return Err(format!("unsupported skeleton language: {language}")),
    }
    .map_err(|error| error.to_string())?;
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

fn offset(source: &str, position: proc_macro2::LineColumn) -> usize {
    let line = source
        .split_inclusive('\n')
        .take(position.line.saturating_sub(1))
        .map(str::len)
        .sum::<usize>();
    line + source[line..]
        .char_indices()
        .nth(position.column)
        .map_or(source[line..].len(), |(index, _)| index)
}

fn span_text(source: &str, span: proc_macro2::Span) -> &str {
    &source[offset(source, span.start())..offset(source, span.end())]
}

struct RustSkeleton<'a> {
    source: &'a str,
    target: Option<&'a str>,
    scope: Vec<String>,
    rendered: Vec<String>,
    symbols: Vec<String>,
    matched: bool,
}

impl RustSkeleton<'_> {
    fn matches(&self, name: &str) -> bool {
        self.target.is_none_or(|target| {
            target == name || target == format!("{}::{name}", self.scope.join("::"))
        })
    }

    fn attributes(&self, attrs: &[syn::Attribute]) -> String {
        attrs
            .iter()
            .map(|attr| span_text(self.source, attr.span()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn function(
        &mut self,
        sig: &syn::Signature,
        attrs: &[syn::Attribute],
        vis: Option<&syn::Visibility>,
    ) {
        let name = sig.ident.to_string();
        self.symbols.push(name.clone());
        if self.matches(&name) {
            self.matched = true;
            let attrs = self.attributes(attrs);
            let visibility = vis
                .map(|item| span_text(self.source, item.span()))
                .unwrap_or("");
            let indent = "    ".repeat(self.scope.len());
            self.rendered.push(
                format!(
                    "{indent}{attrs}\n{indent}{visibility} {} {{ ... }}",
                    span_text(self.source, sig.span())
                )
                .trim()
                .to_owned(),
            );
        }
    }

    fn container(&mut self, name: String, span: proc_macro2::Span, visit: impl FnOnce(&mut Self)) {
        let before = self.rendered.len();
        self.scope.push(name);
        visit(self);
        self.scope.pop();
        if self.rendered.len() > before {
            let header = span_text(self.source, span)
                .split('{')
                .next()
                .unwrap_or_default()
                .trim();
            self.rendered.insert(before, format!("{header} {{"));
            self.rendered.push("}".to_owned());
        }
    }
}

impl<'ast> Visit<'ast> for RustSkeleton<'_> {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.function(&node.sig, &node.attrs, Some(&node.vis));
    }
    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.function(&node.sig, &node.attrs, Some(&node.vis));
    }
    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        self.function(&node.sig, &node.attrs, None);
    }
    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        self.container(
            span_text(self.source, node.self_ty.span()).to_owned(),
            node.span(),
            |this| visit::visit_item_impl(this, node),
        );
    }
    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        self.container(node.ident.to_string(), node.span(), |this| {
            visit::visit_item_trait(this, node)
        });
    }
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if node.content.is_some() {
            self.container(node.ident.to_string(), node.span(), |this| {
                visit::visit_item_mod(this, node)
            });
        } else if self.target.is_none() {
            self.rendered
                .push(span_text(self.source, node.span()).to_owned());
        }
    }
    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        self.rendered
            .push(span_text(self.source, node.span()).to_owned());
    }
    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        self.symbols.push(node.ident.to_string());
        if self.matches(&node.ident.to_string()) {
            self.matched = true;
            self.rendered
                .push(span_text(self.source, node.span()).to_owned());
        }
    }
    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        self.symbols.push(node.ident.to_string());
        if self.matches(&node.ident.to_string()) {
            self.matched = true;
            self.rendered
                .push(span_text(self.source, node.span()).to_owned());
        }
    }
}

pub fn rust(source: &str, target: Option<&str>) -> Result<Value, String> {
    if source.len() > 1 << 20 {
        return Err("skeleton source exceeds 1 MiB".to_owned());
    }
    let file = syn::parse_file(source).map_err(|error| format!("Rust syntax error: {error}"))?;
    let mut projection = RustSkeleton {
        source,
        target,
        scope: Vec::new(),
        rendered: Vec::new(),
        symbols: Vec::new(),
        matched: false,
    };
    projection.visit_file(&file);
    if let Some(target) = target {
        if !projection.matched {
            return Err(format!("symbol {target} not found"));
        }
    }
    Ok(
        json!({"skeleton": projection.rendered.join("\n"), "symbols": projection.symbols, "language": "rust"}),
    )
}
