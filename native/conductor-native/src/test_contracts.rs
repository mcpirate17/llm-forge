//! Explicit source-to-Rust-contract discovery for the migrated Python APIs.
//!
//! Selection stays separate from pytest. This module returns data and argv;
//! callers decide when and how to execute the Cargo command.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use syn::visit::Visit;

const MANIFEST: &str = "native/conductor-native/Cargo.toml";
const TEST_DIR: &str = "native/conductor-native/tests";
const REGISTRY_PATH: &str = "native/conductor-native/src/python_contract_targets.tsv";
const REGISTRY_EXTENSION_PATH: &str =
    "native/conductor-native/src/python_contract_targets_extra.tsv";
const REGISTRY_EXTENSION_DIRECTIVE: &str = "# include: python_contract_targets_extra.tsv";
const COMPILED_REGISTRY: &str = include_str!("python_contract_targets.tsv");
const COMPILED_REGISTRY_EXTENSION: &str = include_str!("python_contract_targets_extra.tsv");

fn native_source(path: &str) -> bool {
    ["native/conductor-native/src/", "native/slop-core/src/"]
        .iter()
        .any(|prefix| {
            path.strip_prefix(*prefix)
                .is_some_and(|name| name.ends_with(".rs") && !name.contains('/'))
        })
        || path
            .strip_prefix("native/forge/src/")
            .is_some_and(|name| name.ends_with(".rs"))
}

fn fixture_source(path: &str) -> bool {
    path.starts_with(&format!("{TEST_DIR}/fixtures/")) && path.ends_with(".rs")
}

fn corpus_source(path: &str) -> bool {
    path.ends_with(".json")
}

fn patch_fixture_source(path: &str) -> bool {
    path.starts_with("src/conductor/testdata/") && path.ends_with(".patch")
}

fn python_fixture_source(path: &str) -> bool {
    (path.starts_with("src/conductor/testdata/")
        || path.starts_with(&format!("{TEST_DIR}/fixtures/")))
        && path.ends_with(".py")
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CargoCommand {
    pub cwd: String,
    pub argv: Vec<String>,
    pub targets: Vec<String>,
    pub test_paths: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContractPlan {
    /// Mapped Python evidence inputs, including fixtures; not a coverage inventory.
    pub source_paths: Vec<String>,
    pub targets: Vec<String>,
    pub test_paths: Vec<String>,
    pub commands: Vec<CargoCommand>,
}

#[derive(Default)]
struct Registry {
    by_source: BTreeMap<String, BTreeSet<String>>,
    targets: BTreeSet<String>,
}

fn parse_registry(contents: &str) -> Result<Registry> {
    let mut entries = Registry::default();
    for (index, line) in contents.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (source, target) = line
            .split_once('\t')
            .with_context(|| format!("contract registry line {} has no tab", index + 1))?;
        let source_file = (source.starts_with("src/")
            && !python_fixture_source(source)
            && [".py", ".json", ".sh", ".toml"]
                .iter()
                .any(|suffix| source.ends_with(suffix)))
            || source == ".claude/hooks/dispatch.py";
        let helper = source.starts_with(&format!("{TEST_DIR}/python_contracts/"))
            && source.ends_with(".rs")
            && !source
                .trim_start_matches(&format!("{TEST_DIR}/python_contracts/"))
                .contains('/');
        if !(source_file
            || helper
            || native_source(source)
            || fixture_source(source)
            || corpus_source(source)
            || patch_fixture_source(source)
            || python_fixture_source(source))
            || source.contains("..")
            || source.contains('\\')
            || source.chars().any(char::is_control)
            || source.split('/').any(|part| part.is_empty() || part == ".")
        {
            bail!(
                "invalid contract path on registry line {}: {source}",
                index + 1
            );
        }
        if !target.starts_with("python_contracts_")
            || !target
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            bail!(
                "invalid contract target on registry line {}: {target}",
                index + 1
            );
        }
        if !entries
            .by_source
            .entry(source.to_owned())
            .or_default()
            .insert(target.to_owned())
        {
            bail!("duplicate contract mapping on registry line {}", index + 1);
        }
        entries.targets.insert(target.to_owned());
    }
    if entries.targets.is_empty() {
        bail!("contract registry is empty");
    }
    Ok(entries)
}

fn registry() -> Result<Registry> {
    parse_registry(&format!(
        "{COMPILED_REGISTRY}\n{COMPILED_REGISTRY_EXTENSION}"
    ))
}

