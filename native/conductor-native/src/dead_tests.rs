//! Dead-test dependency closure and untracked-import closure for the agent tooling.
//!
//! Moved verbatim from research-runtime's `repository_analysis.rs` (tooling boundary
//! step 2a); the guardrail and compile-callsite scanners that research/tools call stay
//! there. The two share nothing but `std`, pyo3 and ruff_python_ast.

// PyO3's generated argument conversion trips this Rust 1.93 lint even though the
// handwritten functions do not perform a redundant conversion.
#![allow(clippy::useless_conversion)]

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use pyo3::exceptions::{PyKeyError, PyOSError, PyValueError};
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
enum DeadTestScanError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: String,
        detail: String,
    },
}

impl DeadTestScanError {
    fn into_python(self) -> PyErr {
        match self {
            Self::Read { path, source } => {
                PyOSError::new_err(format!("cannot read {}: {source}", path.display()))
            }
            Self::Parse { path, detail } => {
                PyValueError::new_err(format!("{path} does not parse: {detail}"))
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
) {
    if let Some(attribute) = attribute.filter(|attribute| *attribute != "*") {
        let dotted = if base.is_empty() {
            attribute.to_owned()
        } else {
            format!("{base}.{attribute}")
        };
        if let Some(path) = resolver.resolve(&dotted, &module.path) {
            module.deps.insert(path);
            return;
        }
    }
    if base.is_empty() {
        return;
    }
    if let Some(path) = resolver.resolve(base, &module.path) {
        module.deps.insert(path);
    } else if resolver.is_first_party(base) && !resolver.is_satisfied(base, &module.path) {
        if let Some(path) = resolver.resolve_untracked(base, &module.path) {
            module.untracked.insert(path);
        } else if hard {
            module.missing.insert(base.to_owned());
        } else {
            module.soft_missing.insert(base.to_owned());
        }
    }
}

fn scan_dead_test_module(
    source_root: &Path,
    path: &str,
    resolver: &PythonModuleResolver,
) -> Result<DeadTestModule, DeadTestScanError> {
    let source_path = source_root.join(path);
    let source = fs::read(&source_path).map_err(|source| DeadTestScanError::Read {
        path: source_path,
        source,
    })?;
    let source = String::from_utf8_lossy(&source);
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
        record_dead_test_import(&mut module, resolver, &base, attribute.as_deref(), true);
    }
    for (base, attribute) in collector.soft {
        record_dead_test_import(&mut module, resolver, &base, attribute.as_deref(), false);
    }
    for dotted in collector.strings {
        if let Some(path) = resolver.resolve(&dotted, path) {
            module.deps.insert(path);
        }
    }
    module.deps.remove(path);
    Ok(module)
}

fn is_dead_test_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some(stem) = name.strip_suffix(".py") else {
        return false;
    };
    stem.starts_with("test_") || stem.ends_with("_test")
}

fn is_dead_test_side(path: &str) -> bool {
    if is_dead_test_file(path) || path.rsplit('/').next() == Some("conftest.py") {
        return true;
    }
    let parts = path.split('/').collect::<Vec<_>>();
    parts
        .get(..parts.len().saturating_sub(1))
        .is_some_and(|parents| parents.contains(&"tests"))
}

fn dotted_module_name(path: &str) -> String {
    let mut stem = path.strip_suffix(".py").unwrap_or(path);
    for marker in ["/__init__", "/__main__"] {
        if let Some(value) = stem.strip_suffix(marker) {
            stem = value;
            break;
        }
    }
    stem.replace('/', ".")
}

fn corpus_references(path: &str, corpus: &str) -> bool {
    corpus.contains(path) || corpus.contains(&dotted_module_name(path))
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

#[derive(Debug, Serialize)]
struct BrokenDeadTest {
    test: String,
    missing: Vec<String>,
    last_commit: Option<String>,
}

#[derive(Debug, Serialize)]
struct UntrackedDeadTest {
    test: String,
    untracked: Vec<String>,
    last_commit: Option<String>,
}

#[derive(Debug, Serialize)]
struct OrphanDeadTest {
    test: String,
    targets: Vec<String>,
    notes_only: Vec<String>,
    last_commit: Option<String>,
}

#[derive(Debug, Serialize)]
struct StaleDeadTestImport {
    module: String,
    missing: Vec<String>,
    last_commit: Option<String>,
}

#[derive(Debug, Serialize)]
struct UnreferencedDeadTestSource {
    path: String,
    has_main: bool,
    notes_only: bool,
    last_commit: Option<String>,
}

#[derive(Debug, Serialize)]
struct DeadTestReportCore {
    tests_scanned: usize,
    modules_scanned: usize,
    broken: Vec<BrokenDeadTest>,
    depends_on_untracked: Vec<UntrackedDeadTest>,
    untracked_importers: BTreeMap<String, Vec<String>>,
    stale_imports: Vec<StaleDeadTestImport>,
    orphan_target: Vec<OrphanDeadTest>,
    unreferenced_sources: Vec<UnreferencedDeadTestSource>,
}

#[pyclass(module = "conductor_native")]
struct DeadTestsResolverNative {
    resolver: PythonModuleResolver,
}

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

#[pyclass(module = "conductor_native")]
struct DeadTestsAnalysisNative {
    modules: BTreeMap<String, DeadTestModule>,
    importers: BTreeMap<String, BTreeSet<String>>,
    dynamic_basenames: BTreeSet<String>,
    untracked_importers: BTreeMap<String, Vec<String>>,
}

#[pymethods]
impl DeadTestsAnalysisNative {
    #[new]
    fn new(repo_root: PathBuf, tracked: Vec<String>) -> PyResult<Self> {
        let resolver = PythonModuleResolver::new(&repo_root, &tracked);
        let mut paths = resolver.tracked_python.iter().cloned().collect::<Vec<_>>();
        paths.sort_unstable();
        let mut modules = BTreeMap::new();
        for path in paths {
            let module = scan_dead_test_module(&repo_root, &path, &resolver)
                .map_err(DeadTestScanError::into_python)?;
            modules.insert(path, module);
        }
        let mut importers = BTreeMap::<String, BTreeSet<String>>::new();
        for module in modules.values() {
            for dependency in &module.deps {
                importers
                    .entry(dependency.clone())
                    .or_default()
                    .insert(module.path.clone());
            }
        }
        let dynamic_basenames = modules
            .values()
            .filter(|module| !is_dead_test_side(&module.path))
            .flat_map(|module| module.basenames.iter().cloned())
            .collect();
        let mut untracked_importers = BTreeMap::<String, Vec<String>>::new();
        for module in modules.values() {
            for path in &module.untracked {
                untracked_importers
                    .entry(path.clone())
                    .or_default()
                    .push(module.path.clone());
            }
        }
        Ok(Self {
            modules,
            importers,
            dynamic_basenames,
            untracked_importers,
        })
    }

    fn report(
        &self,
        config_text: &str,
        notes_text: &str,
        dates: HashMap<String, String>,
    ) -> PyResult<String> {
        let reachable = |path: &str| {
            let non_test = self.importers.get(path).is_some_and(|importers| {
                importers
                    .iter()
                    .any(|importer| !is_dead_test_side(importer))
            });
            let dynamic = path
                .rsplit('/')
                .next()
                .is_some_and(|name| self.dynamic_basenames.contains(name));
            non_test || dynamic || corpus_references(path, config_text)
        };
        let tests = self
            .modules
            .keys()
            .filter(|path| is_dead_test_file(path))
            .cloned()
            .collect::<Vec<_>>();
        let mut broken = Vec::new();
        let mut depends_on_untracked = Vec::new();
        let mut orphan_target = Vec::new();
        for test in &tests {
            let (missing, untracked) =
                dependency_closure(test, &self.modules).map_err(PyKeyError::new_err)?;
            if !missing.is_empty() {
                broken.push(BrokenDeadTest {
                    test: test.clone(),
                    missing: missing.into_iter().collect(),
                    last_commit: dates.get(test).cloned(),
                });
                continue;
            }
            if !untracked.is_empty() {
                depends_on_untracked.push(UntrackedDeadTest {
                    test: test.clone(),
                    untracked: untracked.into_iter().collect(),
                    last_commit: dates.get(test).cloned(),
                });
            }
            let targets = self.modules[test]
                .deps
                .iter()
                .filter(|dependency| !is_dead_test_side(dependency))
                .cloned()
                .collect::<Vec<_>>();
            if !targets.is_empty() && !targets.iter().any(|target| reachable(target)) {
                let notes_only = targets
                    .iter()
                    .filter(|target| corpus_references(target, notes_text))
                    .cloned()
                    .collect();
                orphan_target.push(OrphanDeadTest {
                    test: test.clone(),
                    targets,
                    notes_only,
                    last_commit: dates.get(test).cloned(),
                });
            }
        }
        let stale_imports = self
            .modules
            .values()
            .filter(|module| !module.soft_missing.is_empty())
            .map(|module| StaleDeadTestImport {
                module: module.path.clone(),
                missing: module.soft_missing.iter().cloned().collect(),
                last_commit: dates.get(&module.path).cloned(),
            })
            .collect();
        let unreferenced_sources = self
            .modules
            .values()
            .filter(|module| {
                !is_dead_test_side(&module.path)
                    && self
                        .importers
                        .get(&module.path)
                        .is_none_or(BTreeSet::is_empty)
                    && !module.path.ends_with("__init__.py")
                    && !reachable(&module.path)
            })
            .map(|module| UnreferencedDeadTestSource {
                path: module.path.clone(),
                has_main: module.has_main,
                notes_only: corpus_references(&module.path, notes_text),
                last_commit: dates.get(&module.path).cloned(),
            })
            .collect();
        let report = DeadTestReportCore {
            tests_scanned: tests.len(),
            modules_scanned: self.modules.len(),
            broken,
            depends_on_untracked,
            untracked_importers: self.untracked_importers.clone(),
            stale_imports,
            orphan_target,
            unreferenced_sources,
        };
        serde_json::to_string(&report).map_err(|error| PyValueError::new_err(error.to_string()))
    }
}

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

#[pyfunction]
fn scan_untracked_import_closure_native(
    py: Python<'_>,
    repo_root: PathBuf,
    tracked: Vec<String>,
) -> PyResult<String> {
    let scan = py.detach(|| untracked_import_closure_scan(&repo_root, &tracked));
    Ok(serde_json::to_string(&scan).expect("import closure contains serializable values"))
}

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
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        dependency_closure, scan_dead_test_module, untracked_import_closure_scan, DeadTestModule,
        PythonModuleResolver, UntrackedImportFinding,
    };

