//! Syntax-only facts for Forge's structural code graph.
//!
//! These parsers do not infer dynamic dispatch, imports, or semantic similarity.
//! A caller must resolve each call conservatively against definitions it indexed.

use ruff_python_ast as ast;
use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
use std::fmt;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Definition {
    /// Stable within a file while the lexical scope and name are unchanged.
    pub qualified: String,
    pub name: String,
    pub kind: String,
    pub line_start: usize,
    pub line_end: usize,
    pub signature: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Call {
    /// Lexical definition containing the call; empty for module-level calls.
    pub caller: String,
    /// Syntactic call path, such as `helper`, `self.helper`, or `module.helper`.
    pub target: String,
    pub line: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Import {
    /// Local name used at a call site.
    pub alias: String,
    /// Dotted Python module path. A leading dot denotes a relative import.
    pub module: String,
    /// Imported definition name; absent for `import module`.
    pub member: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileFacts {
    pub definitions: Vec<Definition>,
    pub calls: Vec<Call>,
    pub imports: Vec<Import>,
    /// Calls whose callable is a dynamic expression and has no static path.
    pub dynamic_calls: usize,
}

#[derive(Debug)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

struct Lines(Vec<usize>);

impl Lines {
    fn new(source: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(i, byte)| (byte == b'\n').then_some(i + 1)),
        );
        Self(starts)
    }

    fn at(&self, offset: usize) -> usize {
        self.0.partition_point(|start| *start <= offset)
    }
}

fn py_path(expr: &ast::Expr) -> Option<String> {
    match expr {
        ast::Expr::Name(name) => Some(name.id.as_str().to_owned()),
        ast::Expr::Attribute(attr) => Some(format!(
            "{}.{}",
            py_path(attr.value.as_ref())?,
            attr.attr.as_str()
        )),
        _ => None,
    }
}

struct PythonCollector<'a> {
    source: &'a str,
    lines: Lines,
    scope: Vec<String>,
    facts: FileFacts,
}

impl PythonCollector<'_> {
    fn definition(&mut self, name: &str, kind: &'static str, range: ruff_text_size::TextRange) {
        let qualified = if self.scope.is_empty() {
            name.to_owned()
        } else {
            format!("{}.{}", self.scope.join("."), name)
        };
        let start = self.lines.at(range.start().to_usize());
        let end = self.lines.at(range.end().to_usize().saturating_sub(1));
        let raw_signature = self
            .source
            .lines()
            .nth(start.saturating_sub(1))
            .unwrap_or_default()
            .trim();
        let signature = raw_signature
            .char_indices()
            .take_while(|(index, _)| *index < 512)
            .map(|(_, ch)| ch)
            .collect();
        self.facts.definitions.push(Definition {
            qualified,
            name: name.to_owned(),
            kind: kind.to_owned(),
            line_start: start,
            line_end: end.max(start),
            signature,
        });
    }
}

impl<'a> SourceOrderVisitor<'a> for PythonCollector<'_> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        match statement {
            ast::Stmt::FunctionDef(function) => {
                let name = function.name.as_str();
                let kind = if self.scope.is_empty() {
                    "Function"
                } else {
                    "Method"
                };
                self.definition(name, kind, function.range);
                self.scope.push(name.to_owned());
                for child in &function.body {
                    self.visit_stmt(child);
                }
                self.scope.pop();
            }
            ast::Stmt::ClassDef(class) => {
                let name = class.name.as_str();
                self.definition(name, "Class", class.range);
                self.scope.push(name.to_owned());
                for child in &class.body {
                    self.visit_stmt(child);
                }
                self.scope.pop();
            }
            ast::Stmt::Import(import) if self.scope.is_empty() => {
                for alias in &import.names {
                    self.facts.imports.push(Import {
                        alias: alias
                            .asname
                            .as_ref()
                            .map_or(alias.name.as_str(), |name| name.as_str())
                            .to_owned(),
                        module: alias.name.as_str().to_owned(),
                        member: None,
                    });
                }
            }
            ast::Stmt::ImportFrom(import) if self.scope.is_empty() => {
                let module = format!(
                    "{}{}",
                    ".".repeat(import.level as usize),
                    import.module.as_ref().map_or("", |name| name.as_str())
                );
                for alias in &import.names {
                    if alias.name.as_str() == "*" {
                        continue;
                    }
                    self.facts.imports.push(Import {
                        alias: alias
                            .asname
                            .as_ref()
                            .map_or(alias.name.as_str(), |name| name.as_str())
                            .to_owned(),
                        module: module.clone(),
                        member: Some(alias.name.as_str().to_owned()),
                    });
                }
            }
            _ => walk_stmt(self, statement),
        }
    }

    fn visit_expr(&mut self, expression: &'a ast::Expr) {
        if let ast::Expr::Call(call) = expression {
            if let Some(target) = py_path(call.func.as_ref()) {
                self.facts.calls.push(Call {
                    caller: self.scope.join("."),
                    target,
                    line: self.lines.at(call.range.start().to_usize()),
                });
            } else {
                self.facts.dynamic_calls += 1;
            }
        }
        walk_expr(self, expression);
    }
}

