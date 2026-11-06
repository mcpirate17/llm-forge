//! Dead-test dependency closure and untracked-import closure for the agent tooling.
//!
//! Moved verbatim from research-runtime's `repository_analysis.rs` (tooling boundary
//! step 2a); the guardrail and compile-callsite scanners that research/tools call stay
//! there. The two share nothing but `std`, pyo3 and ruff_python_ast.

// PyO3's generated argument conversion trips this Rust 1.93 lint even though the
// handwritten functions do not perform a redundant conversion.
#![allow(clippy::useless_conversion)]

#[cfg(feature = "python")]
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(feature = "python")]
use pyo3::exceptions::{PyKeyError, PyOSError, PyValueError};
#[cfg(feature = "python")]
use pyo3::prelude::*;
use ruff_python_ast as ast;
use ruff_python_ast::visitor::source_order::{walk_expr, walk_stmt, SourceOrderVisitor};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize)]
struct UntrackedImportFinding {
    module: String,
    importers: Vec<String>,
    tracked_importers: Vec<String>,
}

#[derive(Debug, PartialEq, Serialize)]
struct UntrackedImportClosureScan {
    records: Vec<UntrackedImportFinding>,
    parse_error_paths: Vec<String>,
}

const NATIVE_SUFFIXES: [&str; 8] = [".pyx", ".rs", ".cpp", ".cc", ".cu", ".c", ".so", ".pyd"];

struct PythonModuleResolver {
    repo_root: PathBuf,
    tracked_python: HashSet<String>,
    first_party: HashSet<String>,
    directories: HashSet<String>,
    native_modules: HashSet<String>,
}

impl PythonModuleResolver {
    fn new(repo_root: &Path, tracked: &[String]) -> Self {
        let tracked_python = tracked
            .iter()
            .filter(|path| path.ends_with(".py"))
            .cloned()
            .collect::<HashSet<_>>();
        let first_party = tracked_python
            .iter()
            .filter_map(|path| path.split_once('/').map(|(root, _)| root.to_owned()))
            .collect();
        let mut directories = HashSet::new();
        for path in &tracked_python {
            let parts = path.split('/').collect::<Vec<_>>();
            for length in 1..parts.len() {
                directories.insert(parts[..length].join("/"));
            }
            directories.insert(".".to_owned());
        }
        let native_modules = tracked
            .iter()
            .filter_map(|path| {
                NATIVE_SUFFIXES
                    .iter()
                    .find_map(|suffix| path.strip_suffix(suffix))
                    .map(str::to_owned)
            })
            .collect();
        Self {
            repo_root: repo_root.to_path_buf(),
            tracked_python,
            first_party,
            directories,
            native_modules,
        }
    }

    fn bases(from_file: &str) -> Vec<String> {
        let parent = from_file.rsplit_once('/').map_or("", |(parent, _)| parent);
        let parts = parent
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        let mut bases = vec![String::new()];
        for length in (0..=parts.len()).rev() {
            if length == 0 {
                bases.push(String::new());
            } else {
                bases.push(format!("{}/", parts[..length].join("/")));
            }
        }
        bases
    }

    fn candidates(dotted: &str, from_file: &str) -> Vec<String> {
        let relative = dotted.replace('.', "/");
        Self::bases(from_file)
            .into_iter()
            .flat_map(|base| {
                [
                    format!("{base}{relative}.py"),
                    format!("{base}{relative}/__init__.py"),
                ]
            })
            .collect()
    }

    fn resolve(&self, dotted: &str, from_file: &str) -> Option<String> {
        Self::candidates(dotted, from_file)
            .into_iter()
            .find(|candidate| self.tracked_python.contains(candidate))
    }

    fn resolve_untracked(&self, dotted: &str, from_file: &str) -> Option<String> {
        Self::candidates(dotted, from_file)
            .into_iter()
            .find(|candidate| self.repo_root.join(candidate).is_file())
    }

