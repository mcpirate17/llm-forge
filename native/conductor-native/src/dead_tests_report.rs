//! Shared dead-test classification over caller-captured source and lookups.
//! The caller owns input bounds, file/lookup identity and final revalidation.
pub use super::DeadTestScanError as ScanError;
use super::{dependency_closure, scan_dead_test_bytes, DeadTestModule, PythonModuleResolver};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// Apply the existing basename rule to a repository-relative Python path.
pub fn is_test_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some(stem) = name.strip_suffix(".py") else {
        return false;
    };
    stem.starts_with("test_") || stem.ends_with("_test")
}

fn is_dead_test_side(path: &str) -> bool {
    if is_test_file(path) || path.rsplit('/').next() == Some("conftest.py") {
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

#[derive(Debug, Serialize)]
pub struct BrokenDeadTest {
    pub test: String,
    pub missing: Vec<String>,
    pub last_commit: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UntrackedDeadTest {
    pub test: String,
    pub untracked: Vec<String>,
    pub last_commit: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OrphanDeadTest {
    pub test: String,
    pub targets: Vec<String>,
    pub notes_only: Vec<String>,
    pub last_commit: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StaleDeadTestImport {
    pub module: String,
    pub missing: Vec<String>,
    pub last_commit: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UnreferencedDeadTestSource {
    pub path: String,
    pub has_main: bool,
    pub notes_only: bool,
    pub last_commit: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub tests_scanned: usize,
    pub modules_scanned: usize,
    pub broken: Vec<BrokenDeadTest>,
    pub depends_on_untracked: Vec<UntrackedDeadTest>,
    pub untracked_importers: BTreeMap<String, Vec<String>>,
    pub stale_imports: Vec<StaleDeadTestImport>,
    pub orphan_target: Vec<OrphanDeadTest>,
    pub unreferenced_sources: Vec<UnreferencedDeadTestSource>,
}

/// Caller-supplied repository-relative observations; no authority or IO policy is implied.
pub trait Inputs {
    fn read(&mut self, path: &str) -> anyhow::Result<Vec<u8>>;
    fn is_file(&mut self, path: &str) -> anyhow::Result<bool>;
}
type TestRows = (
    Vec<BrokenDeadTest>,
    Vec<UntrackedDeadTest>,
    Vec<OrphanDeadTest>,
);

pub struct Analysis {
    modules: BTreeMap<String, DeadTestModule>,
    importers: BTreeMap<String, BTreeSet<String>>,
    dynamic_basenames: BTreeSet<String>,
    untracked_importers: BTreeMap<String, Vec<String>>,
}
impl Analysis {
    /// Parse caller-supplied bytes using the compatibility lossy UTF-8 policy.
    /// Each lookup receives one repository-relative candidate; errors are never treated as absence.
    pub fn from_captured(
        root: &Path,
        tracked: &[String],
        inputs: &mut impl Inputs,
    ) -> Result<Self, ScanError> {
        let resolver = PythonModuleResolver::new(root, tracked);
        let mut paths: Vec<_> = resolver.tracked_python.iter().cloned().collect();
        paths.sort_unstable();
        let mut modules = BTreeMap::new();
        for path in paths {
            let bytes = inputs.read(&path).map_err(|source| ScanError::Read {
                path: root.join(&path),
                source,
            })?;
            let module = scan_dead_test_bytes(&path, &bytes, &resolver, &mut |candidate| {
                inputs.is_file(candidate)
            })?;
            modules.insert(path, module);
        }
        Ok(Self::from_modules(modules))
    }
    fn from_modules(modules: BTreeMap<String, DeadTestModule>) -> Self {
        let mut importers = BTreeMap::<String, BTreeSet<String>>::new();
        let mut untracked_importers = BTreeMap::<String, Vec<String>>::new();
        for module in modules.values() {
            for dependency in &module.deps {
                importers
                    .entry(dependency.clone())
                    .or_default()
                    .insert(module.path.clone());
            }
            for path in &module.untracked {
                untracked_importers
                    .entry(path.clone())
                    .or_default()
                    .push(module.path.clone());
            }
        }
        let dynamic_basenames = modules
            .values()
            .filter(|module| !is_dead_test_side(&module.path))
            .flat_map(|module| module.basenames.iter().cloned())
            .collect();
        Self {
            modules,
            importers,
            dynamic_basenames,
            untracked_importers,
        }
    }
    fn source_reachable(&self, path: &str, config_text: &str) -> bool {
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
    }
    fn test_rows(
        &self,
        tests: &[String],
        config_text: &str,
        notes_text: &str,
        dates: &BTreeMap<String, String>,
    ) -> Result<TestRows, String> {
        let mut broken = Vec::new();
        let mut untracked_rows = Vec::new();
        let mut orphans = Vec::new();
        for test in tests {
            let (missing, untracked) = dependency_closure(test, &self.modules)?;
            if !missing.is_empty() {
                broken.push(BrokenDeadTest {
                    test: test.clone(),
                    missing: missing.into_iter().collect(),
                    last_commit: dates.get(test).cloned(),
                });
                continue;
            }
            if !untracked.is_empty() {
                untracked_rows.push(UntrackedDeadTest {
                    test: test.clone(),
                    untracked: untracked.into_iter().collect(),
                    last_commit: dates.get(test).cloned(),
                });
            }
            let targets: Vec<_> = self.modules[test]
                .deps
                .iter()
                .filter(|dependency| !is_dead_test_side(dependency))
                .cloned()
                .collect();
            if !targets.is_empty()
                && !targets
                    .iter()
                    .any(|target| self.source_reachable(target, config_text))
            {
                let notes_only = targets
                    .iter()
                    .filter(|target| corpus_references(target, notes_text))
                    .cloned()
                    .collect();
                orphans.push(OrphanDeadTest {
                    test: test.clone(),
                    targets,
                    notes_only,
                    last_commit: dates.get(test).cloned(),
                });
            }
        }
        Ok((broken, untracked_rows, orphans))
    }
    /// Compute the existing advisory report; this does not authorize deletion.
    pub fn report(
        &self,
        config_text: &str,
        notes_text: &str,
        dates: &BTreeMap<String, String>,
    ) -> Result<Report, String> {
        let tests: Vec<_> = self
            .modules
            .keys()
            .filter(|path| is_test_file(path))
            .cloned()
            .collect();
        let (broken, depends_on_untracked, orphan_target) =
            self.test_rows(&tests, config_text, notes_text, dates)?;
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
                    && !self.source_reachable(&module.path, config_text)
            })
            .map(|module| UnreferencedDeadTestSource {
                path: module.path.clone(),
                has_main: module.has_main,
                notes_only: corpus_references(&module.path, notes_text),
                last_commit: dates.get(&module.path).cloned(),
            })
            .collect();
        Ok(Report {
            tests_scanned: tests.len(),
            modules_scanned: self.modules.len(),
            broken,
            depends_on_untracked,
            untracked_importers: self.untracked_importers.clone(),
            stale_imports,
            orphan_target,
            unreferenced_sources,
        })
    }
}
#[cfg(test)]
#[path = "dead_tests_report_tests.rs"]
mod tests;