pub fn extract_python(source: &str) -> Result<FileFacts, ParseError> {
    let module = ruff_python_parser::parse_module(source)
        .map_err(|error| ParseError(error.to_string()))?
        .into_syntax();
    let mut collector = PythonCollector {
        source,
        lines: Lines::new(source),
        scope: Vec::new(),
        facts: FileFacts::default(),
    };
    for statement in &module.body {
        collector.visit_stmt(statement);
    }
    Ok(collector.facts)
}

fn rust_path(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn rust_type_name(ty: &syn::Type) -> Option<String> {
    match ty {
        syn::Type::Path(path) => Some(rust_path(&path.path)),
        syn::Type::Reference(reference) => rust_type_name(&reference.elem),
        _ => None,
    }
}

#[derive(Default)]
struct RustCollector {
    scope: Vec<String>,
    facts: FileFacts,
}

impl RustCollector {
    fn definition(&mut self, name: &str, kind: &'static str, span: proc_macro2::Span) {
        let qualified = if self.scope.is_empty() {
            name.to_owned()
        } else {
            format!("{}::{name}", self.scope.join("::"))
        };
        self.facts.definitions.push(Definition {
            qualified,
            name: name.to_owned(),
            kind: kind.to_owned(),
            line_start: span.start().line,
            line_end: span.end().line.max(span.start().line),
            signature: String::new(),
        });
    }
}

impl<'ast> Visit<'ast> for RustCollector {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        let name = node.ident.to_string();
        self.definition(&name, "Module", node.span());
        self.scope.push(name);
        visit::visit_item_mod(self, node);
        self.scope.pop();
    }

    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        self.definition(&node.ident.to_string(), "Struct", node.span());
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        self.definition(&node.ident.to_string(), "Enum", node.span());
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let name = node.sig.ident.to_string();
        let kind = if node.attrs.iter().any(|attr| {
            attr.path()
                .segments
                .last()
                .is_some_and(|part| part.ident == "test")
        }) {
            "Test"
        } else {
            "Function"
        };
        self.definition(&name, kind, node.span());
        self.scope.push(name);
        visit::visit_item_fn(self, node);
        self.scope.pop();
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        let Some(type_name) = rust_type_name(&node.self_ty) else {
            visit::visit_item_impl(self, node);
            return;
        };
        self.scope.push(type_name);
        if let Some((_, trait_path, _)) = &node.trait_ {
            self.scope.push(rust_path(trait_path));
        }
        visit::visit_item_impl(self, node);
        if node.trait_.is_some() {
            self.scope.pop();
        }
        self.scope.pop();
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        let name = node.ident.to_string();
        self.definition(&name, "Trait", node.span());
        self.scope.push(name);
        visit::visit_item_trait(self, node);
        self.scope.pop();
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        let name = node.sig.ident.to_string();
        self.definition(&name, "Method", node.span());
        self.scope.push(name);
        visit::visit_trait_item_fn(self, node);
        self.scope.pop();
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        let name = node.sig.ident.to_string();
        self.definition(&name, "Method", node.span());
        self.scope.push(name);
        visit::visit_impl_item_fn(self, node);
        self.scope.pop();
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        match node.func.as_ref() {
            syn::Expr::Path(path) => self.facts.calls.push(Call {
                caller: self.scope.join("::"),
                target: rust_path(&path.path),
                line: node.span().start().line,
            }),
            _ => self.facts.dynamic_calls += 1,
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let receiver = match node.receiver.as_ref() {
            syn::Expr::Path(path) => rust_path(&path.path),
            _ => String::new(),
        };
        if receiver.is_empty() {
            self.facts.dynamic_calls += 1;
        } else {
            self.facts.calls.push(Call {
                caller: self.scope.join("::"),
                target: format!("{receiver}::{}", node.method),
                line: node.span().start().line,
            });
        }
        visit::visit_expr_method_call(self, node);
    }
}

pub fn extract_rust(source: &str) -> Result<FileFacts, ParseError> {
    let file = syn::parse_file(source).map_err(|error| ParseError(error.to_string()))?;
    let mut collector = RustCollector::default();
    collector.visit_file(&file);
    Ok(collector.facts)
}