    fn is_satisfied(&self, dotted: &str, from_file: &str) -> bool {
        let relative = dotted.replace('.', "/");
        Self::bases(from_file).into_iter().any(|base| {
            let candidate = format!("{base}{relative}");
            self.directories.contains(&candidate) || self.native_modules.contains(&candidate)
        })
    }

    fn is_first_party(&self, dotted: &str) -> bool {
        let root = dotted.split_once('.').map_or(dotted, |(root, _)| root);
        self.first_party.contains(root)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct DeadTestModule {
    path: String,
    has_main: bool,
    basenames: BTreeSet<String>,
    deps: BTreeSet<String>,
    missing: BTreeSet<String>,
    soft_missing: BTreeSet<String>,
    untracked: BTreeSet<String>,
}

#[derive(Default)]
struct DeadTestImportCollector {
    path: String,
    hard: BTreeSet<(String, Option<String>)>,
    soft: BTreeSet<(String, Option<String>)>,
    strings: BTreeSet<String>,
    basenames: BTreeSet<String>,
    has_main: bool,
    guard_depth: usize,
}

impl DeadTestImportCollector {
    fn new(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            ..Self::default()
        }
    }

    fn add(&mut self, base: String, attribute: Option<String>) {
        if self.guard_depth == 0 {
            self.hard.insert((base, attribute));
        } else {
            self.soft.insert((base, attribute));
        }
    }

    fn guarded<F>(&mut self, visit: F)
    where
        F: FnOnce(&mut Self),
    {
        self.guard_depth += 1;
        visit(self);
        self.guard_depth -= 1;
    }
}

fn is_type_checking_expression(expression: &ast::Expr) -> bool {
    match expression {
        ast::Expr::Name(name) => name.id.as_str() == "TYPE_CHECKING",
        ast::Expr::Attribute(attribute) => attribute.attr.as_str() == "TYPE_CHECKING",
        _ => false,
    }
}

fn is_main_guard_expression(expression: &ast::Expr) -> bool {
    let ast::Expr::Compare(compare) = expression else {
        return false;
    };
    matches!(compare.left.as_ref(), ast::Expr::Name(name) if name.id.as_str() == "__name__")
}

fn is_dotted_name(value: &str) -> bool {
    value.contains('.')
        && value.split('.').all(|component| {
            let mut chars = component.chars();
            chars
                .next()
                .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
                && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
        })
}

impl<'a> SourceOrderVisitor<'a> for DeadTestImportCollector {
    fn visit_stmt(&mut self, statement: &'a ast::Stmt) {
        match statement {
            ast::Stmt::Import(node) => {
                for alias in &node.names {
                    self.add(alias.name.to_string(), None);
                }
            }
            ast::Stmt::ImportFrom(node) => {
                let mut base = node
                    .module
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                let level = node.level as usize;
                if level > 0 {
                    let prefix = relative_import_base(&self.path, level);
                    base = if base.is_empty() {
                        prefix
                    } else if prefix.is_empty() {
                        base
                    } else {
                        format!("{prefix}.{base}")
                    };
                }
                for alias in &node.names {
                    self.add(base.clone(), Some(alias.name.to_string()));
                }
            }
            ast::Stmt::FunctionDef(_) | ast::Stmt::Try(_) => {
                self.guarded(|collector| walk_stmt(collector, statement));
            }
            ast::Stmt::If(node) => {
                let main_guard = is_main_guard_expression(&node.test);
                self.has_main |= main_guard;
                if main_guard || is_type_checking_expression(&node.test) {
                    self.guarded(|collector| walk_stmt(collector, statement));
                } else {
                    walk_stmt(self, statement);
                }
            }
            _ => walk_stmt(self, statement),
        }
    }

