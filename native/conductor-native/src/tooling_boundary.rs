//! Batched filesystem and Python-AST scan for the extractable-tooling boundary.
//!
//! Python retains policy constants, hook-root selection, diagnostics, CLI behavior,
//! and validation of live native exports. This module owns the repeated directory
//! traversal, source reads, parsing, and literal/import collection.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use pyo3::exceptions::{PyOSError, PySyntaxError, PyValueError};
use pyo3::prelude::*;
use ruff_python_ast as ast;
use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
use ruff_text_size::Ranged;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct BoundaryFact {
    path: String,
    line: usize,
    kind: &'static str,
    value: String,
}

#[derive(Debug, Default, Serialize)]
struct BoundaryFacts {
    a: Vec<BoundaryFact>,
    b: Vec<BoundaryFact>,
    c: Vec<BoundaryFact>,
    d: Vec<BoundaryFact>,
}

struct Lines {
    starts: Vec<usize>,
}

impl Lines {
    fn new(source: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .as_bytes()
                .iter()
                .enumerate()
                .filter_map(|(index, byte)| (*byte == b'\n').then_some(index + 1)),
        );
        Self { starts }
    }

    fn number(&self, offset: usize) -> usize {
        self.starts.partition_point(|start| *start <= offset)
    }
}

fn collect_files(root: &Path, python_only: bool) -> std::io::Result<Vec<PathBuf>> {
    fn visit(dir: &Path, python_only: bool, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit(&path, python_only, found)?;
            } else if path.is_file()
                && (!python_only || path.extension().is_some_and(|suffix| suffix == "py"))
            {
                found.push(path);
            }
        }
        Ok(())
    }

    let mut found = Vec::new();
    visit(root, python_only, &mut found)?;
    found.sort();
    Ok(found)
}

fn shown_path(path: &Path, package_dir: &Path) -> String {
    let root = package_dir.parent().unwrap_or(package_dir);
    let resolved_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let resolved_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    resolved_path
        .strip_prefix(resolved_root)
        .unwrap_or(&resolved_path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn is_test_path(relative: &Path) -> bool {
    relative
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("test_"))
        || relative
            .parent()
            .is_some_and(|parent| parent.components().any(|part| part.as_os_str() == "tests"))
}

fn allowed(
    allowlist: &HashSet<(String, String, String)>,
    relative: &str,
    kind: &str,
    value: &str,
) -> bool {
    allowlist.contains(&(relative.to_owned(), kind.to_owned(), value.to_owned()))
}

fn project_string(value: &str, packages: &HashSet<String>) -> bool {
    let (module, plugin) = value
        .split_once(':')
        .map_or((value, None), |(left, right)| (left, Some(right)));
    if plugin.is_some_and(|name| !identifier(name)) {
        return false;
    }
    let mut parts = module.split('.');
    let Some(root) = parts.next() else {
        return false;
    };
    if !packages.contains(root) {
        return false;
    }
    let children = parts.collect::<Vec<_>>();
    (!children.is_empty() || plugin.is_some()) && children.iter().all(|part| identifier(part))
}

fn identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|first| first == '_' || first.is_alphabetic())
        && chars.all(|character| character == '_' || character.is_alphanumeric())
}

struct ImportCollector<'a> {
    relative: &'a str,
    shown: &'a str,
    lines: &'a Lines,
    project_packages: &'a HashSet<String>,
    native_crate: &'a str,
    native_seam: &'a str,
    allowlist: &'a HashSet<(String, String, String)>,
    facts_a: &'a mut Vec<BoundaryFact>,
    facts_d: &'a mut Vec<BoundaryFact>,
}

impl ImportCollector<'_> {
    fn visit_module(&mut self, module: &str, line: usize) {
        let root = module.split('.').next().unwrap_or_default();
        if self.project_packages.contains(root)
            && !allowed(self.allowlist, self.relative, "import", module)
        {
            self.facts_a.push(BoundaryFact {
                path: self.shown.to_owned(),
                line,
                kind: "project_import",
                value: module.to_owned(),
            });
        }
        if self.relative != self.native_seam
            && root == self.native_crate
            && !allowed(self.allowlist, self.relative, "import", self.native_crate)
        {
            self.facts_d.push(BoundaryFact {
                path: self.shown.to_owned(),
                line,
                kind: "native_import",
                value: self.native_crate.to_owned(),
            });
        }
    }
}

impl<'a> SourceOrderVisitor<'a> for ImportCollector<'_> {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        let line = self.lines.number(statement.range().start().to_usize());
        match statement {
            ast::Stmt::Import(node) => {
                for alias in &node.names {
                    self.visit_module(alias.name.as_str(), line);
                }
            }
            ast::Stmt::ImportFrom(node) if node.level == 0 => {
                if let Some(module) = &node.module {
                    self.visit_module(module.as_str(), line);
                }
            }
            _ => {}
        }
        walk_stmt(self, statement);
    }

    fn visit_expr(&mut self, expression: &'a ast::Expr) {
        if let ast::Expr::StringLiteral(literal) = expression {
            let value = literal.value.to_str();
            let line = self.lines.number(expression.range().start().to_usize());
            if project_string(value, self.project_packages)
                && !allowed(
                    self.allowlist,
                    self.relative,
                    "string",
                    value.split(':').next().unwrap_or(value),
                )
            {
                self.facts_a.push(BoundaryFact {
                    path: self.shown.to_owned(),
                    line,
                    kind: "project_string",
                    value: value.to_owned(),
                });
            }
            if self.relative != self.native_seam
                && value == self.native_crate
                && !allowed(self.allowlist, self.relative, "string", self.native_crate)
            {
                self.facts_d.push(BoundaryFact {
                    path: self.shown.to_owned(),
                    line,
                    kind: "native_string",
                    value: value.to_owned(),
                });
            }
        }
        walk_expr(self, expression);
    }
}