    static NEXT_TEST_PATH: AtomicU64 = AtomicU64::new(0);

    fn repo_path() -> PathBuf {
        let serial = NEXT_TEST_PATH.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "conductor-native-dead-tests-{}-{serial}",
            std::process::id()
        ))
    }

    fn write(root: &std::path::Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture has a parent")).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn untracked_import_closure_is_transitive_and_preserves_immediate_importers() {
        let root = repo_path();
        write(&root, "pkg/__init__.py", "");
        write(&root, "pkg/tracked.py", "VALUE = 1\n");
        write(&root, "pkg/nested/item.py", "VALUE = 2\n");
        write(
            &root,
            "pkg/entry.py",
            r#"from pkg import tracked
def guarded():
    from pkg import untracked
try:
    from . import sibling
except ImportError:
    pass
import pkg.native
import pkg.nested
"#,
        );
        write(&root, "pkg/untracked.py", "from pkg import deeper\n");
        write(&root, "pkg/deeper.py", "VALUE = 3\n");
        write(&root, "pkg/sibling.py", "from pkg import deeper\n");
        write(&root, "pkg/broken.py", "from pkg import hidden\nif:\n");
        write(&root, "pkg/hidden.py", "VALUE = 4\n");
        write(&root, "pkg/native.rs", "pub fn marker() {}\n");
        let tracked = vec![
            "pkg/__init__.py".to_owned(),
            "pkg/broken.py".to_owned(),
            "pkg/entry.py".to_owned(),
            "pkg/native.rs".to_owned(),
            "pkg/nested/item.py".to_owned(),
            "pkg/tracked.py".to_owned(),
        ];

        let scan = untracked_import_closure_scan(&root, &tracked);

        assert_eq!(
            scan.records,
            vec![
                UntrackedImportFinding {
                    module: "pkg/deeper.py".to_owned(),
                    importers: vec!["pkg/sibling.py".to_owned(), "pkg/untracked.py".to_owned(),],
                    tracked_importers: vec![],
                },
                UntrackedImportFinding {
                    module: "pkg/sibling.py".to_owned(),
                    importers: vec!["pkg/entry.py".to_owned()],
                    tracked_importers: vec!["pkg/entry.py".to_owned()],
                },
                UntrackedImportFinding {
                    module: "pkg/untracked.py".to_owned(),
                    importers: vec!["pkg/entry.py".to_owned()],
                    tracked_importers: vec!["pkg/entry.py".to_owned()],
                },
            ]
        );
        assert_eq!(scan.parse_error_paths, vec!["pkg/broken.py"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dead_test_scanner_preserves_guard_relative_string_and_native_semantics() {
        let root = repo_path();
        write(&root, "pkg/__init__.py", "");
        write(&root, "pkg/dep.py", "VALUE = 1\n");
        write(&root, "pkg/dynamic.py", "VALUE = 2\n");
        write(&root, "pkg/native.rs", "pub fn marker() {}\n");
        write(
            &root,
            "pkg/sub/module.py",
            r#"from typing import TYPE_CHECKING
from .. import dep
import pkg.native
if FLAG:
    import pkg.hard_missing
if TYPE_CHECKING:
    import pkg.type_missing
if __name__ == "__main__":
    import pkg.main_missing
try:
    import pkg.try_missing
except ImportError:
    pass
def lazy():
    import pkg.lazy_missing
"pkg.dynamic"
"loader.py"
"#,
        );
        let tracked = vec![
            "pkg/__init__.py".to_owned(),
            "pkg/dep.py".to_owned(),
            "pkg/dynamic.py".to_owned(),
            "pkg/native.rs".to_owned(),
            "pkg/sub/module.py".to_owned(),
        ];
        let resolver = PythonModuleResolver::new(&root, &tracked);

        let module = scan_dead_test_module(&root, "pkg/sub/module.py", &resolver).unwrap();

        assert!(module.has_main);
        assert_eq!(module.basenames, BTreeSet::from(["loader.py".to_owned()]));
        assert_eq!(
            module.deps,
            BTreeSet::from(["pkg/dep.py".to_owned(), "pkg/dynamic.py".to_owned()])
        );
        assert_eq!(
            module.missing,
            BTreeSet::from(["pkg.hard_missing".to_owned()])
        );
        assert_eq!(
            module.soft_missing,
            BTreeSet::from([
                "pkg.lazy_missing".to_owned(),
                "pkg.main_missing".to_owned(),
                "pkg.try_missing".to_owned(),
                "pkg.type_missing".to_owned(),
            ])
        );
        assert!(module.untracked.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dead_test_closure_handles_ten_thousand_module_cycle() {
        let count = 10_000;
        let mut modules = BTreeMap::new();
        for index in 0..count {
            let path = format!("pkg/module_{index:05}.py");
            let dependency = format!("pkg/module_{:05}.py", (index + 1) % count);
            modules.insert(
                path.clone(),
                DeadTestModule {
                    path,
                    has_main: false,
                    basenames: BTreeSet::new(),
                    deps: BTreeSet::from([dependency]),
                    missing: if index == count - 1 {
                        BTreeSet::from(["pkg.gone".to_owned()])
                    } else {
                        BTreeSet::new()
                    },
                    soft_missing: BTreeSet::new(),
                    untracked: if index == count / 2 {
                        BTreeSet::from(["pkg/local_only.py".to_owned()])
                    } else {
                        BTreeSet::new()
                    },
                },
            );
        }

        let (missing, untracked) = dependency_closure("pkg/module_00000.py", &modules).unwrap();

        assert_eq!(missing, BTreeSet::from(["pkg.gone".to_owned()]));
        assert_eq!(untracked, BTreeSet::from(["pkg/local_only.py".to_owned()]));
    }
}