    fn visit_expr(&mut self, expression: &'a ast::Expr) {
        if let ast::Expr::StringLiteral(literal) = expression {
            let value = literal.value.to_str();
            if value.ends_with(".py") {
                if !value.contains('/') {
                    self.basenames.insert(value.to_owned());
                }
            } else if is_dotted_name(value) {
                self.strings.insert(value.to_owned());
            }
        }
        walk_expr(self, expression);
    }
}

#[derive(Debug)]
pub enum DeadTestScanError {
    Read {
        path: PathBuf,
        source: anyhow::Error,
    },
    Parse {
        path: String,
        detail: String,
    },
    Lookup {
        path: String,
        source: anyhow::Error,
    },
}

impl std::fmt::Display for DeadTestScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            Self::Parse { path, detail } => write!(f, "{path} does not parse: {detail}"),
            Self::Lookup { path, source } => write!(f, "cannot inspect {path}: {source}"),
        }
    }
}
impl std::error::Error for DeadTestScanError {}

impl DeadTestScanError {
    #[cfg(feature = "python")]
    fn into_python(self) -> PyErr {
        match self {
            Self::Read { path, source } => {
                PyOSError::new_err(format!("cannot read {}: {source}", path.display()))
            }
            Self::Parse { path, detail } => {
                PyValueError::new_err(format!("{path} does not parse: {detail}"))
            }
            Self::Lookup { path, source } => {
                PyOSError::new_err(format!("cannot inspect {path}: {source}"))
            }
        }
    }
}

fn record_dead_test_import(
    module: &mut DeadTestModule,
    resolver: &PythonModuleResolver,
    base: &str,
    attribute: Option<&str>,
    hard: bool,
    lookup: &mut impl FnMut(&str) -> anyhow::Result<bool>,
) -> Result<(), DeadTestScanError> {
    if let Some(attribute) = attribute.filter(|attribute| *attribute != "*") {
        let dotted = if base.is_empty() {
            attribute.to_owned()
        } else {
            format!("{base}.{attribute}")
        };
        if let Some(path) = resolver.resolve(&dotted, &module.path) {
            module.deps.insert(path);
            return Ok(());
        }
    }
    if base.is_empty() {
        return Ok(());
    }
    if let Some(path) = resolver.resolve(base, &module.path) {
        module.deps.insert(path);
    } else if resolver.is_first_party(base) && !resolver.is_satisfied(base, &module.path) {
        let mut found = None;
        for candidate in PythonModuleResolver::candidates(base, &module.path) {
            if lookup(&candidate).map_err(|source| DeadTestScanError::Lookup {
                path: candidate.clone(),
                source,
            })? {
                found = Some(candidate);
                break;
            }
        }
        if let Some(path) = found {
            module.untracked.insert(path);
        } else if hard {
            module.missing.insert(base.to_owned());
        } else {
            module.soft_missing.insert(base.to_owned());
        }
    }
    Ok(())
}

#[cfg(any(feature = "python", test))]
fn scan_dead_test_module(
    source_root: &Path,
    path: &str,
    resolver: &PythonModuleResolver,
) -> Result<DeadTestModule, DeadTestScanError> {
    let source_path = source_root.join(path);
    let source = fs::read(&source_path).map_err(|source| DeadTestScanError::Read {
        path: source_path,
        source: source.into(),
    })?;
    scan_dead_test_bytes(path, &source, resolver, &mut |candidate| {
        Ok(resolver.repo_root.join(candidate).is_file())
    })
}

