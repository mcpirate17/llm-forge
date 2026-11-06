//! Validate confined Cargo metadata and select the campaign's package-wide suite.
use anyhow::{ensure, Context, Result};
use conductor_native::mutation_workspace_selector::{Selector, Target};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug)]
pub(crate) struct Selection {
    pub baseline_args: Vec<String>,
    pub engine_args: Vec<String>,
    pub package_id: String,
    pub package_name: String,
    pub package_version: String,
    pub targets: Vec<String>,
    pub test_scope: String,
    pub engine_sources: Vec<String>,
    pub report_root: String,
    pub selector: Option<Selector>,
    pub selected_target: Option<ResolvedTarget>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct ResolvedTarget {
    pub kind: String,
    pub name: String,
    pub source: String,
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("Cargo metadata missing {key}"))
}
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    value
        .get(key)
        .and_then(Value::as_array)
        .with_context(|| format!("Cargo metadata missing {key}"))
}
fn inside(root: &Path, value: &str) -> Result<PathBuf> {
    inside_optional(root, value, false)
}

/// Only unused dependency dev targets may have an absent suffix.
fn inside_optional(root: &Path, value: &str, allow_missing: bool) -> Result<PathBuf> {
    let path = Path::new(value);
    ensure!(
        root.is_absolute() && root.canonicalize()? == root,
        "metadata root is not canonical"
    );
    ensure!(
        value.len() <= 4096 && !value.bytes().any(|b| b.is_ascii_control()),
        "invalid metadata path"
    );
    ensure!(
        path.is_absolute() && path.starts_with(root),
        "metadata path outside admitted root: {value}"
    );
    let mut normalized = root.to_owned();
    let mut missing = false;
    for component in path.strip_prefix(root)?.components() {
        match component {
            Component::Normal(name) => {
                normalized.push(name);
                if !missing {
                    match std::fs::symlink_metadata(&normalized) {
                        Ok(meta) => ensure!(
                            !meta.file_type().is_symlink(),
                            "metadata path traverses symlink: {value}"
                        ),
                        Err(error)
                            if allow_missing && error.kind() == std::io::ErrorKind::NotFound =>
                        {
                            missing = true
                        }
                        Err(error) => {
                            return Err(error).with_context(|| {
                                format!("metadata path component {}", normalized.display())
                            })
                        }
                    }
                }
            }
            Component::ParentDir => {
                ensure!(!missing, "metadata parent after absent component: {value}");
                ensure!(
                    normalized != root,
                    "metadata path escapes admitted root: {value}"
                );
                normalized.pop();
            }
            _ => anyhow::bail!("unsupported metadata path component: {value}"),
        }
    }
    ensure!(
        normalized.starts_with(root),
        "metadata path escapes admitted root: {value}"
    );
    if !missing {
        ensure!(
            path.canonicalize()
                .with_context(|| format!("canonicalizing metadata path {value}"))?
                == normalized,
            "metadata path is aliased: {value}"
        );
    }
    Ok(normalized)
}

fn verify_pins(root: &Path, pins: &BTreeMap<String, String>) -> Result<()> {
    ensure!(
        !pins.is_empty() && pins.len() <= 100_000,
        "missing or excessive Cargo pins"
    );
    for (path, expected) in pins {
        crate::candidate_source::safe_path(path.as_bytes())?;
        let (_, bytes) =
            crate::candidate_inputs::repository_live(&root.join(path), 32 * 1024 * 1024)?;
        ensure!(
            format!("{:x}", Sha256::digest(&bytes)) == *expected,
            "Cargo pinned input changed: {path}"
        );
    }
    Ok(())
}
fn target_inventory(root: &Path, targets: &[Value]) -> Result<Vec<String>> {
    ensure!(targets.len() <= 4096, "Cargo target bound exceeded");
    let mut names = BTreeSet::new();
    for target in targets {
        let name = text(target, "name")?;
        let kinds = array(target, "kind")?;
        ensure!(!kinds.is_empty(), "Cargo target kind missing");
        let kinds: Vec<&str> = kinds
            .iter()
            .map(|v| v.as_str().context("invalid target kind"))
            .collect::<Result<_>>()?;
        inside(root, text(target, "src_path")?)?;
        ensure!(
            names.insert(format!("{}:{name}", kinds.join(","))),
            "duplicate Cargo target"
        );
    }
    Ok(names.into_iter().collect())
}

