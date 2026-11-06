//! Bounded fallback inventory and runtime registry selection.
use super::inputs::Inputs;
use anyhow::{ensure, Context, Result};
use conductor_native::contract_registry::{self, Registry};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

const REGISTRY: &str = "native/conductor-native/src/python_contract_targets.tsv";
const EXTENSION: &str = "native/conductor-native/src/python_contract_targets_extra.tsv";
const DIRECTIVE: &str = "# include: python_contract_targets_extra.tsv";
const TEST_DIR: &str = "native/conductor-native/tests";

pub(super) fn python_tests(inputs: &Inputs<'_>, needed: bool) -> Result<Vec<String>> {
    if !needed {
        return Ok(Vec::new());
    }
    let mut selected = Vec::new();
    for entry in inputs.entries().values() {
        if entry.path.ends_with(".py") && conductor_native::graph_context::is_test_path(&entry.path)
        {
            ensure!(
                matches!(entry.mode.as_str(), "100644" | "100755") && entry.object_type == "blob",
                "conventional test must be a regular Git blob: {}",
                entry.path
            );
            selected.push(entry.path.clone());
            ensure!(
                selected.len() <= 4096,
                "conventional test inventory exceeds 4096 files"
            );
        }
    }
    Ok(selected)
}
fn owner(inputs: &Inputs<'_>, path: &str) -> Option<String> {
    let mut parent = std::path::Path::new(path).parent();
    while let Some(directory) = parent {
        let root = directory.to_str()?;
        let manifest = if root.is_empty() {
            "Cargo.toml".to_owned()
        } else {
            format!("{root}/Cargo.toml")
        };
        if inputs.entries().contains_key(&manifest) {
            return Some(root.to_owned());
        }
        parent = directory.parent();
    }
    None
}
pub(super) fn rust_tests(
    inputs: &mut Inputs<'_>,
    sources: &[String],
) -> Result<Map<String, Value>> {
    let mut owners = BTreeMap::new();
    for source in sources.iter().filter(|path| path.ends_with(".rs")) {
        if let Some(root) = owner(inputs, source) {
            owners.insert(source.clone(), root);
        }
    }
    let roots: BTreeSet<_> = owners.values().cloned().collect();
    ensure!(
        roots.len() <= 256
            && roots
                .len()
                .checked_mul(inputs.entries().len())
                .is_some_and(|n| n <= 10_000_000),
        "Rust test inventory exceeds work limit"
    );
    let path_work = inputs.entries().keys().try_fold(0usize, |sum, path| {
        let depth = path.bytes().filter(|b| *b == b'/').count() + 1;
        sum.checked_add(
            path.len()
                .checked_mul(depth)
                .context("Rust path work overflow")?,
        )
        .context("Rust path work overflow")
    })?;
    ensure!(
        path_work
            .checked_mul(roots.len())
            .is_some_and(|n| n <= 100_000_000),
        "Rust test inventory path work exceeds limit"
    );
    let mut cache = BTreeMap::new();
    for root in roots {
        cache.insert(root.clone(), rust_tests_in_crate(inputs, &root)?);
    }
    let mut result = Map::new();
    let mut retained = 0usize;
    for (source, root) in owners {
        let Some(tests) = cache.get(&root).filter(|tests| !tests.is_empty()) else {
            continue;
        };
        retained = retained
            .checked_add(source.len())
            .context("Rust test output overflow")?;
        for path in tests {
            retained = retained
                .checked_add(path.len() + 16)
                .context("Rust test output overflow")?;
        }
        ensure!(
            retained <= 16 * 1024 * 1024,
            "Rust test output exceeds 16 MiB"
        );
        result.insert(source, json!(tests));
    }
    Ok(result)
}

