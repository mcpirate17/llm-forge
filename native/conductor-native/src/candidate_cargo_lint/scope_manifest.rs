use crate::candidate_cargo_lint::{metadata, path_parts, read, Budget};
use cap_std::fs::Dir;
use toml::Value;
pub(super) struct Captured {
    pub path: String,
    pub crate_path: String,
    pub package: bool,
    pub workspace: Option<Workspace>,
    pub borrowed: Vec<String>,
    pub workspace_link: Option<String>,
    pub local_dependencies: Vec<String>,
}
pub(super) struct Workspace {
    pub members: Vec<String>,
    pub excludes: Vec<String>,
}
pub(super) fn parse(
    root: &Dir,
    path: &str,
    bytes: &[u8],
    budget: &mut Budget,
) -> Result<Captured, String> {
    if path.rsplit('/').next() != Some("Cargo.toml") {
        return Err("lint inventory must identify Cargo.toml files".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "lint Cargo.toml must be UTF-8")?;
    let value: Value = toml::from_str(text).map_err(|_| "invalid captured Cargo.toml")?;
    let table = value.as_table().ok_or("Cargo.toml must be a table")?;
    let package = match table.get("package") {
        None => false,
        Some(Value::Table(_)) => true,
        _ => return Err("package must be a table".into()),
    };
    let workspace = match table.get("workspace") {
        None => None,
        Some(Value::Table(w)) => Some(Workspace {
            members: strings(w.get("members"))?,
            excludes: strings(w.get("exclude"))?,
        }),
        _ => return Err("workspace must be a table".into()),
    };
    if !package && workspace.is_none() {
        return Err("manifest must declare package or workspace".into());
    }
    let base = path.rsplit_once('/').map(|p| p.0).unwrap_or(".");
    let (workspace_link, local_dependencies) = local_links(root, base, table, budget)?;
    let mut paths = Vec::new();
    if let Some(lib) = table.get("lib") {
        target(lib, &mut paths)?;
    }
    for key in ["bin", "test", "example", "bench"] {
        if let Some(values) = table.get(key) {
            let values = values
                .as_array()
                .ok_or("explicit Cargo targets must be arrays of tables")?;
            for value in values {
                target(value, &mut paths)?;
            }
        }
    }
    if let Some(build) = table
        .get("package")
        .and_then(Value::as_table)
        .and_then(|p| p.get("build"))
    {
        match build {
            Value::String(p) => paths.push(p.clone()),
            Value::Boolean(false) => {}
            _ => return Err("package.build must be string or false".into()),
        }
    }
    if paths.len() > 10_000 {
        return Err("explicit target paths exceed 10000".into());
    }
    let mut borrowed = Vec::new();
    for raw in paths {
        let path = normalize(root, base, &raw, false, budget)?;
        let parts = path_parts(&path, false)?;
        let parent =
            read::parent(root, &path, budget)?.ok_or("explicit target parent is missing")?;
        let meta = metadata(&parent, parts[parts.len() - 1], budget)?
            .ok_or("explicit target file is missing")?;
        if !meta.is_file() {
            return Err("explicit target must be a regular file".into());
        }
        if !within(&path, base) {
            let parent = path.rsplit_once('/').map(|p| p.0).unwrap_or(".").to_owned();
            budget.retain(parent.len())?;
            borrowed.push(parent);
        }
    }
    borrowed.sort();
    borrowed.dedup();
    budget.retain(path.len() + base.len())?;
    Ok(Captured {
        path: path.into(),
        crate_path: base.into(),
        package,
        workspace,
        borrowed,
        workspace_link,
        local_dependencies,
    })
}
fn manifest_path(root: &Dir, base: &str, raw: &str, budget: &mut Budget) -> Result<String, String> {
    let path = normalize(root, base, raw, false, budget)?;
    let path = if path == "." {
        "Cargo.toml".into()
    } else {
        format!("{path}/Cargo.toml")
    };
    path_parts(&path, false)?;
    budget.retain(path.len())?;
    Ok(path)
}
fn local_links(
    root: &Dir,
    base: &str,
    table: &toml::value::Table,
    budget: &mut Budget,
) -> Result<(Option<String>, Vec<String>), String> {
    let workspace_link = table
        .get("package")
        .and_then(Value::as_table)
        .and_then(|p| p.get("workspace"))
        .map(|p| {
            p.as_str()
                .ok_or_else(|| "package.workspace must be a string".to_owned())
                .and_then(|raw| manifest_path(root, base, raw, budget))
        })
        .transpose()?;
    let mut raw = Vec::new();
    dependency_tables(table, &mut raw)?;
    if let Some(workspace) = table.get("workspace").and_then(Value::as_table) {
        dependency_tables(workspace, &mut raw)?;
    }
    if let Some(targets) = table.get("target") {
        for target in targets.as_table().ok_or("target must be a table")?.values() {
            dependency_tables(
                target.as_table().ok_or("target entry must be a table")?,
                &mut raw,
            )?;
        }
    }
    if let Some(patches) = table.get("patch") {
        for registry in patches.as_table().ok_or("patch must be a table")?.values() {
            descriptors(registry, &mut raw)?;
        }
    }
    if let Some(replace) = table.get("replace") {
        descriptors(replace, &mut raw)?;
    }
    let mut paths = raw
        .iter()
        .map(|p| manifest_path(root, base, p, budget))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    paths.dedup();
    Ok((workspace_link, paths))
}
fn dependency_tables(table: &toml::value::Table, out: &mut Vec<String>) -> Result<(), String> {
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(value) = table.get(key) {
            descriptors(value, out)?;
        }
    }
    Ok(())
}
fn descriptors(value: &Value, out: &mut Vec<String>) -> Result<(), String> {
    let entries = value
        .as_table()
        .ok_or("dependency descriptors must be a table")?;
    for descriptor in entries.values() {
        match descriptor {
            Value::String(_) => {}
            Value::Table(table) => {
                if let Some(path) = table.get("path") {
                    let path = path
                        .as_str()
                        .ok_or("local dependency path must be a string")?;
                    if out.len() >= 10_000 {
                        return Err("local dependency paths exceed 10000".into());
                    }
                    out.push(path.into());
                }
            }
            _ => return Err("unsupported dependency descriptor type".into()),
        }
    }
    Ok(())
}
fn target(value: &Value, paths: &mut Vec<String>) -> Result<(), String> {
    let table = value
        .as_table()
        .ok_or("explicit Cargo target must be a table")?;
    if let Some(path) = table.get("path") {
        let path = path
            .as_str()
            .ok_or("explicit Cargo target path must be a string")?;
        if paths.len() >= 10_000 {
            return Err("explicit target paths exceed 10000".into());
        }
        paths.push(path.into());
    }
    Ok(())
}
fn strings(value: Option<&Value>) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let list = value
        .as_array()
        .ok_or("workspace members/exclude must be arrays")?;
    if list.len() > 128 {
        return Err("workspace members/exclude exceed 128".into());
    }
    list.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| "workspace member/exclude must be strings".into())
        })
        .collect()
}
pub(super) fn within(path: &str, parent: &str) -> bool {
    parent == "."
        || path == parent
        || path
            .strip_prefix(parent)
            .is_some_and(|v| v.starts_with('/'))
}
pub(super) fn normalize(
    root: &Dir,
    base: &str,
    raw: &str,
    glob: bool,
    budget: &mut Budget,
) -> Result<String, String> {
    if raw.is_empty() || raw.len() > 4096 || raw.starts_with('/') || raw.contains('\0') {
        return Err("unsafe explicit Cargo path".into());
    }
    let mut parts = path_parts(base, true)?
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut wildcard = false;
    for part in raw.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            if wildcard || parts.is_empty() {
                return Err("Cargo path escapes repository or parent follows glob".into());
            }
            let prefix = format!("{}/Cargo.toml", parts.join("/"));
            if read::parent(root, &prefix, budget)?.is_none() {
                return Err("Cargo parent path prefix is missing".into());
            }
            parts.pop();
            continue;
        }
        if glob && part.contains(['*', '?', '[']) {
            wildcard = true;
        }
        parts.push(part.into());
        if parts.len() > 128 {
            return Err("Cargo path exceeds depth128".into());
        }
    }
    let result = if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    };
    path_parts(&result, true)?;
    Ok(result)
}