fn scan_dead_test_bytes(
    path: &str,
    source: &[u8],
    resolver: &PythonModuleResolver,
    lookup: &mut impl FnMut(&str) -> anyhow::Result<bool>,
) -> Result<DeadTestModule, DeadTestScanError> {
    let source = String::from_utf8_lossy(source);
    let statements = ruff_python_parser::parse_module(&source)
        .map_err(|error| DeadTestScanError::Parse {
            path: path.to_owned(),
            detail: error.to_string(),
        })?
        .into_syntax()
        .body;
    let mut collector = DeadTestImportCollector::new(path);
    for statement in &statements {
        collector.visit_stmt(statement);
    }
    let mut module = DeadTestModule {
        path: path.to_owned(),
        has_main: collector.has_main,
        basenames: collector.basenames,
        deps: BTreeSet::new(),
        missing: BTreeSet::new(),
        soft_missing: BTreeSet::new(),
        untracked: BTreeSet::new(),
    };
    for (base, attribute) in collector.hard {
        record_dead_test_import(
            &mut module,
            resolver,
            &base,
            attribute.as_deref(),
            true,
            lookup,
        )?;
    }
    for (base, attribute) in collector.soft {
        record_dead_test_import(
            &mut module,
            resolver,
            &base,
            attribute.as_deref(),
            false,
            lookup,
        )?;
    }
    for dotted in collector.strings {
        if let Some(path) = resolver.resolve(&dotted, path) {
            module.deps.insert(path);
        }
    }
    module.deps.remove(path);
    Ok(module)
}

fn dependency_closure(
    start: &str,
    modules: &BTreeMap<String, DeadTestModule>,
) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
    let mut seen = HashSet::new();
    let mut missing = BTreeSet::new();
    let mut untracked = BTreeSet::new();
    let mut queue = VecDeque::from([start.to_owned()]);
    while let Some(current) = queue.pop_front() {
        if !seen.insert(current.clone()) {
            continue;
        }
        let module = modules.get(&current).ok_or(current)?;
        missing.extend(module.missing.iter().cloned());
        untracked.extend(module.untracked.iter().cloned());
        queue.extend(
            module
                .deps
                .iter()
                .filter(|dependency| !seen.contains(*dependency))
                .cloned(),
        );
    }
    Ok((missing, untracked))
}

#[cfg(feature = "python")]
#[pyclass(module = "conductor_native")]
struct DeadTestsResolverNative {
    resolver: PythonModuleResolver,
}

#[cfg(feature = "python")]
#[pymethods]
impl DeadTestsResolverNative {
    #[new]
    fn new(repo_root: PathBuf, tracked: Vec<String>) -> Self {
        Self {
            resolver: PythonModuleResolver::new(&repo_root, &tracked),
        }
    }

    fn files(&self) -> Vec<String> {
        let mut values = self
            .resolver
            .tracked_python
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        values.sort_unstable();
        values
    }

    fn first_party(&self) -> Vec<String> {
        let mut values = self
            .resolver
            .first_party
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        values.sort_unstable();
        values
    }

    fn directories(&self) -> Vec<String> {
        let mut values = self
            .resolver
            .directories
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        values.sort_unstable();
        values
    }

    fn native_modules(&self) -> Vec<String> {
        let mut values = self
            .resolver
            .native_modules
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        values.sort_unstable();
        values
    }

    fn resolve(&self, dotted: &str, from_file: &str) -> Option<String> {
        self.resolver.resolve(dotted, from_file)
    }

    fn resolve_untracked(&self, dotted: &str, from_file: &str) -> Option<String> {
        self.resolver.resolve_untracked(dotted, from_file)
    }

    fn is_satisfied(&self, dotted: &str, from_file: &str) -> bool {
        self.resolver.is_satisfied(dotted, from_file)
    }

    fn is_first_party(&self, dotted: &str) -> bool {
        self.resolver.is_first_party(dotted)
    }

    fn scan_module(&self, path: &str, source_root: PathBuf) -> PyResult<String> {
        let module = scan_dead_test_module(&source_root, path, &self.resolver)
            .map_err(DeadTestScanError::into_python)?;
        serde_json::to_string(&module).map_err(|error| PyValueError::new_err(error.to_string()))
    }
}

#[cfg(feature = "python")]
struct FileReportInputs<'a> {
    root: &'a Path,
}
#[cfg(feature = "python")]
impl report::Inputs for FileReportInputs<'_> {
    fn read(&mut self, path: &str) -> anyhow::Result<Vec<u8>> {
        Ok(fs::read(self.root.join(path))?)
    }
    fn is_file(&mut self, path: &str) -> anyhow::Result<bool> {
        Ok(self.root.join(path).is_file())
    }
}
#[cfg(feature = "python")]
#[pyclass(module = "conductor_native")]
struct DeadTestsAnalysisNative {
    analysis: report::Analysis,
}

