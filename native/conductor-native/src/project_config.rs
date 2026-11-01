//! One bounded native project manifest reader, shared by paths and lifecycle policy.
//! A present forge.toml is authoritative; pyproject.toml is a host-data fallback
//! only when the native manifest is absent. Neither format executes an interpreter.

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use toml::{Table, Value};

pub const CONFIG_LIMIT: usize = 64 * 1024;

#[derive(Debug)]
pub struct ProjectConfig {
    pub path: PathBuf,
    pub namespace: &'static str,
    pub conductor: Table,
}

impl ProjectConfig {
    pub fn label(&self, key: &str) -> String {
        format!("[{}].{key} in {}", self.namespace, self.path.display())
    }
}

fn read(path: &Path) -> Result<Option<Value>, String> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "{}: cannot open configuration: {error}",
                path.display()
            ))
        }
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("{}: cannot inspect configuration: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "{}: configuration must be a regular file",
            path.display()
        ));
    }
    if metadata.len() > CONFIG_LIMIT as u64 {
        return Err(format!("{}: configuration exceeds 64 KiB", path.display()));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(CONFIG_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: cannot read configuration: {error}", path.display()))?;
    if bytes.len() > CONFIG_LIMIT {
        return Err(format!("{}: configuration exceeds 64 KiB", path.display()));
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| format!("{}: invalid TOML configuration: {error}", path.display()))?;
    document(path, text).map(Some)
}

fn document(path: &Path, text: &str) -> Result<Value, String> {
    if text.len() > CONFIG_LIMIT {
        return Err(format!("{}: configuration exceeds 64 KiB", path.display()));
    }
    text.parse::<Value>()
        .map_err(|error| format!("{}: invalid TOML configuration: {error}", path.display()))
}

fn section<'a>(
    parent: Option<&'a Value>,
    key: &str,
    path: &Path,
    label: &str,
) -> Result<Option<&'a Value>, String> {
    match parent.and_then(|value| value.get(key)) {
        None => Ok(None),
        Some(value) if value.is_table() => Ok(Some(value)),
        Some(_) => Err(format!("[{label}] in {} is not a table", path.display())),
    }
}

fn native(path: &Path, payload: Value) -> Result<ProjectConfig, String> {
    if payload.get("schema_version").and_then(Value::as_integer) != Some(1) {
        return Err(format!(
            "{}: schema_version must be integer 1",
            path.display()
        ));
    }
    let conductor = section(Some(&payload), "conductor", path, "conductor")?;
    let table = conductor
        .and_then(Value::as_table)
        .cloned()
        .unwrap_or_default();
    Ok(ProjectConfig {
        path: path.to_owned(),
        namespace: "conductor",
        conductor: table,
    })
}

fn legacy(path: &Path, payload: Value) -> Result<ProjectConfig, String> {
    let tool = section(Some(&payload), "tool", path, "tool")?;
    let conductor = section(tool, "conductor", path, "tool.conductor")?;
    let table = conductor
        .and_then(Value::as_table)
        .cloned()
        .unwrap_or_default();
    Ok(ProjectConfig {
        path: path.to_owned(),
        namespace: "tool.conductor",
        conductor: table,
    })
}

/// Parse one captured snapshot, so provisioning need not reread a changing file.
pub fn parse_native(path: &Path, text: &str) -> Result<ProjectConfig, String> {
    native(path, document(path, text)?)
}

pub fn parse_legacy(path: &Path, text: &str) -> Result<ProjectConfig, String> {
    legacy(path, document(path, text)?)
}

pub fn load(root: &Path) -> Result<ProjectConfig, String> {
    let path = root.join("forge.toml");
    if let Some(payload) = read(&path)? {
        return native(&path, payload);
    }
    let path = root.join("pyproject.toml");
    legacy(
        &path,
        read(&path)?.unwrap_or_else(|| Value::Table(Table::new())),
    )
}

#[cfg(test)]
#[path = "project_config_tests.rs"]
mod tests;
