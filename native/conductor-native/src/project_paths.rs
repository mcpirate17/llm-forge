//! Host project path resolution. All configured choices are made here, at call time.

use std::env;
use std::path::{Component, Path, PathBuf};

use crate::project_config::{self, ProjectConfig};
use toml::Value;

#[derive(Clone, Copy)]
struct PathSpec {
    key: &'static str,
    env: &'static str,
    default: &'static str,
}

const PATHS: [PathSpec; 11] = [
    PathSpec {
        key: "candidate_policy",
        env: "CONDUCTOR_CANDIDATE_POLICY",
        default: "conductor/candidate_policy.toml",
    },
    PathSpec {
        key: "mutation_registry",
        env: "CONDUCTOR_MUTATION_REGISTRY",
        default: "conductor/mutation_campaigns/registry.json",
    },
    PathSpec {
        key: "package_root",
        env: "CONDUCTOR_PACKAGE_ROOT",
        default: "conductor",
    },
    PathSpec {
        key: "mutation_receipt_root",
        env: "CONDUCTOR_MUTATION_RECEIPT_ROOT",
        default: "research/reports/mutation_testing",
    },
    PathSpec {
        key: "notes_root",
        env: "CONDUCTOR_NOTES_ROOT",
        default: "research/notes",
    },
    PathSpec {
        key: "notes_db",
        env: "CONDUCTOR_NOTES_DB",
        default: "research/notes.db",
    },
    PathSpec {
        key: "guardrail_allowlist",
        env: "CONDUCTOR_GUARDRAIL_ALLOWLIST",
        default: "conductor/guardrail_allowlist.json",
    },
    PathSpec {
        key: "memory_sources",
        env: "CONDUCTOR_MEMORY_SOURCES",
        default: "conductor/memory_sources.toml",
    },
    PathSpec {
        key: "crate_roster",
        env: "CONDUCTOR_CRATE_ROSTER",
        default: "tooling/native/crates.toml",
    },
    PathSpec {
        key: "native_root",
        env: "CONDUCTOR_NATIVE_ROOT",
        default: "tooling/native",
    },
    PathSpec {
        key: "radon_complexity_baseline",
        env: "CONDUCTOR_RADON_BASELINE",
        default: "conductor/radon_complexity_baseline.json",
    },
];

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "str",
        Value::Integer(_) => "int",
        Value::Float(_) => "float",
        Value::Boolean(_) => "bool",
        Value::Datetime(_) => "datetime",
        Value::Array(_) => "list",
        Value::Table(_) => "dict",
    }
}

fn python_repr(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut result = String::from(quote);
    for character in text.chars() {
        match character {
            '\\' => result.push_str("\\\\"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            value if value == quote => {
                result.push('\\');
                result.push(value);
            }
            value => result.push(value),
        }
    }
    result.push(quote);
    result
}

/// Normalize using PurePosixPath's lexical rules, including backslash conversion.
pub fn relative(raw: &str, source: &str) -> Result<String, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(format!("{source} must not be empty"));
    }
    let normalized = text.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(format!(
            "{source} must be repo-root-relative: {}",
            python_repr(text)
        ));
    }
    let parts: Vec<_> = normalized
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if parts.is_empty() || parts.contains(&"..") {
        return Err(format!(
            "{source} must be repo-root-relative: {}",
            python_repr(text)
        ));
    }
    Ok(parts.join("/"))
}

fn environment_path(spec: PathSpec) -> Result<Option<(String, bool)>, String> {
    match env::var(spec.env) {
        Ok(raw) if !raw.trim().is_empty() => {
            return relative(&raw, &format!("${}", spec.env)).map(|path| Some((path, true)));
        }
        Err(env::VarError::NotUnicode(_)) => return Err(format!("{} must be UTF-8", spec.env)),
        Ok(_) | Err(env::VarError::NotPresent) => {}
    }
    Ok(None)
}

fn configured_value(config: &ProjectConfig, spec: PathSpec) -> Result<(String, bool), String> {
    if let Some(value) = config.conductor.get(spec.key) {
        let label = config.label(spec.key);
        let raw = value
            .as_str()
            .ok_or_else(|| format!("{label} must be a string, got {}", type_name(value)))?;
        return relative(raw, &label).map(|path| (path, true));
    }
    Ok((spec.default.to_owned(), false))
}