fn literal_facts(
    path: &str,
    source: &str,
    literals: &[String],
    kind: &'static str,
) -> Vec<BoundaryFact> {
    source
        .lines()
        .enumerate()
        .flat_map(|(index, line)| {
            literals
                .iter()
                .filter(move |literal| line.contains(*literal))
                .map(move |literal| BoundaryFact {
                    path: path.to_owned(),
                    line: index + 1,
                    kind,
                    value: literal.clone(),
                })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn scan(
    package_dir: &Path,
    hook_dirs: &[PathBuf],
    project_packages: HashSet<String>,
    native_crate: &str,
    native_seam: &str,
    path_literals: &[String],
    hook_literals: &[String],
    hook_excluded_subdir: &str,
    allowlist: HashSet<(String, String, String)>,
) -> Result<BoundaryFacts, PyErr> {
    let mut facts = BoundaryFacts::default();
    let modules =
        collect_files(package_dir, true).map_err(|error| PyOSError::new_err(error.to_string()))?;
    for path in modules {
        let relative_path = path.strip_prefix(package_dir).unwrap_or(&path);
        if relative_path
            .components()
            .any(|part| part.as_os_str() == "__pycache__")
        {
            continue;
        }
        let relative = relative_path.to_string_lossy().replace('\\', "/");
        let shown = shown_path(&path, package_dir);
        let source = fs::read_to_string(&path)
            .map_err(|error| PyOSError::new_err(format!("{}: {error}", path.display())))?;
        let parsed = ruff_python_parser::parse_module(&source)
            .map_err(|error| PySyntaxError::new_err(format!("{}: {error}", path.display())))?;
        let syntax = parsed.into_syntax();
        let lines = Lines::new(&source);
        let mut collector = ImportCollector {
            relative: &relative,
            shown: &shown,
            lines: &lines,
            project_packages: &project_packages,
            native_crate,
            native_seam,
            allowlist: &allowlist,
            facts_a: &mut facts.a,
            facts_d: &mut facts.d,
        };
        for statement in &syntax.body {
            collector.visit_stmt(statement);
        }
        if !is_test_path(relative_path) {
            facts.c.extend(
                literal_facts(&shown, &source, path_literals, "module_literal")
                    .into_iter()
                    .filter(|fact| !allowed(&allowlist, &relative, "literal", &fact.value)),
            );
        }
    }

    for hook_dir in hook_dirs {
        let files = collect_files(hook_dir, false)
            .map_err(|error| PyOSError::new_err(error.to_string()))?;
        for path in files {
            let relative = path.strip_prefix(hook_dir).unwrap_or(&path);
            let mut components = relative.components();
            if components
                .next()
                .is_some_and(|part| part.as_os_str() == hook_excluded_subdir)
                || relative
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("test_"))
                || relative
                    .components()
                    .any(|part| part.as_os_str() == "__pycache__")
            {
                continue;
            }
            let source = String::from_utf8_lossy(
                &fs::read(&path)
                    .map_err(|error| PyOSError::new_err(format!("{}: {error}", path.display())))?,
            )
            .into_owned();
            facts.b.extend(literal_facts(
                &shown_path(&path, package_dir),
                &source,
                hook_literals,
                "hook_literal",
            ));
        }
    }
    Ok(facts)
}

#[pyfunction]
#[pyo3(signature = (package_dir, hook_dirs, project_packages, native_crate, native_seam, path_literals, hook_literals, hook_excluded_subdir, allowlist))]
#[allow(clippy::too_many_arguments)]
fn tooling_boundary_facts_native(
    py: Python<'_>,
    package_dir: String,
    hook_dirs: Vec<String>,
    project_packages: Vec<String>,
    native_crate: String,
    native_seam: String,
    path_literals: Vec<String>,
    hook_literals: Vec<String>,
    hook_excluded_subdir: String,
    allowlist: Vec<(String, String, String)>,
) -> PyResult<String> {
    let result = py.detach(|| {
        scan(
            Path::new(&package_dir),
            &hook_dirs.into_iter().map(PathBuf::from).collect::<Vec<_>>(),
            project_packages.into_iter().collect(),
            &native_crate,
            &native_seam,
            &path_literals,
            &hook_literals,
            &hook_excluded_subdir,
            allowlist.into_iter().collect(),
        )
    })?;
    serde_json::to_string(&result).map_err(|error| PyValueError::new_err(error.to_string()))
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(tooling_boundary_facts_native, module)?)?;
    Ok(())
}