fn verify_harness(manifest: &Path) -> Result<()> {
    let (_, bytes) = crate::candidate_inputs::repository_live(manifest, 1024 * 1024)?;
    let value: toml::Value = std::str::from_utf8(&bytes)?.parse()?;
    let mut targets = Vec::new();
    if let Some(lib) = value.get("lib") {
        targets.push(lib);
    }
    for key in ["test", "bin", "example"] {
        if let Some(items) = value.get(key) {
            targets.extend(
                items
                    .as_array()
                    .with_context(|| format!("invalid Cargo {key} targets"))?,
            );
        }
    }
    for target in targets {
        let table = target.as_table().context("invalid Cargo target")?;
        if let Some(harness) = table.get("harness") {
            ensure!(
                harness.as_bool() == Some(true),
                "custom Cargo test harness unsupported"
            );
        }
    }
    Ok(())
}

fn selector_table<'a>(
    manifest: &'a toml::Value,
    selector: &Selector,
) -> Result<Option<&'a toml::value::Table>> {
    match &selector.target {
        Target::Lib {} => manifest
            .get("lib")
            .map(|v| v.as_table().context("invalid Cargo lib target"))
            .transpose(),
        Target::Test { name } => {
            let mut matching = Vec::new();
            if let Some(items) = manifest.get("test") {
                for item in items.as_array().context("invalid Cargo test targets")? {
                    let table = item.as_table().context("invalid Cargo test target")?;
                    if table.get("name").and_then(toml::Value::as_str) == Some(name.as_str()) {
                        matching.push(table);
                    }
                }
            }
            ensure!(
                matching.len() <= 1,
                "ambiguous selected Cargo target declaration"
            );
            Ok(matching.into_iter().next())
        }
    }
}
fn standard_selected_target(target: &Value, table: Option<&toml::value::Table>) -> Result<()> {
    ensure!(
        target.get("test").and_then(Value::as_bool) == Some(true),
        "selected Cargo target is not test-enabled"
    );
    if let Some(features) = target.get("required-features") {
        ensure!(
            features.as_array().is_some_and(Vec::is_empty),
            "selected feature-requiring Cargo target unsupported"
        );
    }
    if let Some(table) = table {
        for key in ["harness", "test"] {
            if let Some(value) = table.get(key) {
                ensure!(
                    value.as_bool() == Some(true),
                    "selected Cargo target {key}=false unsupported"
                );
            }
        }
        if let Some(features) = table.get("required-features") {
            ensure!(
                features.as_array().is_some_and(Vec::is_empty),
                "selected feature-requiring Cargo target unsupported"
            );
        }
    }
    Ok(())
}
fn selected_source(
    root: &Path,
    manifest: &Path,
    selector: &Selector,
    table: Option<&toml::value::Table>,
    declared: &toml::Value,
) -> Result<PathBuf> {
    let package = manifest
        .parent()
        .context("selected manifest parent missing")?;
    if let Some(path) = table.and_then(|t| t.get("path")) {
        return inside(
            root,
            package
                .join(path.as_str().context("invalid Cargo target path")?)
                .to_str()
                .context("target path UTF8")?,
        );
    }
    if table.is_none() {
        let discovery = match &selector.target {
            Target::Lib {} => "autolib",
            Target::Test { .. } => "autotests",
        };
        if let Some(value) = declared.get("package").and_then(|p| p.get(discovery)) {
            ensure!(
                value.as_bool() == Some(true),
                "selected Cargo target has {discovery}=false without explicit declaration"
            );
        }
    }
    let candidates = match &selector.target {
        Target::Lib {} => vec![package.join("src/lib.rs")],
        Target::Test { name } => vec![
            package.join(format!("tests/{name}.rs")),
            package.join(format!("tests/{name}/main.rs")),
        ],
    };
    let mut existing = Vec::new();
    for path in candidates {
        match std::fs::symlink_metadata(&path) {
            Ok(_) => existing.push(path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e).context("selected Cargo source lookup"),
        }
    }
    ensure!(
        existing.len() == 1,
        "selected Cargo source is missing or ambiguous"
    );
    inside(root, existing[0].to_str().context("target source UTF8")?)
}
fn resolve_target(
    root: &Path,
    manifest: &Path,
    package: &Value,
    selector: &Selector,
) -> Result<ResolvedTarget> {
    selector.validate().map_err(anyhow::Error::msg)?;
    let mut matching = Vec::new();
    for target in array(package, "targets")? {
        let kinds = array(target, "kind")?;
        let matches = match &selector.target {
            Target::Lib {} => {
                !kinds.is_empty()
                    && kinds.iter().all(|kind| {
                        kind.as_str().is_some_and(
                            conductor_native::mutation_workspace_selector::is_library_kind,
                        )
                    })
                    && kinds
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<BTreeSet<_>>()
                        .len()
                        == kinds.len()
            }
            Target::Test { name } => {
                kinds.len() == 1
                    && kinds[0].as_str() == Some("test")
                    && text(target, "name")? == name
            }
        };
        if matches {
            matching.push(target);
        }
    }
    ensure!(
        matching.len() == 1,
        "selected Cargo target is missing or ambiguous"
    );
    let target = matching[0];
    let (_, bytes) = crate::candidate_inputs::repository_live(manifest, 1024 * 1024)?;
    let declared: toml::Value = std::str::from_utf8(&bytes)?.parse()?;
    ensure!(
        declared
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
            == Some(text(package, "name")?),
        "Cargo package name differs from held manifest"
    );
    let table = selector_table(&declared, selector)?;
    standard_selected_target(target, table)?;
    if let Some(name) = table.and_then(|t| t.get("name")) {
        ensure!(
            name.as_str() == Some(text(target, "name")?),
            "Cargo target name differs from held manifest"
        );
    }
    let source = selected_source(root, manifest, selector, table, &declared)?;
    ensure!(
        source == inside(root, text(target, "src_path")?)?,
        "selected Cargo source differs from held manifest"
    );
    crate::candidate_inputs::repository_live(&source, 32 * 1024 * 1024)?;
    Ok(ResolvedTarget {
        kind: match &selector.target {
            Target::Lib {} => "lib",
            Target::Test { .. } => "test",
        }
        .into(),
        name: text(target, "name")?.into(),
        source: source
            .strip_prefix(root)?
            .to_str()
            .context("selected source UTF8")?
            .into(),
    })
}
fn command_args(
    manifest: &str,
    package: &str,
    selector: Option<&Selector>,
) -> (Vec<String>, Vec<String>) {
    let mut baseline = vec![
        "cargo".into(),
        "test".into(),
        "--manifest-path".into(),
        manifest.into(),
        "--package".into(),
        package.into(),
        "--locked".into(),
        "--offline".into(),
    ];
    let mut engine = vec!["--test-package".into(), package.into()];
    if let Some(selector) = selector {
        baseline.extend(selector.cargo_args());
        baseline.extend(selector.libtest_args());
        engine.extend(selector.engine_args());
    }
    (baseline, engine)
}

