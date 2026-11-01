//! Pure crate version discipline over caller-bound candidate/base evidence.
//! Installed entries are explicit metadata observations, never runtime proof.
use crate::{candidate_checks::Finding, candidate_rust_artifact::production_source};
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
};
const MAX_RECORDS: usize = 4096;
const MAX_TEXT: usize = 1024 * 1024;
const MAX_TOTAL: usize = 32 * 1024 * 1024;
const MAX_PATH: usize = 4096;
const MAX_NAME: usize = 256;
struct File<'a> {
    path: &'a str,
    current: Option<&'a str>,
    base: Option<&'a str>,
}
struct Payload<'a> {
    manifests: Vec<File<'a>>,
    sources: Vec<File<'a>>,
    installed: BTreeMap<String, &'a str>,
}
struct Budget(usize);
impl Budget {
    fn text<'a>(&mut self, value: &'a Value, maximum: usize) -> Result<&'a str, String> {
        let text = value
            .as_str()
            .ok_or("crate-version evidence field must be a string")?;
        if text.len() > maximum {
            return Err("crate-version individual string exceeds byte limit".into());
        }
        self.0 = self
            .0
            .checked_add(text.len())
            .filter(|size| *size <= MAX_TOTAL)
            .ok_or("crate-version evidence exceeds 32 MiB")?;
        Ok(text)
    }
    fn optional<'a>(&mut self, value: &'a Value) -> Result<Option<&'a str>, String> {
        if value.is_null() {
            Ok(None)
        } else {
            self.text(value, MAX_TEXT).map(Some)
        }
    }
}
fn object<'a>(value: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>, String> {
    let object = value
        .as_object()
        .ok_or("crate-version evidence must contain objects")?;
    if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key)) {
        return Err("crate-version evidence has missing or unknown fields".into());
    }
    Ok(object)
}
fn array(value: &Value) -> Result<&[Value], String> {
    let values = value
        .as_array()
        .ok_or("crate-version evidence field must be an array")?;
    if values.len() > MAX_RECORDS {
        return Err("crate-version evidence record count exceeds 4096".into());
    }
    Ok(values)
}
fn path(text: &str) -> Result<(), String> {
    let parts: Vec<_> = text.split('/').collect();
    if text.is_empty()
        || text.len() > MAX_PATH
        || parts.len() > 128
        || parts
            .iter()
            .any(|part| part.is_empty() || matches!(*part, "." | ".."))
        || text.chars().any(char::is_control)
    {
        return Err("crate-version paths must be bounded normalized relative paths".into());
    }
    Ok(())
}
fn files<'a>(
    value: &'a Value,
    budget: &mut Budget,
    manifests: bool,
) -> Result<Vec<File<'a>>, String> {
    let values = array(value)?;
    let mut seen = BTreeSet::new();
    let mut files = Vec::with_capacity(values.len());
    for value in values {
        let value = object(value, &["path", "current", "base"])?;
        let name = budget.text(&value["path"], MAX_PATH)?;
        path(name)?;
        if !seen.insert(name) {
            return Err("duplicate crate-version file path".into());
        }
        if manifests && name.rsplit('/').next() != Some("Cargo.toml") {
            return Err("crate-version manifest path must name Cargo.toml".into());
        }
        let current = budget.optional(&value["current"])?;
        let base = budget.optional(&value["base"])?;
        if !manifests && current.is_none() && base.is_none() {
            return Err("crate-version source has neither endpoint".into());
        }
        files.push(File {
            path: name,
            current,
            base,
        });
    }
    Ok(files)
}
/// Python distribution-name normalization (PEP503), with strict bounded ASCII
/// spelling. Only metadata names are compared; this does not locate a package.
pub fn normalize_distribution_name(name: &str) -> Result<String, String> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_NAME
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
        || bytes
            .iter()
            .any(|byte| !byte.is_ascii_alphanumeric() && !b"._-".contains(byte))
    {
        return Err("invalid bounded ASCII distribution name".into());
    }
    let mut output = String::with_capacity(name.len());
    let mut separator = false;
    for byte in bytes {
        if b"._-".contains(byte) {
            if !separator {
                output.push('-');
            }
            separator = true;
        } else {
            output.push(byte.to_ascii_lowercase() as char);
            separator = false;
        }
    }
    Ok(output)
}
fn version(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_NAME && !text.chars().any(char::is_control)
}
impl<'a> Payload<'a> {
    fn parse(value: &'a Value) -> Result<Self, String> {
        let value = object(value, &["manifests", "sources", "installed"])?;
        // Admit every collection count before decoding any large source string.
        for key in ["manifests", "sources", "installed"] {
            array(&value[key])?;
        }
        let mut budget = Budget(0);
        let manifests = files(&value["manifests"], &mut budget, true)?;
        let sources = files(&value["sources"], &mut budget, false)?;
        let mut installed = BTreeMap::new();
        for entry in array(&value["installed"])? {
            let entry = object(entry, &["name", "version"])?;
            let name = normalize_distribution_name(budget.text(&entry["name"], MAX_NAME)?)?;
            let found = budget.text(&entry["version"], MAX_NAME)?;
            if !version(found) {
                return Err("invalid installed metadata version".into());
            }
            if installed.insert(name, found).is_some() {
                return Err("duplicate normalized installed distribution".into());
            }
        }
        Ok(Self {
            manifests,
            sources,
            installed,
        })
    }
}
#[derive(Default)]
struct Owner<'a> {
    manifest: Option<usize>,
    children: BTreeMap<&'a str, Owner<'a>>,
}
impl<'a> Owner<'a> {
    fn index(files: &[File<'a>]) -> Self {
        let mut root = Self::default();
        for (index, file) in files.iter().enumerate() {
            let directory = directory(file.path);
            let mut node = &mut root;
            for part in directory.split('/').filter(|part| !part.is_empty()) {
                node = node.children.entry(part).or_default();
            }
            node.manifest = Some(index);
        }
        root
    }
    fn owning(&self, source: &str) -> Option<usize> {
        let mut node = self;
        let mut owner = self.manifest;
        for part in directory(source).split('/').filter(|part| !part.is_empty()) {
            let Some(child) = node.children.get(part) else {
                break;
            };
            node = child;
            if node.manifest.is_some() {
                owner = node.manifest;
            }
        }
        owner
    }
}
fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}
fn reaches_artifact(file: &File<'_>, manifest: &File<'_>) -> bool {
    let suffix = file
        .path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .filter(|(stem, _)| !stem.is_empty())
        .map(|(_, suffix)| suffix);
    let Some(suffix) = suffix else {
        return false;
    };
    if !["rs", "c", "cc", "cpp", "cu", "h", "hpp"]
        .iter()
        .any(|allowed| suffix.eq_ignore_ascii_case(allowed))
    {
        return false;
    }
    let root = directory(manifest.path);
    let relative = if root.is_empty() {
        file.path
    } else {
        &file.path[root.len() + 1..]
    };
    if matches!(
        relative.split('/').next(),
        Some("tests" | "benches" | "examples")
    ) {
        return false;
    }
    match (file.current, file.base) {
        (Some(current), Some(base)) if suffix.eq_ignore_ascii_case("rs") => {
            match (production_source(current), production_source(base)) {
                (Ok(current), Ok(base)) => current != base,
                _ => true,
            }
        }
        (Some(current), Some(base)) => current != base,
        _ => true,
    }
}
struct Package {
    name: Option<String>,
    version: String,
}
fn package(text: &str, with_name: bool) -> Result<Package, &'static str> {
    let value: toml::Value = toml::from_str(text).map_err(|_| "manifest TOML is malformed")?;
    let mut pending = vec![(&value, 0usize)];
    let mut nodes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if nodes > 65_536 || depth > 64 {
            return Err("manifest TOML exceeds structural limits");
        }
        match value {
            toml::Value::Array(values) => {
                pending.extend(values.iter().map(|value| (value, depth + 1)))
            }
            toml::Value::Table(values) => {
                pending.extend(values.values().map(|value| (value, depth + 1)))
            }
            _ => {}
        }
    }
    let package = value
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or("manifest has no package table")?;
    let current = package
        .get("version")
        .and_then(toml::Value::as_str)
        .filter(|text| version(text))
        .ok_or("package version is not a bounded nonempty literal string")?;
    let name = if with_name {
        let name = package
            .get("name")
            .and_then(toml::Value::as_str)
            .ok_or("package name is not a literal string")?;
        Some(normalize_distribution_name(name).map_err(|_| "package name is invalid")?)
    } else {
        None
    };
    Ok(Package {
        name,
        version: current.to_owned(),
    })
}
fn push(
    rows: &mut Vec<Finding>,
    rule: &'static str,
    severity: &'static str,
    path: &str,
    message: String,
) -> Result<(), String> {
    if rows.len() == MAX_RECORDS {
        return Err("crate-version findings exceed 4096; evaluation incomplete".into());
    }
    rows.push(Finding::new("crate-version", rule, severity, message).path(path));
    Ok(())
}
fn evaluate_crate(
    file: &File<'_>,
    installed: &BTreeMap<String, &str>,
    rows: &mut Vec<Finding>,
) -> Result<(), String> {
    let Some(text) = file.current else {
        return push(rows, "unreadable-manifest", "critical", file.path,
            "Captured crate sources changed but current manifest bytes are unavailable; a version change cannot be established".into());
    };
    let current = match package(text, true) {
        Ok(package) => package,
        Err(reason) => return push(rows, "unresolvable-crate-version", "high", file.path,
            format!("Captured crate sources changed but {reason}; a literal version change cannot be established")),
    };
    if let Some(base) = file.base {
        match package(base, false) {
            Ok(base) if base.version == current.version => push(rows, "source-changed-without-version-bump", "critical", file.path,
                format!("Captured production source changed while the manifest version remained {}; update the version for the changed artifact", current.version))?,
            Ok(_) => {}
            Err(reason) => push(rows, "unresolvable-crate-version", "high", file.path,
                format!("Base {reason}; the captured version change cannot be established"))?,
        }
    }
    if let Some(name) = current.name {
        if let Some(found) = installed
            .get(&name)
            .filter(|found| **found != current.version)
        {
            push(rows, "installed-version-drift", "high", file.path,
                format!("Candidate declares {} but supplied installed metadata for {name} records {found}; this comparison does not establish runtime artifact identity", current.version))?;
        }
    }
    Ok(())
}
/// Evaluate only supplied evidence. No filesystem, process, environment or
/// interpreter access; no confidence that installed metadata is an active build.
pub fn evaluate(payload: &Value) -> Result<Value, String> {
    let payload = Payload::parse(payload)?;
    let owners = Owner::index(&payload.manifests);
    let mut touched = BTreeSet::new();
    for source in &payload.sources {
        if let Some(index) = owners.owning(source.path) {
            if reaches_artifact(source, &payload.manifests[index]) {
                touched.insert(index);
            }
        }
    }
    let mut touched: Vec<_> = touched
        .into_iter()
        .map(|index| &payload.manifests[index])
        .collect();
    touched.sort_by_key(|file| file.path);
    let mut rows = Vec::new();
    for file in &touched {
        evaluate_crate(file, &payload.installed, &mut rows)?;
    }
    let mut findings = serde_json::to_value(rows).map_err(|_| "encode crate-version findings")?;
    findings
        .as_array_mut()
        .ok_or("invalid internal finding array")?
        .sort_by(|left, right| {
            (left["path"].as_str(), left["rule_id"].as_str())
                .cmp(&(right["path"].as_str(), right["rule_id"].as_str()))
        });
    let result = json!({"findings":findings,"files":touched.iter().map(|file| file.path).collect::<Vec<_>>(),
        "metrics":{"crates_indexed":payload.manifests.len(),"crates_touched":touched.len()}});
    serde_json::to_writer(Size(0), &result)
        .map_err(|_| "crate-version encoded output exceeds 32 MiB")?;
    Ok(result)
}
struct Size(usize);
impl Write for Size {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|size| *size <= MAX_TOTAL)
            .ok_or_else(|| io::Error::other("crate-version output limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[cfg(test)]
#[path = "candidate_crate_version_tests.rs"]
mod tests;
