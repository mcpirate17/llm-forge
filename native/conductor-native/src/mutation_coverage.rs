// PyO3 0.29's generated argument conversion trips this Rust 1.93 lint even
// though the handwritten functions do not perform a redundant conversion.
#![allow(clippy::useless_conversion)]

use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use serde_json::Value;

fn value_error(message: impl Into<String>) -> PyErr {
    PyValueError::new_err(message.into())
}

fn normalize_mutation_path(value: &str, label: &str) -> Result<String, String> {
    let text = value.replace('\\', "/");
    if text.trim().is_empty() {
        return Err(format!("{label} must be a non-empty string"));
    }
    if text.starts_with('/') || text.starts_with("./") {
        return Err(format!(
            "{label} must be a normalized repository-relative path"
        ));
    }

    let mut parts = Vec::new();
    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(format!(
                    "{label} must be a normalized repository-relative path"
                ));
            }
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        Ok(".".to_owned())
    } else {
        Ok(parts.join("/"))
    }
}

#[pyfunction]
fn normalize_mutation_path_native(value: &str, label: &str) -> PyResult<String> {
    normalize_mutation_path(value, label).map_err(value_error)
}

fn lexical_absolute(path: &Path) -> Result<PathBuf, String> {
    let source = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| format!("cannot resolve current directory: {error}"))?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in source.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Ok(normalized)
}

fn resolved_existing_or_lexical(path: &Path) -> Result<PathBuf, String> {
    match fs::canonicalize(path) {
        Ok(resolved) => Ok(resolved),
        Err(_) => lexical_absolute(path),
    }
}

fn registry_patterns(
    repo_root: &str,
    registry_path: &str,
    canonical_patterns: &[String],
) -> Result<Vec<String>, String> {
    let root = resolved_existing_or_lexical(Path::new(repo_root))?;
    let registry = resolved_existing_or_lexical(Path::new(registry_path))?;
    if registry.strip_prefix(&root).is_err() {
        return Err("mutation registry must be inside the repository".to_owned());
    }
    let text = fs::read_to_string(&registry).map_err(|error| {
        format!(
            "cannot load mutation registry {}: {error}",
            registry.display()
        )
    })?;
    let payload: Value = serde_json::from_str(&text).map_err(|error| {
        format!(
            "cannot load mutation registry {}: {error}",
            registry.display()
        )
    })?;
    let object = payload
        .as_object()
        .ok_or_else(|| "registry must be a JSON object".to_owned())?;
    let raw_patterns = object.get("test_patterns").and_then(Value::as_array);
    let Some(raw_patterns) = raw_patterns else {
        return Err("registry.test_patterns must be a list of non-empty strings".to_owned());
    };
    if raw_patterns.is_empty() {
        return Err("registry.test_patterns must be a list of non-empty strings".to_owned());
    }
    let mut patterns = Vec::with_capacity(raw_patterns.len());
    for raw in raw_patterns {
        let Some(pattern) = raw.as_str() else {
            return Err("registry.test_patterns must be a list of non-empty strings".to_owned());
        };
        if pattern.is_empty() {
            return Err("registry.test_patterns must be a list of non-empty strings".to_owned());
        }
        patterns.push(pattern.to_owned());
    }
    if patterns != canonical_patterns {
        return Err("registry.test_patterns must match the canonical inventory".to_owned());
    }
    Ok(patterns)
}

#[pyfunction]
fn mutation_registry_patterns_native(
    repo_root: &str,
    registry_path: &str,
    canonical_patterns: Vec<String>,
) -> PyResult<Vec<String>> {
    registry_patterns(repo_root, registry_path, &canonical_patterns).map_err(value_error)
}

fn wildcard_segment_matches(candidate: &str, pattern: &str) -> bool {
    let candidate: Vec<char> = candidate.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    let mut row = vec![false; candidate.len() + 1];
    row[0] = true;
    for token in pattern {
        let mut next = vec![false; candidate.len() + 1];
        match token {
            '*' => {
                next[0] = row[0];
                for index in 1..=candidate.len() {
                    next[index] = row[index] || next[index - 1];
                }
            }
            '?' => {
                next[1..].copy_from_slice(&row[..candidate.len()]);
            }
            literal => {
                for index in 1..=candidate.len() {
                    next[index] = row[index - 1] && candidate[index - 1] == literal;
                }
            }
        }
        row = next;
    }
    row[candidate.len()]
}

