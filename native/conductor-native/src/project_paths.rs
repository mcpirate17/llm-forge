//! Host project path resolution. All configured choices are made here, at call time.

use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};

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

fn source(root: &Path, key: &str) -> String {
    format!(
        "[tool.conductor].{key} in {}",
        root.join("pyproject.toml").display()
    )
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

fn table(root: &Path) -> Result<toml::map::Map<String, Value>, String> {
    let manifest = root.join("pyproject.toml");
    if !manifest.is_file() {
        return Ok(toml::map::Map::new());
    }
    let load_error = || format!("PROJECT_PATHS_MANIFEST_ERROR:{}", root.display());
    let body = fs::read_to_string(&manifest).map_err(|_| load_error())?;
    let parsed: Value = body.parse::<Value>().map_err(|_| load_error())?;
    let conductor = parsed
        .get("tool")
        .and_then(Value::as_table)
        .and_then(|tool| tool.get("conductor"));
    match conductor {
        None => Ok(toml::map::Map::new()),
        Some(Value::Table(table)) => Ok(table.clone()),
        Some(_) => Err(format!(
            "[tool.conductor] in {} is not a table",
            manifest.display()
        )),
    }
}

fn configured(
    root: &Path,
    config: &mut Option<toml::map::Map<String, Value>>,
    spec: PathSpec,
) -> Result<(String, bool), String> {
    if let Ok(raw) = env::var(spec.env) {
        if !raw.trim().is_empty() {
            return relative(&raw, &format!("${}", spec.env)).map(|path| (path, true));
        }
    }
    if config.is_none() {
        *config = Some(table(root)?);
    }
    if let Some(value) = config.as_ref().and_then(|table| table.get(spec.key)) {
        let label = source(root, spec.key);
        let raw = value
            .as_str()
            .ok_or_else(|| format!("{label} must be a string, got {}", type_name(value)))?;
        return relative(raw, &label).map(|path| (path, true));
    }
    Ok((spec.default.to_owned(), false))
}

/// Values are in ProjectPaths dataclass field order, followed by matching flags.
pub fn resolve(root: &Path) -> Result<Vec<(String, bool)>, String> {
    let mut config = None;
    PATHS
        .iter()
        .map(|spec| configured(root, &mut config, *spec))
        .collect()
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
    let config = table(root)?;
    let Some(value) = config.get(key) else {
        return Ok(None);
    };
    let label = source(root, key);
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

pub fn integration_branch(root: &Path) -> Result<String, String> {
    if let Ok(raw) = env::var("CONDUCTOR_INTEGRATION_BRANCH") {
        let value = raw.trim();
        if !value.is_empty() {
            return Ok(value.to_owned());
        }
    }
    let config = table(root)?;
    match config.get("integration_branch") {
        Some(value) => nonempty_string(value, &source(root, "integration_branch")),
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

fn absolute_resolved(path: &Path) -> Result<PathBuf, String> {
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