fn configured(
    root: &Path,
    config: &mut Option<ProjectConfig>,
    spec: PathSpec,
) -> Result<(String, bool), String> {
    if let Some(selected) = environment_path(spec)? {
        return Ok(selected);
    }
    if config.is_none() {
        *config = Some(project_config::load(root)?);
    }
    let config = config.as_ref().expect("loaded above");
    configured_value(config, spec)
}

/// Resolve candidate policy from one admitted manifest without reopening its path.
/// The existing environment override, validation and default remain authoritative.
pub fn candidate_policy_relative(config: &ProjectConfig) -> Result<(String, bool), String> {
    let spec = PATHS[0];
    if let Some(selected) = environment_path(spec)? {
        return Ok(selected);
    }
    configured_value(config, spec)
}

/// Captured project configuration only; no ambient environment override or file read.
pub fn mutation_registry_from_config(config: Option<&ProjectConfig>) -> Result<String, String> {
    match config {
        Some(config) => configured_value(config, PATHS[1]).map(|(path, _)| path),
        None => Ok(PATHS[1].default.into()),
    }
}

/// Captured receipt-root choice without ambient environment overrides.
pub fn mutation_receipt_root_from_config(config: Option<&ProjectConfig>) -> Result<String, String> {
    match config {
        Some(config) => configured_value(config, PATHS[3]).map(|(path, _)| path),
        None => Ok(PATHS[3].default.into()),
    }
}

/// Captured package-root choice, sharing ordinary validation/defaults without
/// consulting environment overrides or reopening the manifest.
pub fn package_root_from_config(config: Option<&ProjectConfig>) -> Result<String, String> {
    match config {
        Some(config) => configured_value(config, PATHS[2]).map(|(path, _)| path),
        None => Ok(PATHS[2].default.into()),
    }
}

/// Resolve the crate roster from captured configuration without reopening files.
pub fn crate_roster_relative(config: &ProjectConfig) -> Result<(String, bool), String> {
    let spec = PATHS[8];
    if let Some(selected) = environment_path(spec)? {
        return Ok(selected);
    }
    configured_value(config, spec)
}

/// Resolve one declared path, preserving precedence without validating unrelated keys.
pub fn resolve_key(root: &Path, key: &str) -> Result<(String, bool), String> {
    let spec = PATHS
        .iter()
        .find(|spec| spec.key == key)
        .ok_or_else(|| format!("unknown project path key: {key}"))?;
    configured(root, &mut None, *spec)
}

/// Values are in ProjectPaths dataclass field order, followed by matching flags.
pub fn resolve(root: &Path) -> Result<Vec<(String, bool)>, String> {
    let mut config = None;
    PATHS
        .iter()
        .map(|spec| configured(root, &mut config, *spec))
        .collect()
}

/// Complexity baseline, preserving full ProjectPaths validation and precedence.
/// Unlike an explicit CLI flag, configured paths remain root-relative.
pub fn radon_baseline(root: &Path) -> Result<PathBuf, String> {
    let values = resolve(root)?;
    Ok(root.join(&values[PATHS.len() - 1].0))
}

/// The configured prose index, resolved against the supplied host.
pub fn notes_db(root: &Path) -> Result<std::path::PathBuf, String> {
    let spec = PATHS
        .iter()
        .find(|spec| spec.key == "notes_db")
        .expect("declared path");
    Ok(root.join(configured(root, &mut None, *spec)?.0))
}

/// Memory catalog path and whether the host explicitly selected it.
pub fn memory_sources(root: &Path) -> Result<(PathBuf, bool), String> {
    let spec = PATHS
        .iter()
        .find(|spec| spec.key == "memory_sources")
        .expect("declared path");
    let (relative, explicit) = configured(root, &mut None, *spec)?;
    Ok((root.join(relative), explicit))
}