fn glob_segments_match(path: &[&str], pattern: &[&str]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    if pattern[0] == "**" {
        // pathlib's leading ``**/`` requires at least one directory component.
        return (1..=path.len()).any(|count| glob_segments_match(&path[count..], &pattern[1..]));
    }
    if path.is_empty() || !wildcard_segment_matches(path[0], pattern[0]) {
        return false;
    }
    glob_segments_match(&path[1..], &pattern[1..])
}

fn is_mutation_test_path(path: &str, patterns: &[String]) -> bool {
    let normalized = path.replace('\\', "/");
    let path_parts: Vec<&str> = normalized
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    patterns.iter().any(|raw_pattern| {
        let normalized_pattern = raw_pattern.replace('\\', "/");
        let pattern_parts: Vec<&str> = normalized_pattern
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        if pattern_parts.is_empty() {
            return false;
        }
        if pattern_parts.len() == 1 {
            return path_parts
                .last()
                .is_some_and(|name| wildcard_segment_matches(name, pattern_parts[0]));
        }
        if pattern_parts.first() == Some(&"**") {
            return glob_segments_match(&path_parts, &pattern_parts);
        }
        if pattern_parts.len() > path_parts.len() {
            return false;
        }
        glob_segments_match(
            &path_parts[path_parts.len() - pattern_parts.len()..],
            &pattern_parts,
        )
    })
}

#[pyfunction]
fn is_mutation_test_path_native(path: &str, patterns: Vec<String>) -> bool {
    is_mutation_test_path(path, &patterns)
}

/// Read the attribute path out of a `#[...]` line: `#[tokio::test]` is
/// `tokio::test`, `#[cfg(test)]` is `cfg`.
fn rust_attribute_path(trimmed: &str) -> Option<&str> {
    let rest = trimmed.strip_prefix("#[")?;
    let end = rest.find(['(', ']'])?;
    Some(rest[..end].trim())
}

/// Does this Rust source declare tests?
///
/// Rust puts unit tests in the module they test, so no filename glob can find
/// them: `**/test_*.rs` matches nothing real, and an inventory built from
/// globs alone reports zero Rust test files while the repository has dozens.
/// Deciding it needs the file's contents, not its name.
///
/// The rule is the attribute path, the same one `mutation_manifest`'s nodeid
/// reader applies: an attribute whose final segment is `test` declares a test,
/// so `#[test]` and `#[tokio::test]` both count, while `#[cfg(test)]` and
/// `#[cfg_attr(test, ..)]` gate code rather than declare one and do not.
/// Requiring `#[` at the START of the trimmed line is also what excludes a
/// commented-out test: `// #[test]` does not begin an attribute. A separate
/// comment guard reads as protection but is unreachable, and a mutation run
/// proved it -- deleting it changed no outcome.
fn declares_rust_tests(source: &str) -> bool {
    source.lines().any(|line| {
        rust_attribute_path(line.trim())
            .is_some_and(|path| path.rsplit("::").next().unwrap_or(path) == "test")
    })
}

/// A `.rs` file is a test surface when it declares tests. An unreadable file
/// is not one: the inventory reports what it can see, and a path git lists
/// that the filesystem cannot open is a different failure.
fn is_rust_test_surface(repo_root: &Path, relative: &str) -> bool {
    relative.ends_with(".rs")
        && fs::read_to_string(repo_root.join(relative))
            .is_ok_and(|source| declares_rust_tests(&source))
}