fn source_mapping(
    root: &Path,
    workspace: &Path,
    sources: &BTreeMap<String, String>,
) -> Result<(Vec<String>, String)> {
    let workspace_relative = workspace
        .strip_prefix(root)?
        .to_str()
        .context("workspace path is not UTF-8")?;
    if !workspace_relative.is_empty() {
        crate::candidate_source::safe_path(workspace_relative.as_bytes())?;
    }
    let mut selected = Vec::with_capacity(sources.len());
    for path in sources.keys() {
        let full = root.join(path);
        let relative = full
            .strip_prefix(workspace)
            .context("selected source outside Cargo workspace")?;
        let relative = relative.to_str().context("source path is not UTF-8")?;
        crate::candidate_source::safe_path(relative.as_bytes())?;
        ensure!(
            !relative.contains(['*', '?', '[', ']', '{', '}', '\\']),
            "Cargo source has glob metacharacters"
        );
        ensure!(relative.contains('/'),
            "cargo-mutants treats slashless source {relative:?} as a recursive basename glob; root-level workspace sources are unsupported");
        selected.push(relative.to_owned());
    }
    Ok((selected, workspace_relative.to_owned()))
}

fn selected_package<'a>(
    data: &'a Value,
    root: &Path,
    cargo_home: &Path,
    selected_manifest: &Path,
    package: &str,
) -> Result<&'a Value> {
    let members: BTreeSet<&str> = array(data, "workspace_members")?
        .iter()
        .map(|v| v.as_str().context("invalid workspace member"))
        .collect::<Result<_>>()?;
    ensure!(
        !members.is_empty() && members.len() <= 4096,
        "invalid Cargo workspace members"
    );
    let packages = array(data, "packages")?;
    ensure!(
        packages.len() <= 4096,
        "Cargo metadata package bound exceeded"
    );
    let mut ids = BTreeSet::new();
    let mut selected = None;
    for item in packages {
        let id = text(item, "id")?;
        ensure!(ids.insert(id), "duplicate Cargo package id");
        let name = text(item, "name")?;
        let path = text(item, "manifest_path")?;
        let admitted = match item.get("source") {
            Some(Value::Null) => root.to_owned(),
            Some(Value::String(source))
                if source == "registry+https://github.com/rust-lang/crates.io-index" =>
            {
                cargo_home.join("registry/src")
            }
            _ => anyhow::bail!("unsupported Cargo package source"),
        };
        let normalized_manifest = inside(&admitted, path)
            .with_context(|| format!("Cargo package {name} ({id}) manifest {path}"))?;
        let is_selected = name == package && members.contains(id);
        for target in array(item, "targets")? {
            let target_name = text(target, "name")?;
            let target_path = text(target, "src_path")?;
            let kinds = array(target, "kind")?;
            let dev_only = !kinds.is_empty()
                && kinds
                    .iter()
                    .all(|kind| matches!(kind.as_str(), Some("example" | "test" | "bench")));
            inside_optional(&admitted, target_path, !is_selected && dev_only).with_context(|| {
                format!("Cargo package {name} ({id}) target {target_name} kinds {kinds:?} path {target_path}")
            })?;
        }
        if is_selected {
            ensure!(selected.is_none(), "ambiguous Cargo workspace package name");
            ensure!(
                normalized_manifest == selected_manifest,
                "Cargo selected manifest mismatch"
            );
            selected = Some(item);
        }
    }
    ensure!(
        members.iter().all(|id| ids.contains(id)),
        "workspace member missing from metadata"
    );
    let resolve = data
        .get("resolve")
        .filter(|v| !v.is_null())
        .context("Cargo resolve graph missing")?;
    let mut nodes = BTreeSet::new();
    for node in array(resolve, "nodes")? {
        let id = text(node, "id")?;
        ensure!(
            ids.contains(id) && nodes.insert(id),
            "invalid Cargo resolve node"
        );
        for dependency in array(node, "deps")? {
            ensure!(
                ids.contains(text(dependency, "pkg")?),
                "Cargo resolve dependency missing"
            );
        }
    }
    ensure!(
        members.iter().all(|id| nodes.contains(id)),
        "workspace resolve node missing"
    );
    selected.context("selected Cargo workspace package missing")
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn validate(
    bytes: &[u8],
    root: &Path,
    cargo_home: &Path,
    lockfile: &str,
    manifest: &str,
    package: &str,
    sources: &BTreeMap<String, String>,
    tests: &BTreeMap<String, String>,
    selector: Option<&Selector>,
) -> Result<Selection> {
    ensure!(
        bytes.len() <= 16 * 1024 * 1024,
        "Cargo metadata exceeds bound"
    );
    ensure!(
        root.is_absolute() && root.canonicalize()? == root,
        "snapshot root is not canonical"
    );
    ensure!(
        cargo_home.is_absolute() && cargo_home.canonicalize()? == cargo_home,
        "Cargo home is not canonical"
    );
    crate::candidate_source::safe_path(lockfile.as_bytes())?;
    crate::candidate_source::safe_path(manifest.as_bytes())?;
    let data = crate::tool_schema::parse_document_with_limit(
        std::str::from_utf8(bytes)?,
        16 * 1024 * 1024,
    )?;
    ensure!(
        data.get("version").and_then(Value::as_u64) == Some(1),
        "unsupported Cargo metadata version"
    );
    let workspace = inside(root, text(&data, "workspace_root")?)?;
    ensure!(
        workspace.join("Cargo.lock") == root.join(lockfile),
        "Cargo metadata lockfile differs from grant"
    );
    inside(root, root.join(lockfile).to_str().context("lockfile path")?)?;
    let selected_manifest = inside(root, root.join(manifest).to_str().context("manifest path")?)?;
    verify_pins(root, sources)?;
    verify_pins(root, tests)?;
    let selected = selected_package(&data, root, cargo_home, &selected_manifest, package)?;
    let package_id = text(selected, "id")?.to_owned();
    let package_version = text(selected, "version")?.to_owned();
    ensure!(!package_version.is_empty(), "Cargo package version missing");
    let selected_target = match selector {
        Some(selector) => Some(resolve_target(
            root,
            &selected_manifest,
            selected,
            selector,
        )?),
        None => {
            verify_harness(&selected_manifest)?;
            None
        }
    };
    let targets = target_inventory(root, array(selected, "targets")?)?;
    let (engine_sources, report_root) = source_mapping(root, &workspace, sources)?;
    let (baseline_args, engine_args) = command_args(manifest, package, selector);
    Ok(Selection {
        baseline_args,
        engine_args,
        package_id,
        package_name: package.into(),
        package_version,
        targets,
        test_scope: if selector.is_some() {
            "selected-target-full-source-snapshot"
        } else {
            "package-wide-full-source-snapshot"
        }
        .into(),
        engine_sources,
        report_root,
        selector: selector.cloned(),
        selected_target,
    })
}

#[cfg(test)]
#[path = "mutation_workspace_metadata_tests.rs"]
mod tests;