/// The host's configured notes directory, including its environment override.
pub fn notes_root(root: &Path) -> Result<PathBuf, String> {
    let spec = PATHS
        .iter()
        .find(|spec| spec.key == "notes_root")
        .expect("declared path");
    Ok(root.join(configured(root, &mut None, *spec)?.0))
}

fn nonempty_string(value: &Value, label: &str) -> Result<String, String> {
    let raw = value
        .as_str()
        .ok_or_else(|| format!("{label} must be a string, got {}", type_name(value)))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("{label} must not be empty"));
    }
    Ok(trimmed.to_owned())
}

fn named_list(root: &Path, key: &str) -> Result<Option<Vec<String>>, String> {
    let config = project_config::load(root)?;
    config_list(&config, key)
}

fn config_list(config: &ProjectConfig, key: &str) -> Result<Option<Vec<String>>, String> {
    let Some(value) = config.conductor.get(key) else {
        return Ok(None);
    };
    let label = config.label(key);
    let list = value.as_array().ok_or_else(|| {
        format!(
            "{label} must be a list of strings, got {}",
            type_name(value)
        )
    })?;
    list.iter()
        .enumerate()
        .map(|(index, item)| nonempty_string(item, &format!("{label}[{index}]")))
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// Validate declared choices from one captured manifest, independent of process
/// overrides. Provisioning must not persist an invalid choice hidden by an env var.
pub fn validate_config(config: &ProjectConfig) -> Result<(), String> {
    for spec in PATHS {
        if let Some(value) = config.conductor.get(spec.key) {
            let label = config.label(spec.key);
            let raw = value
                .as_str()
                .ok_or_else(|| format!("{label} must be a string, got {}", type_name(value)))?;
            relative(raw, &label)?;
        }
    }
    if let Some(value) = config.conductor.get("integration_branch") {
        nonempty_string(value, &config.label("integration_branch"))?;
    }
    for key in ["retired_integration_branches", "worktree_patterns"] {
        config_list(config, key)?;
    }
    Ok(())
}

pub fn integration_branch(root: &Path) -> Result<String, String> {
    if let Ok(raw) = env::var("CONDUCTOR_INTEGRATION_BRANCH") {
        let value = raw.trim();
        if !value.is_empty() {
            return Ok(value.to_owned());
        }
    }
    let config = project_config::load(root)?;
    match config.conductor.get("integration_branch") {
        Some(value) => nonempty_string(value, &config.label("integration_branch")),
        None => Ok("main".to_owned()),
    }
}

pub fn retired_integration_branches(root: &Path) -> Result<Vec<String>, String> {
    Ok(named_list(root, "retired_integration_branches")?.unwrap_or_default())
}

pub fn worktree_patterns(root: &Path) -> Result<Vec<String>, String> {
    Ok(named_list(root, "worktree_patterns")?.unwrap_or_else(|| {
        vec![
            r"/tmp/llm-[\w.-]+".to_owned(),
            r"/home/\w+/Projects/LLM[\w.-]*".to_owned(),
        ]
    }))
}

/// The filesystem root itself is never considered a repository.
pub fn enclosing_repo(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .filter(|candidate| candidate.parent().is_some())
        .find(|candidate| candidate.join(".git").exists())
        .map(Path::to_path_buf)
}

pub fn absolute_resolved(path: &Path) -> Result<PathBuf, String> {
    // Python's Path.resolve is non-strict. Canonicalize the closest existing
    // ancestor, then append missing lexical components for explicit starts.
    if let Ok(resolved) = path.canonicalize() {
        return Ok(resolved);
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    let ancestor = absolute
        .ancestors()
        .find(|candidate| candidate.exists())
        .ok_or_else(|| format!("cannot resolve {}", path.display()))?;
    let mut resolved = ancestor.canonicalize().map_err(|error| error.to_string())?;
    let suffix = absolute
        .strip_prefix(ancestor)
        .map_err(|error| error.to_string())?;
    for component in suffix.components() {
        match component {
            Component::Normal(name) => resolved.push(name),
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => unreachable!("suffix is relative"),
        }
    }
    Ok(resolved)
}

pub fn host_root(start: Option<&Path>) -> Result<PathBuf, String> {
    if let Ok(raw) = env::var("CONDUCTOR_HOST_ROOT") {
        if !raw.is_empty() {
            let path = Path::new(&raw);
            if !path.is_absolute() || !path.exists() {
                return Err(format!(
                    "CONDUCTOR_HOST_ROOT={} must be an absolute, existing path",
                    python_repr(&raw)
                ));
            }
            return absolute_resolved(path);
        }
    }
    let base = match start {
        Some(path) => absolute_resolved(path)?,
        None => env::current_dir().map_err(|error| error.to_string())?,
    };
    Ok(enclosing_repo(&base).unwrap_or(base))
}

pub fn package_tree_root(package_dir: &Path) -> Result<PathBuf, String> {
    let package = absolute_resolved(package_dir)?;
    let repo = enclosing_repo(&package);
    let mut candidates = Vec::new();
    if let Some(repo) = repo {
        candidates.push(repo);
    }
    candidates.extend(package.ancestors().skip(1).map(Path::to_path_buf));
    for candidate in candidates {
        let configured = resolve(&candidate)?;
        let package_path = candidate.join(&configured[2].0);
        if absolute_resolved(&package_path)? == package {
            return Ok(candidate);
        }
    }
    Err(format!(
        "no tree root above {} resolves its configured package root back to it",
        package.display()
    ))
}

#[cfg(feature = "python")]
mod python {
    use super::*;
    use pyo3::exceptions::PyRuntimeError;
    use pyo3::prelude::*;

    fn py_error(message: String) -> PyErr {
        PyRuntimeError::new_err(message)
    }

    #[pyfunction]
    fn project_paths_relative_native(raw: &str, source: &str) -> PyResult<String> {
        relative(raw, source).map_err(py_error)
    }

    #[pyfunction]
    fn project_paths_resolve_native(root: &str) -> PyResult<Vec<(String, bool)>> {
        resolve(Path::new(root)).map_err(py_error)
    }

    #[pyfunction]
    fn project_paths_integration_branch_native(root: &str) -> PyResult<String> {
        integration_branch(Path::new(root)).map_err(py_error)
    }

    #[pyfunction]
    fn project_paths_retired_integration_branches_native(root: &str) -> PyResult<Vec<String>> {
        retired_integration_branches(Path::new(root)).map_err(py_error)
    }

    #[pyfunction]
    fn project_paths_worktree_patterns_native(root: &str) -> PyResult<Vec<String>> {
        worktree_patterns(Path::new(root)).map_err(py_error)
    }

    #[pyfunction]
    fn project_paths_enclosing_repo_native(start: &str) -> Option<String> {
        enclosing_repo(Path::new(start)).map(|path| path.to_string_lossy().into_owned())
    }

    #[pyfunction]
    #[pyo3(signature = (start=None))]
    fn project_paths_host_root_native(start: Option<&str>) -> PyResult<String> {
        host_root(start.map(Path::new))
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(py_error)
    }

    #[pyfunction]
    fn project_paths_package_tree_root_native(package_dir: &str) -> PyResult<String> {
        package_tree_root(Path::new(package_dir))
            .map(|path| path.to_string_lossy().into_owned())
            .map_err(py_error)
    }

    pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
        module.add_function(wrap_pyfunction!(project_paths_relative_native, module)?)?;
        module.add_function(wrap_pyfunction!(project_paths_resolve_native, module)?)?;
        module.add_function(wrap_pyfunction!(
            project_paths_integration_branch_native,
            module
        )?)?;
        module.add_function(wrap_pyfunction!(
            project_paths_retired_integration_branches_native,
            module
        )?)?;
        module.add_function(wrap_pyfunction!(
            project_paths_worktree_patterns_native,
            module
        )?)?;
        module.add_function(wrap_pyfunction!(
            project_paths_enclosing_repo_native,
            module
        )?)?;
        module.add_function(wrap_pyfunction!(project_paths_host_root_native, module)?)?;
        module.add_function(wrap_pyfunction!(
            project_paths_package_tree_root_native,
            module
        )?)?;
        Ok(())
    }
}

#[cfg(feature = "python")]
pub use python::register as register_py;