/// Is this path a mutation test surface?
///
/// Two questions, because neither language answers for the other: the registry's
/// globs decide it for every named test file, and the file's contents decide it
/// for Rust. Asking only the first is what left the inventory blind to a tree of
/// unit tests; asking only the second would drop every Python and JavaScript
/// test on the floor.
fn is_inventory_surface(repo_root: &Path, normalized: &str, patterns: &[String]) -> bool {
    is_mutation_test_path(normalized, patterns) || is_rust_test_surface(repo_root, normalized)
}

#[pyfunction]
fn mutation_rust_test_surface_native(repo_root: &str, path: &str) -> bool {
    is_rust_test_surface(Path::new(repo_root), path)
}

fn git_paths(repo_root: &str, args: &[String]) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .map_err(|error| format!("git {} failed: {error}", args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        return Err(format!("git {} failed: {detail}", args.join(" ")));
    }
    let stdout = String::from_utf8(output.stdout)
        .map_err(|error| format!("git {} produced non-UTF-8 output: {error}", args.join(" ")))?;
    Ok(stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.replace('\\', "/"))
        .collect())
}

#[pyfunction]
fn mutation_git_paths_native(repo_root: &str, args: Vec<String>) -> PyResult<Vec<String>> {
    git_paths(repo_root, &args).map_err(value_error)
}

fn should_skip_mutation_path(path: &str, skip_directory_names: &HashSet<&str>) -> bool {
    let normalized = path.replace('\\', "/");
    let parts: Vec<&str> = normalized
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    parts.iter().any(|part| skip_directory_names.contains(part))
        || (parts.contains(&"research") && parts.contains(&"cache"))
}

#[pyfunction]
fn should_skip_mutation_path_native(path: &str, skip_directory_names: Vec<String>) -> bool {
    let skip: HashSet<&str> = skip_directory_names.iter().map(String::as_str).collect();
    should_skip_mutation_path(path, &skip)
}

#[pyfunction]
fn mutation_test_inventory_native(
    repo_root: &str,
    registry_path: &str,
    canonical_patterns: Vec<String>,
    skip_directory_names: Vec<String>,
    mode: &str,
    include_untracked: bool,
) -> PyResult<Vec<String>> {
    let patterns =
        registry_patterns(repo_root, registry_path, &canonical_patterns).map_err(value_error)?;
    let mut raw_paths = match mode {
        "all" => git_paths(repo_root, &["ls-files".to_owned()]).map_err(value_error)?,
        "changed" => git_paths(
            repo_root,
            &[
                "diff".to_owned(),
                "--name-only".to_owned(),
                "HEAD".to_owned(),
            ],
        )
        .map_err(value_error)?,
        _ => {
            return Err(value_error(format!(
                "mutation inventory mode must be 'all' or 'changed', got {mode:?}"
            )));
        }
    };
    if include_untracked || mode == "changed" {
        raw_paths.extend(
            git_paths(
                repo_root,
                &[
                    "ls-files".to_owned(),
                    "--others".to_owned(),
                    "--exclude-standard".to_owned(),
                ],
            )
            .map_err(value_error)?,
        );
    }

    let skip: HashSet<&str> = skip_directory_names.iter().map(String::as_str).collect();
    let mut seen = HashSet::with_capacity(raw_paths.len());
    let mut selected = Vec::new();
    for raw in raw_paths {
        let path = raw.replace('\\', "/");
        let normalized = path
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect::<Vec<_>>()
            .join("/");
        if !seen.insert(normalized.clone())
            || should_skip_mutation_path(&normalized, &skip)
            || !is_inventory_surface(Path::new(repo_root), &normalized, &patterns)
        {
            continue;
        }
        selected.push(normalized);
    }
    selected.sort_unstable();
    Ok(selected)
}