#[cfg(feature = "python")]
#[pymethods]
impl DeadTestsAnalysisNative {
    #[new]
    fn new(repo_root: PathBuf, tracked: Vec<String>) -> PyResult<Self> {
        let analysis = report::Analysis::from_captured(
            &repo_root,
            &tracked,
            &mut FileReportInputs { root: &repo_root },
        )
        .map_err(DeadTestScanError::into_python)?;
        Ok(Self { analysis })
    }
    fn report(
        &self,
        config_text: &str,
        notes_text: &str,
        dates: HashMap<String, String>,
    ) -> PyResult<String> {
        let dates = dates.into_iter().collect();
        let result = self
            .analysis
            .report(config_text, notes_text, &dates)
            .map_err(PyKeyError::new_err)?;
        serde_json::to_string(&result).map_err(|error| PyValueError::new_err(error.to_string()))
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn dead_tests_closure_native(start: &str, modules_json: &str) -> PyResult<String> {
    let modules = serde_json::from_str::<BTreeMap<String, DeadTestModule>>(modules_json)
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
    let (missing, untracked) = dependency_closure(start, &modules).map_err(PyKeyError::new_err)?;
    serde_json::to_string(&(missing, untracked))
        .map_err(|error| PyValueError::new_err(error.to_string()))
}

fn relative_import_base(path: &str, level: usize) -> String {
    let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    let parts = parent
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let keep = parts.len().saturating_sub(level.saturating_sub(1));
    parts[..keep].join(".")
}

fn collect_python_imports(
    statements: &[ast::Stmt],
    path: &str,
    output: &mut Vec<(String, Option<String>)>,
) {
    fn collect_bodies(
        bodies: &[&[ast::Stmt]],
        path: &str,
        output: &mut Vec<(String, Option<String>)>,
    ) {
        for body in bodies {
            collect_python_imports(body, path, output);
        }
    }

    for statement in statements {
        match statement {
            ast::Stmt::Import(node) => {
                output.extend(
                    node.names
                        .iter()
                        .map(|alias| (alias.name.to_string(), None)),
                );
            }
            ast::Stmt::ImportFrom(node) => {
                let mut base = node
                    .module
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                let level = node.level as usize;
                if level > 0 {
                    let prefix = relative_import_base(path, level);
                    base = if base.is_empty() {
                        prefix
                    } else if prefix.is_empty() {
                        base
                    } else {
                        format!("{prefix}.{base}")
                    };
                }
                output.extend(
                    node.names
                        .iter()
                        .map(|alias| (base.clone(), Some(alias.name.to_string()))),
                );
            }
            ast::Stmt::FunctionDef(node) => collect_python_imports(&node.body, path, output),
            ast::Stmt::ClassDef(node) => collect_python_imports(&node.body, path, output),
            ast::Stmt::For(node) => {
                collect_bodies(&[&node.body, &node.orelse], path, output);
            }
            ast::Stmt::While(node) => {
                collect_bodies(&[&node.body, &node.orelse], path, output);
            }
            ast::Stmt::If(node) => {
                collect_python_imports(&node.body, path, output);
                for clause in &node.elif_else_clauses {
                    collect_python_imports(&clause.body, path, output);
                }
            }
            ast::Stmt::With(node) => collect_python_imports(&node.body, path, output),
            ast::Stmt::Match(node) => {
                for case in &node.cases {
                    collect_python_imports(&case.body, path, output);
                }
            }
            ast::Stmt::Try(node) => {
                collect_bodies(&[&node.body, &node.orelse, &node.finalbody], path, output);
                for handler in &node.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    collect_python_imports(&handler.body, path, output);
                }
            }
            _ => {}
        }
    }
}

fn scan_python_imports(repo_root: &Path, path: &str) -> Option<Vec<(String, Option<String>)>> {
    let source = fs::read_to_string(repo_root.join(path)).ok()?;
    let statements = ruff_python_parser::parse_module(&source)
        .ok()?
        .into_syntax()
        .body;
    let mut imports = Vec::new();
    collect_python_imports(&statements, path, &mut imports);
    Some(imports)
}

fn record_untracked_import(
    resolver: &PythonModuleResolver,
    importer: &str,
    base: &str,
    attribute: Option<&str>,
    output: &mut HashSet<String>,
) {
    if let Some(attribute) = attribute.filter(|attribute| *attribute != "*") {
        let dotted = if base.is_empty() {
            attribute.to_owned()
        } else {
            format!("{base}.{attribute}")
        };
        if resolver.resolve(&dotted, importer).is_some() {
            return;
        }
        if let Some(path) = resolver.resolve_untracked(&dotted, importer) {
            output.insert(path);
            return;
        }
    }
    if base.is_empty() || resolver.resolve(base, importer).is_some() {
        return;
    }
    if resolver.is_first_party(base) && !resolver.is_satisfied(base, importer) {
        if let Some(path) = resolver.resolve_untracked(base, importer) {
            output.insert(path);
        }
    }
}

fn untracked_import_closure_scan(
    repo_root: &Path,
    tracked: &[String],
) -> UntrackedImportClosureScan {
    let resolver = PythonModuleResolver::new(repo_root, tracked);
    let tracked_set = tracked.iter().cloned().collect::<HashSet<_>>();
    let mut importers = BTreeMap::<String, HashSet<String>>::new();
    let mut parse_error_paths = Vec::new();
    let mut seen = HashSet::new();
    let mut frontier = tracked
        .iter()
        .filter(|path| path.ends_with(".py"))
        .cloned()
        .collect::<Vec<_>>();
    while let Some(path) = frontier.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let Some(imports) = scan_python_imports(repo_root, &path) else {
            parse_error_paths.push(path);
            continue;
        };
        let mut untracked = HashSet::new();
        for (base, attribute) in imports {
            record_untracked_import(
                &resolver,
                &path,
                &base,
                attribute.as_deref(),
                &mut untracked,
            );
        }
        for dependency in untracked {
            importers
                .entry(dependency.clone())
                .or_default()
                .insert(path.clone());
            frontier.push(dependency);
        }
    }
    let records = importers
        .into_iter()
        .map(|(module, paths)| {
            let mut importers = paths.into_iter().collect::<Vec<_>>();
            importers.sort_unstable();
            let tracked_importers = importers
                .iter()
                .filter(|path| tracked_set.contains(*path))
                .cloned()
                .collect();
            UntrackedImportFinding {
                module,
                importers,
                tracked_importers,
            }
        })
        .collect();
    parse_error_paths.sort_unstable();
    UntrackedImportClosureScan {
        records,
        parse_error_paths,
    }
}

#[cfg(feature = "python")]
#[pyfunction]
fn scan_untracked_import_closure_native(
    py: Python<'_>,
    repo_root: PathBuf,
    tracked: Vec<String>,
) -> PyResult<String> {
    let scan = py.detach(|| untracked_import_closure_scan(&repo_root, &tracked));
    Ok(serde_json::to_string(&scan).expect("import closure contains serializable values"))
}

#[cfg(feature = "python")]
pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<DeadTestsResolverNative>()?;
    module.add_class::<DeadTestsAnalysisNative>()?;
    module.add_function(wrap_pyfunction!(dead_tests_closure_native, module)?)?;
    module.add_function(wrap_pyfunction!(
        scan_untracked_import_closure_native,
        module
    )?)?;
    Ok(())
}

#[cfg(test)]
#[path = "dead_tests_tests.rs"]
mod tests;

#[path = "dead_tests_bounded.rs"]
pub mod bounded;

#[path = "dead_tests_report.rs"]
pub mod report;

#[path = "dead_tests_inputs.rs"]
pub mod inputs;