fn rust_tests_in_crate(inputs: &mut Inputs<'_>, root: &str) -> Result<Vec<String>> {
    let manifest = if root.is_empty() {
        "Cargo.toml".into()
    } else {
        format!("{root}/Cargo.toml")
    };
    let manifest_value: toml::Value = toml::from_str(inputs.read(&manifest)?)
        .context("captured Cargo manifest must be valid TOML")?;
    ensure!(
        manifest_value
            .get("package")
            .and_then(toml::Value::as_table)
            .is_some(),
        "Rust owner Cargo manifest lacks a package"
    );
    let prefix = if root.is_empty() {
        String::new()
    } else {
        format!("{root}/")
    };
    let files = inputs
        .entries()
        .keys()
        .filter(|path| {
            path.starts_with(&prefix)
                && path.ends_with(".rs")
                && !path.split('/').any(|part| part == "target")
                && owner(inputs, path).as_deref() == Some(root)
        })
        .cloned()
        .collect::<Vec<_>>();
    ensure!(
        files.len() <= 4096,
        "Rust crate test inventory exceeds 4096 files"
    );
    let mut tests = Vec::new();
    for path in files {
        let in_tests = std::path::Path::new(&path)
            .parent()
            .is_some_and(|parent| parent == std::path::Path::new(&prefix).join("tests"));
        let text = inputs.read(&path)?;
        // The bounded tree-sitter admission precedes syn's test discovery.
        conductor_native::candidate_rust_artifact::production_source(text)
            .map_err(anyhow::Error::msg)?;
        let has_tests = conductor_native::candidate_verification::native_rust_file_has_tests(text)
            .map_err(anyhow::Error::msg)?;
        if in_tests || has_tests {
            tests.push(path);
        }
    }
    Ok(tests)
}

pub(super) struct Contracts {
    pub sources: Vec<String>,
    pub targets: Vec<String>,
    pub paths: Vec<String>,
    pub absent_relevant: bool,
}
fn registry(
    inputs: &mut Inputs<'_>,
    external: Option<(&std::path::Path, &str)>,
) -> Result<Option<Registry>> {
    if !inputs.entries().contains_key(REGISTRY) {
        inputs.absent(REGISTRY)?;
        ensure!(
            !inputs.entries().contains_key(EXTENSION),
            "orphaned candidate contract registry extension"
        );
        inputs.absent(EXTENSION)?;
        return external
            .map(|(path, digest)| {
                let text = inputs.external_registry(path, digest)?;
                ensure!(
                    !text.lines().any(|line| line == DIRECTIVE),
                    "external registry must be consolidated; unresolved include is forbidden"
                );
                contract_registry::parse_registry(&text)
            })
            .transpose();
    }
    let mut base = inputs.read(REGISTRY)?.to_owned();
    if base.lines().any(|line| line == DIRECTIVE) {
        base.push('\n');
        base.push_str(inputs.read(EXTENSION)?);
    } else if inputs.entries().contains_key(EXTENSION) {
        anyhow::bail!("contract registry extension exists but is not declared");
    } else {
        inputs.absent(EXTENSION)?;
    }
    Ok(Some(contract_registry::parse_registry(&base)?))
}
pub(super) fn contracts(
    inputs: &mut Inputs<'_>,
    changes: &[String],
    external: Option<(&std::path::Path, &str)>,
) -> Result<Contracts> {
    validate_changed_registries(inputs, changes)?;
    let Some(registry) = registry(inputs, external)? else {
        return Ok(Contracts {
            sources: Vec::new(),
            targets: Vec::new(),
            paths: Vec::new(),
            absent_relevant: changes
                .iter()
                .any(|path| contract_registry::could_map(path)),
        });
    };
    let registered: BTreeSet<String> = registry.targets().map(str::to_owned).collect();
    let mut all = registered.clone();
    for path in inputs.entries().keys() {
        if let Some(name) = path
            .strip_prefix(&format!("{TEST_DIR}/"))
            .and_then(|name| name.strip_suffix(".rs"))
        {
            if name.starts_with("python_contracts_") && !name.contains('/') {
                ensure!(
                    name.bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'),
                    "invalid captured contract target name"
                );
                all.insert(name.to_owned());
            }
        }
    }
    let mut sources = BTreeSet::new();
    let mut selected = false;
    for path in changes {
        if registry.targets_for(path).next().is_some() {
            selected = true;
            sources.insert(path.clone());
        }
        if path == REGISTRY || path == EXTENSION {
            selected = true;
        }
        if let Some(name) = path
            .strip_prefix(&format!("{TEST_DIR}/"))
            .and_then(|name| name.strip_suffix(".rs"))
        {
            if name.starts_with("python_contracts_") && !name.contains('/') {
                ensure!(
                    registered.contains(name),
                    "changed contract target missing from captured registry"
                );
                selected = true;
            }
        }
        if path.starts_with(&format!("{TEST_DIR}/python_contracts/")) {
            ensure!(
                registry.targets_for(path).next().is_some(),
                "changed contract helper has no captured mapping"
            );
        }
    }
    if !selected {
        return Ok(Contracts {
            sources: Vec::new(),
            targets: Vec::new(),
            paths: Vec::new(),
            absent_relevant: false,
        });
    }
    ensure!(all.len() <= 4096, "contract target inventory exceeds 4096");
    let targets: Vec<_> = all.into_iter().collect();
    validate_manifest(inputs, &targets)?;
    let paths = targets
        .iter()
        .map(|target| format!("{TEST_DIR}/{target}.rs"))
        .collect::<Vec<_>>();
    for path in &paths {
        inputs.read(path)?;
    }
    Ok(Contracts {
        sources: sources.into_iter().collect(),
        targets,
        paths,
        absent_relevant: false,
    })
}