pub fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(normalize_mutation_path_native, module)?)?;
    module.add_function(wrap_pyfunction!(mutation_registry_patterns_native, module)?)?;
    module.add_function(wrap_pyfunction!(is_mutation_test_path_native, module)?)?;
    module.add_function(wrap_pyfunction!(mutation_git_paths_native, module)?)?;
    module.add_function(wrap_pyfunction!(should_skip_mutation_path_native, module)?)?;
    module.add_function(wrap_pyfunction!(mutation_rust_test_surface_native, module)?)?;
    module.add_function(wrap_pyfunction!(mutation_test_inventory_native, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        declares_rust_tests, is_inventory_surface, is_rust_test_surface, rust_attribute_path,
    };

    #[test]
    fn attribute_path_stops_at_the_first_delimiter() {
        assert_eq!(rust_attribute_path("#[test]"), Some("test"));
        assert_eq!(rust_attribute_path("#[tokio::test]"), Some("tokio::test"));
        assert_eq!(rust_attribute_path("#[cfg(test)]"), Some("cfg"));
        assert_eq!(rust_attribute_path("#[ cfg_attr (test)]"), Some("cfg_attr"));
        assert_eq!(rust_attribute_path("fn test() {}"), None);
        assert_eq!(rust_attribute_path("#[unterminated"), None);
    }

    #[test]
    fn a_test_attribute_marks_the_file() {
        assert!(declares_rust_tests("#[test]\nfn one() {}\n"));
        assert!(declares_rust_tests(
            "    #[tokio::test]\n    async fn two() {}\n"
        ));
    }

    #[test]
    fn gating_attributes_are_not_test_declarations() {
        // The whole point of the attribute-path rule: `#[cfg(test)]` marks the
        // module a test lives in, and a file can carry one with every test
        // since removed.
        assert!(!declares_rust_tests("#[cfg(test)]\nmod tests {}\n"));
        assert!(!declares_rust_tests(
            "#[cfg_attr(test, derive(Debug))]\nstruct S;\n"
        ));
    }

    #[test]
    fn a_commented_out_test_does_not_count() {
        assert!(!declares_rust_tests("// #[test]\n// fn gone() {}\n"));
        assert!(!declares_rust_tests(
            "/// #[tokio::test]\n/// async fn gone() {}\n"
        ));
        // The attribute has to OPEN the line; trailing prose after a real one
        // is still a declaration.
        assert!(declares_rust_tests(
            "#[test] // still a test\nfn one() {}\n"
        ));
    }

    #[test]
    fn plain_rust_source_declares_nothing() {
        assert!(!declares_rust_tests("pub fn one() -> u8 {\n    1\n}\n"));
        // Ordinary Rust is full of attributes; carrying one is not declaring a
        // test, or every crate in the tree would be a test surface.
        assert!(!declares_rust_tests(
            "#[derive(Debug)]\npub struct S;\n\n#[inline]\npub fn two() -> u8 {\n    2\n}\n"
        ));
        assert!(!declares_rust_tests(""));
    }

    #[test]
    fn only_rust_paths_are_rust_surfaces() {
        let dir = std::env::temp_dir().join(format!(
            "conductor-native-rust-surface-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("with.rs"), "#[test]\nfn one() {}\n").expect("write");
        std::fs::write(dir.join("without.rs"), "pub fn one() {}\n").expect("write");
        std::fs::write(dir.join("with.py"), "#[test]\n").expect("write");

        assert!(is_rust_test_surface(&dir, "with.rs"));
        assert!(!is_rust_test_surface(&dir, "without.rs"));
        // A `.py` file carrying the same bytes is not a Rust surface, and an
        // absent path is not one either.
        assert!(!is_rust_test_surface(&dir, "with.py"));
        assert!(!is_rust_test_surface(&dir, "absent.rs"));

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn a_surface_is_admitted_by_either_name_or_contents() {
        let dir = std::env::temp_dir().join(format!(
            "conductor-native-inventory-surface-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("lib.rs"), "#[test]\nfn one() {}\n").expect("write");
        std::fs::write(dir.join("plumbing.rs"), "pub fn one() {}\n").expect("write");
        let patterns = vec!["test_*.py".to_owned()];

        // Named like a test but absent from disk: the glob still admits it.
        assert!(is_inventory_surface(&dir, "test_thing.py", &patterns));
        // No glob matches a Rust unit test; only its contents do.
        assert!(is_inventory_surface(&dir, "lib.rs", &patterns));
        assert!(!is_inventory_surface(&dir, "plumbing.rs", &patterns));

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