fn registry_at(root: &Path, changed_paths: &[String]) -> Result<Registry> {
    let file = root.join(REGISTRY_PATH);
    match fs::symlink_metadata(&file) {
        Ok(_) => {
            require_regular_file(root, REGISTRY_PATH)?;
            let contents = fs::read_to_string(&file)
                .with_context(|| format!("cannot read contract registry: {}", file.display()))?;
            let contents = if contents
                .lines()
                .any(|line| line == REGISTRY_EXTENSION_DIRECTIVE)
            {
                require_regular_file(root, REGISTRY_EXTENSION_PATH)?;
                let extension = fs::read_to_string(root.join(REGISTRY_EXTENSION_PATH))
                    .context("cannot read contract registry extension")?;
                format!("{contents}\n{extension}")
            } else {
                if changed_paths
                    .iter()
                    .any(|path| path == REGISTRY_EXTENSION_PATH)
                {
                    bail!("contract registry extension is not declared: {REGISTRY_EXTENSION_PATH}");
                }
                contents
            };
            parse_registry(&contents)
                .with_context(|| format!("invalid contract registry: {}", file.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let crate_present = match fs::symlink_metadata(root.join(MANIFEST)) {
                Ok(_) => true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => return Err(error).context("cannot inspect native Cargo manifest"),
            };
            let known = registry()?;
            if crate_present
                || changed_paths.iter().any(|path| {
                    path == REGISTRY_PATH
                        || path == REGISTRY_EXTENSION_PATH
                        || known.by_source.contains_key(path)
                        || path.starts_with(&format!("{TEST_DIR}/python_contracts_"))
                        || path.starts_with(&format!("{TEST_DIR}/python_contracts/"))
                })
            {
                bail!("contract registry missing: {}", file.display());
            }
            Ok(Registry::default())
        }
        Err(error) => Err(error)
            .with_context(|| format!("cannot inspect contract registry: {}", file.display())),
    }
}

pub fn registered_targets() -> Result<Vec<String>> {
    Ok(registry()?.targets.into_iter().collect())
}

pub fn registered_sources() -> Result<Vec<String>> {
    Ok(registry()?
        .by_source
        .into_keys()
        .filter(|source| source.ends_with(".py") && !python_fixture_source(source))
        .collect())
}

pub(crate) fn require_regular_file(root: &Path, relative: &str) -> Result<()> {
    let file = root.join(relative);
    let metadata = fs::symlink_metadata(&file)
        .with_context(|| format!("mapped contract file missing: {}", file.display()))?;
    if !metadata.file_type().is_file() {
        bail!("mapped contract file is not regular: {}", file.display());
    }
    let canonical = file
        .canonicalize()
        .with_context(|| format!("cannot resolve mapped contract file: {}", file.display()))?;
    if canonical != file {
        bail!("mapped contract file escapes its crate: {}", file.display());
    }
    Ok(())
}

#[derive(Default)]
struct ModuleIncludes {
    paths: Vec<String>,
    error: Option<String>,
}

impl<'ast> Visit<'ast> for ModuleIncludes {
    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        let paths = module
            .attrs
            .iter()
            .filter(|attr| attr.path().is_ident("path"))
            .collect::<Vec<_>>();
        if module
            .attrs
            .iter()
            .any(|attr| attr.path().is_ident("cfg_attr"))
        {
            self.error = Some("conditional module attributes are unsupported in contracts".into());
        }
        if module.content.is_none() {
            let value = paths.first().and_then(|attr| match &attr.meta {
                syn::Meta::NameValue(value) => match &value.value {
                    syn::Expr::Lit(value) => match &value.lit {
                        syn::Lit::Str(value) => Some(value.value()),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            });
            match value {
                Some(path) if paths.len() == 1 => self.paths.push(path),
                _ => self.error = Some("contract external modules require one literal path".into()),
            }
        } else if !paths.is_empty() {
            self.error = Some("contract inline modules cannot change helper paths".into());
        }
        syn::visit::visit_item_mod(self, module);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if node
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "include")
        {
            self.error =
                Some("contract source include! is unsupported; register a helper module".into());
        }
        syn::visit::visit_macro(self, node);
    }
}

fn source_module_paths(root: &Path, relative: &str) -> Result<Vec<String>> {
    require_regular_file(root, relative)?;
    let text = fs::read_to_string(root.join(relative))?;
    let syntax = syn::parse_file(&text)
        .with_context(|| format!("cannot parse Rust contract source: {relative}"))?;
    let mut includes = ModuleIncludes::default();
    includes.visit_file(&syntax);
    if let Some(error) = includes.error {
        bail!("{relative}: {error}");
    }
    Ok(includes.paths)
}

fn target_modules(root: &Path, target: &str) -> Result<BTreeSet<String>> {
    let mut modules = BTreeSet::new();
    for helper in source_module_paths(root, &format!("{TEST_DIR}/{target}.rs"))? {
        let local_helper = helper.starts_with("python_contracts/")
            && !helper.trim_start_matches("python_contracts/").contains('/');
        let fixture = helper.starts_with("fixtures/")
            && !helper.trim_start_matches("fixtures/").contains('/');
        if !(local_helper || fixture)
            || !helper.ends_with(".rs")
            || helper.contains("..")
            || helper.contains('\\')
            || helper.chars().any(char::is_control)
        {
            bail!("unsupported contract helper path: {helper}");
        }
        modules.insert(format!("{TEST_DIR}/{helper}"));
    }
    Ok(modules)
}

fn validate_selected_helpers(root: &Path, target: &str, entries: &Registry) -> Result<()> {
    let included = target_modules(root, target)?;
    let included_helpers = included
        .iter()
        .filter(|path| !fixture_source(path))
        .cloned()
        .collect::<BTreeSet<_>>();
    let registered = entries
        .by_source
        .iter()
        .filter(|(path, targets)| {
            path.starts_with(&format!("{TEST_DIR}/python_contracts/")) && targets.contains(target)
        })
        .map(|(path, _)| path.clone())
        .collect::<BTreeSet<_>>();
    if included_helpers != registered {
        bail!("contract helper registry differs from target include directives: {target}");
    }
    for fixture in included.iter().filter(|path| fixture_source(path)) {
        if !entries
            .by_source
            .get(fixture)
            .is_some_and(|targets| targets.contains(target))
        {
            bail!("contract fixture include missing from registry: {fixture}");
        }
    }
    for helper in registered {
        if !source_module_paths(root, &helper)?.is_empty() {
            bail!("nested contract helper modules are unsupported: {helper}");
        }
    }
    for (fixture, targets) in &entries.by_source {
        if (corpus_source(fixture)
            || patch_fixture_source(fixture)
            || python_fixture_source(fixture))
            && targets.contains(target)
        {
            require_regular_file(root, fixture)?;
        }
        if fixture_source(fixture)
            && targets.contains(target)
            && !source_module_paths(root, fixture)?.is_empty()
        {
            bail!("external modules in contract fixture programs are unsupported: {fixture}");
        }
    }
    Ok(())
}

fn validate_inventory(root: &Path, entries: &Registry) -> Result<()> {
    require_regular_file(root, MANIFEST)?;
    for target in &entries.targets {
        validate_selected_helpers(root, target, entries)?;
    }
    validate_target_inventory(root, entries)
}

fn validate_target_inventory(root: &Path, entries: &Registry) -> Result<()> {
    let test_dir = root.join(TEST_DIR);
    for entry in fs::read_dir(&test_dir)
        .with_context(|| format!("cannot read contract tests: {}", test_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(target) = name
            .strip_prefix("python_contracts_")
            .and_then(|name| name.strip_suffix(".rs"))
        {
            let full_target = format!("python_contracts_{target}");
            if !entries.targets.contains(&full_target) {
                bail!("contract target missing from registry: {full_target}");
            }
        }
    }
    Ok(())
}

/// Verify that the Forge checkout and the checked-in registry cover each other.
/// This is an explicit maintenance check, not a prerequisite for host selection.
pub fn validate_registry_inventory(root: &Path) -> Result<()> {
    let root = root
        .canonicalize()
        .with_context(|| format!("repository root does not exist: {}", root.display()))?;
    require_regular_file(&root, REGISTRY_PATH)?;
    validate_inventory(&root, &registry_at(&root, &[])?)
}

fn normalized_path(root: &Path, root_alias: &Path, raw: &str) -> Result<String> {
    if raw.is_empty() || raw.contains('\\') || raw.chars().any(char::is_control) {
        bail!("invalid changed path: {raw:?}");
    }
    let input = Path::new(raw);
    let relative = if input.is_absolute() {
        input
            .strip_prefix(root)
            .or_else(|_| input.strip_prefix(root_alias))
            .with_context(|| format!("changed path is outside repository: {raw}"))?
    } else {
        input
    };
    let mut components = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => components.push(name),
            Component::ParentDir => {
                if !components.pop() {
                    bail!("changed path escapes repository: {raw}");
                }
            }
            _ => bail!("invalid changed path: {raw:?}"),
        }
    }
    if components.as_os_str().is_empty() {
        bail!("changed path is repository root: {raw:?}");
    }
    let existing = root.join(&components);
    if existing.exists() && !existing.canonicalize()?.starts_with(root) {
        bail!("changed path resolves outside repository: {raw}");
    }
    components
        .to_str()
        .map(str::to_owned)
        .context("changed path is not UTF-8")
}

fn targets_for_rust_path(relative: &str, entries: &Registry) -> Result<BTreeSet<String>> {
    let target = relative
        .strip_prefix(&format!("{TEST_DIR}/"))
        .and_then(|name| name.strip_suffix(".rs"));
    if let Some(target) = target {
        if target.starts_with("python_contracts_") && !target.contains('/') {
            if !entries.targets.contains(target) {
                bail!("changed contract target missing from registry: {target}");
            }
            return Ok(BTreeSet::from([target.to_owned()]));
        }
    }
    let Some(helper) = relative.strip_prefix(&format!("{TEST_DIR}/python_contracts/")) else {
        return Ok(BTreeSet::new());
    };
    if !helper.ends_with(".rs") || helper.contains('/') {
        return Ok(BTreeSet::new());
    }
    if !entries.by_source.contains_key(relative) {
        bail!("changed contract helper has no registered users: {relative}");
    }
    Ok(BTreeSet::new())
}

pub fn plan(root: &Path, changed_paths: &[String]) -> Result<ContractPlan> {
    let root_alias = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };
    let root = root
        .canonicalize()
        .with_context(|| format!("repository root does not exist: {}", root.display()))?;
    if !root.is_dir() {
        bail!("repository root is not a directory: {}", root.display());
    }
    let changed_paths = changed_paths
        .iter()
        .map(|path| normalized_path(&root, &root_alias, path))
        .collect::<Result<Vec<_>>>()?;
    let entries = registry_at(&root, &changed_paths)?;
    let mut targets = BTreeSet::new();
    let mut source_paths = BTreeSet::new();
    for relative in &changed_paths {
        if relative == REGISTRY_PATH || relative == REGISTRY_EXTENSION_PATH {
            targets.extend(entries.targets.iter().cloned());
            continue;
        }
        if let Some(found) = entries.by_source.get(relative) {
            targets.extend(found.iter().cloned());
            if relative.ends_with(".py") {
                source_paths.insert(relative.clone());
            }
        }
        targets.extend(targets_for_rust_path(relative, &entries)?);
    }
    let targets = targets.into_iter().collect::<Vec<_>>();
    if !targets.is_empty() {
        require_regular_file(&root, MANIFEST)?;
        for target in &targets {
            validate_selected_helpers(&root, target, &entries)?;
        }
    }
    if changed_paths
        .iter()
        .any(|path| path == REGISTRY_PATH || path == REGISTRY_EXTENSION_PATH)
    {
        validate_target_inventory(&root, &entries)?;
    }
    let test_paths = targets
        .iter()
        .map(|target| format!("{TEST_DIR}/{target}.rs"))
        .collect::<Vec<_>>();
    let commands = if targets.is_empty() {
        Vec::new()
    } else {
        let mut argv = [
            "cargo",
            "test",
            "--jobs",
            "2",
            "--offline",
            "--locked",
            "--manifest-path",
            MANIFEST,
            "--features",
            "python-compat-tests",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        for target in &targets {
            argv.push("--test".to_owned());
            argv.push(target.clone());
        }
        argv.extend(["--".to_owned(), "--test-threads=1".to_owned()]);
        vec![CargoCommand {
            cwd: root.display().to_string(),
            argv,
            targets: targets.clone(),
            test_paths: test_paths.clone(),
        }]
    };
    Ok(ContractPlan {
        source_paths: source_paths.into_iter().collect(),
        targets,
        test_paths,
        commands,
    })
}

#[cfg(feature = "python")]
mod python {
    use super::plan;
    use pyo3::exceptions::PyValueError;
    use pyo3::prelude::*;
    use std::path::Path;

    #[pyfunction]
    fn contract_test_plan_native(repo_root: &str, changed_paths: Vec<String>) -> PyResult<String> {
        let plan = plan(Path::new(repo_root), &changed_paths).map_err(|error| {
            PyValueError::new_err(format!("contract discovery failed: {error:#}"))
        })?;
        serde_json::to_string(&plan).map_err(|error| PyValueError::new_err(error.to_string()))
    }

    pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
        module.add_function(wrap_pyfunction!(contract_test_plan_native, module)?)?;
        Ok(())
    }
}

#[cfg(feature = "python")]
pub use python::register;