fn validate_changed_registries(inputs: &mut Inputs<'_>, changes: &[String]) -> Result<()> {
    if changes.iter().any(|path| path == REGISTRY) {
        ensure!(
            inputs.entries().contains_key(REGISTRY),
            "changed candidate contract registry is missing"
        );
    }
    if changes.iter().any(|path| path == EXTENSION) {
        ensure!(
            inputs.entries().contains_key(REGISTRY) && inputs.entries().contains_key(EXTENSION),
            "changed candidate contract registry extension is missing"
        );
        ensure!(
            inputs.read(REGISTRY)?.lines().any(|line| line == DIRECTIVE),
            "changed contract registry extension is not declared"
        );
    }
    Ok(())
}

fn validate_manifest(inputs: &mut Inputs<'_>, targets: &[String]) -> Result<()> {
    let manifest: toml::Value = toml::from_str(inputs.read("native/conductor-native/Cargo.toml")?)
        .context("captured contract Cargo manifest must be valid TOML")?;
    let package = manifest
        .get("package")
        .and_then(toml::Value::as_table)
        .context("contract Cargo package table missing")?;
    let automatic = package
        .get("autotests")
        .map(|value| value.as_bool().context("Cargo autotests must be boolean"))
        .transpose()?
        .unwrap_or(true);
    let entries = manifest
        .get("test")
        .map(|value| {
            value
                .as_array()
                .context("Cargo test targets must be an array")
        })
        .transpose()?
        .cloned()
        .unwrap_or_default();
    ensure!(
        entries.len() <= 4096,
        "Cargo test target count exceeds limit"
    );
    let mut configured = BTreeMap::new();
    for entry in entries {
        let entry = entry
            .as_table()
            .context("Cargo test target must be a table")?;
        let name = entry
            .get("name")
            .and_then(toml::Value::as_str)
            .context("Cargo test target name missing")?;
        let path = entry
            .get("path")
            .map(|value| {
                value
                    .as_str()
                    .context("Cargo test target path must be a string")
            })
            .transpose()?;
        ensure!(
            configured
                .insert(name.to_owned(), path.map(str::to_owned))
                .is_none(),
            "duplicate Cargo test target name"
        );
    }
    for target in targets {
        match configured.get(target) {
            Some(Some(path)) => ensure!(
                path == &format!("tests/{target}.rs"),
                "contract target has unsupported nonstandard path: {target}"
            ),
            Some(None) => {}
            None => ensure!(
                automatic,
                "contract target is not configured and Cargo autotests are disabled: {target}"
            ),
        }
    }
    Ok(())
}
