//! Import declaration evaluation over caller-bound source and provider evidence.
//! Provider/stdlib scope is an operator observation, never runtime attestation.
#[path = "candidate_import_source.rs"]
mod source;
use crate::{
    candidate_checks::Finding, candidate_crate_version::normalize_distribution_name as normalize,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
};
use unicode_ident::{is_xid_continue, is_xid_start};

const MAX_FILE: usize = 1024 * 1024;
const MAX_TOTAL: usize = 32 * 1024 * 1024;
const MAX_COUNT: usize = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    path: String,
    text: String,
    manifest: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    path: String,
    text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Module {
    name: String,
    distributions: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Provenance {
    schema_version: u32,
    implementation: String,
    version: String,
    platform: String,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Providers {
    stdlib: Vec<String>,
    modules: Vec<Module>,
    provenance: Provenance,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    files: Vec<File>,
    manifests: Vec<Manifest>,
    strict_trees: Vec<String>,
    providers: Providers,
}

struct Size(usize);
impl Write for Size {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(b.len())
            .filter(|n| *n <= MAX_TOTAL)
            .ok_or_else(|| io::Error::other("size limit"))?;
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn text(s: &str, max: usize) -> Result<(), String> {
    if s.is_empty() || s.len() > max || s.chars().any(char::is_control) {
        return Err("invalid bounded metadata text".into());
    }
    Ok(())
}
fn path(s: &str) -> Result<(), String> {
    text(s, 4096)?;
    if s.contains('\\')
        || s.split('/').count() > 128
        || s.split('/').any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err("invalid relative path".into());
    }
    Ok(())
}
fn module_name(s: &str) -> Result<(), String> {
    text(s, 256)?;
    let mut chars = s.chars();
    if !chars.next().is_some_and(|c| c == '_' || is_xid_start(c))
        || !chars.all(|c| c == '_' || is_xid_continue(c))
    {
        return Err("unsupported top-level module name".into());
    }
    Ok(())
}

fn admit(input: &Input) -> Result<(), String> {
    if input.files.len() > MAX_COUNT
        || input.manifests.len() > MAX_COUNT
        || input.strict_trees.len() > 64
        || input.providers.stdlib.len() > 65_536
        || input.providers.modules.len() > 65_536
    {
        return Err("import evidence count limit exceeded".into());
    }
    let mut seen = BTreeSet::new();
    for f in &input.files {
        path(&f.path)?;
        if f.text.len() > MAX_FILE || !seen.insert(&f.path) {
            return Err("invalid/duplicate source".into());
        }
    }
    seen.clear();
    for m in &input.manifests {
        path(&m.path)?;
        if m.text.len() > MAX_FILE
            || m.path.rsplit('/').next() != Some("pyproject.toml")
            || !seen.insert(&m.path)
        {
            return Err("invalid/duplicate manifest".into());
        }
    }
    seen.clear();
    for t in &input.strict_trees {
        path(t)?;
        if !seen.insert(t) {
            return Err("duplicate strict tree".into());
        }
    }
    let p = &input.providers.provenance;
    if p.schema_version != 1
        || p.sha256.len() != 64
        || !p
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("invalid provider provenance".into());
    }
    for s in [&p.implementation, &p.version, &p.platform] {
        text(s, 256)?;
    }
    let mut stdlib = BTreeSet::new();
    for s in &input.providers.stdlib {
        module_name(s)?;
        if !stdlib.insert(s) {
            return Err("duplicate stdlib module".into());
        }
    }
    let mut modules = BTreeSet::new();
    let mut links = 0usize;
    for m in &input.providers.modules {
        module_name(&m.name)?;
        if !modules.insert(&m.name) || m.distributions.is_empty() || m.distributions.len() > 256 {
            return Err("invalid provider module".into());
        }
        let mut providers = BTreeSet::new();
        for d in &m.distributions {
            links = links.checked_add(1).ok_or("provider work overflow")?;
            if links > 100_000 {
                return Err("provider links exceed limit".into());
            }
            let n = normalize(d)?;
            if !providers.insert(n) {
                return Err("duplicate provider".into());
            }
        }
    }
    Ok(())
}

fn nearest<'a>(file: &str, manifests: &'a BTreeMap<&str, Sets>) -> Option<&'a str> {
    let mut dir = file.rsplit_once('/').map_or("", |(d, _)| d);
    loop {
        let name = if dir.is_empty() {
            "pyproject.toml".into()
        } else {
            format!("{dir}/pyproject.toml")
        };
        if let Some((path, _)) = manifests.get_key_value(name.as_str()) {
            return Some(*path);
        }
        if dir.is_empty() {
            return None;
        }
        dir = dir.rsplit_once('/').map_or("", |(d, _)| d);
    }
}

struct Sets {
    base: BTreeSet<String>,
    declared: BTreeSet<String>,
}
fn specs(value: Option<&toml::Value>, out: &mut BTreeSet<String>) -> Result<(), String> {
    if let Some(items) = value.and_then(toml::Value::as_array) {
        for spec in items.iter().filter_map(toml::Value::as_str) {
            let name = spec
                .trim()
                .split(|c: char| "[<>=!~;@".contains(c) || c.is_whitespace())
                .next()
                .unwrap_or("");
            if !name.is_empty() {
                out.insert(normalize(name)?);
            }
        }
    }
    Ok(())
}
fn manifest(text: &str) -> Result<Sets, String> {
    let value: toml::Value = toml::from_str(text).map_err(|_| "invalid manifest TOML")?;
    let mut base = BTreeSet::new();
    if let Some(name) = value
        .get("project")
        .and_then(|v| v.get("name"))
        .and_then(toml::Value::as_str)
    {
        base.insert(normalize(name)?);
    }
    specs(
        value.get("project").and_then(|v| v.get("dependencies")),
        &mut base,
    )?;
    let mut declared = base.clone();
    for value in [
        value
            .get("project")
            .and_then(|v| v.get("optional-dependencies")),
        value.get("dependency-groups"),
    ] {
        if let Some(groups) = value.and_then(toml::Value::as_table) {
            for group in groups.values() {
                specs(Some(group), &mut declared)?;
            }
        }
    }
    specs(
        value.get("build-system").and_then(|v| v.get("requires")),
        &mut declared,
    )?;
    Ok(Sets { base, declared })
}

fn strict(file: &str, trees: &[String]) -> bool {
    file.rsplit('/').next() != Some("conftest.py")
        && trees.iter().any(|p| {
            file.strip_prefix(p)
                .is_some_and(|rest| rest.starts_with('/'))
        })
}

fn reserve(output: &mut usize, bytes: usize) -> Result<(), String> {
    *output = output
        .checked_add(bytes.checked_mul(6).ok_or("output overflow")?)
        .and_then(|n| n.checked_add(512))
        .filter(|n| *n <= MAX_TOTAL)
        .ok_or("import result budget exceeded")?;
    Ok(())
}
#[derive(Default)]
struct OutputState {
    findings: Vec<Finding>,
    unresolved: Vec<Value>,
    bytes: usize,
    nodes: usize,
}

fn emit(
    output: &mut OutputState,
    rule: &'static str,
    severity: &'static str,
    file: &str,
    module: &str,
    providers: &[String],
    manifest: &str,
) -> Result<(), String> {
    if output.findings.len() >= MAX_COUNT {
        return Err("import findings exceed limit".into());
    }
    let message=match rule {
        "undeclared-dependency" => format!("imports {module}, provided by {}, which {manifest} does not declare",providers.join(", ")),
        "base-dependency-required" => format!("imports {module}, provided by {}, which {manifest} declares only in an extra or a dependency-group; this tree runs in jobs that install [project] dependencies only, so the import fails there",providers.join(", ")),
        _ => format!("RUNTIME_TEST_TOOL_EXEMPTIONS grants {file} a runtime import of {module}, but the file no longer imports it; delete the entry so the grant cannot quietly cover a future import"),
    };
    let names = providers
        .iter()
        .try_fold(0usize, |n, s| n.checked_add(s.len()))
        .ok_or("output overflow")?;
    reserve(
        &mut output.bytes,
        file.len() + module.len() + manifest.len() + message.len() + names,
    )?;
    let evidence = if rule == "stale-exemption" {
        json!({"module":module})
    } else {
        json!({"module":module,"distributions":providers,"manifest":manifest})
    };
    output.findings.push(
        Finding::new("import-declaration", rule, severity, message)
            .path(file)
            .evidence(evidence),
    );
    Ok(())
}

struct Provider {
    names: BTreeSet<String>,
    displays: Vec<String>,
}
struct Environment<'a> {
    stdlib: BTreeSet<&'a str>,
    providers: BTreeMap<&'a str, Provider>,
}

fn evaluate_file(
    file: &File,
    sets: &Sets,
    owner: &str,
    strict_file: bool,
    env: &Environment<'_>,
    output: &mut OutputState,
) -> Result<(), String> {
    let (all, runtime) = source::imports(&file.text, &mut output.nodes)?;
    let mut exemption_used = false;
    let mut optional = Vec::new();
    for module in all {
        if module.starts_with('_') || env.stdlib.contains(module.as_str()) {
            continue;
        }
        let Some(provider) = env.providers.get(module.as_str()) else {
            if output.unresolved.len() >= MAX_COUNT {
                return Err("unresolved imports exceed limit".into());
            }
            reserve(&mut output.bytes, file.path.len() + module.len())?;
            output
                .unresolved
                .push(json!({"path":file.path,"module":module}));
            continue;
        };
        if provider.names.is_disjoint(&sets.declared) {
            emit(
                output,
                "undeclared-dependency",
                "critical",
                &file.path,
                &module,
                &provider.displays,
                owner,
            )?;
        } else if strict_file && runtime.contains(&module) && provider.names.is_disjoint(&sets.base)
        {
            if file.path == "conductor/equivalence_probe.py" && module == "pytest" {
                exemption_used = true;
            } else {
                optional.push((module, provider));
            }
        }
    }
    for (module, provider) in optional {
        emit(
            output,
            "base-dependency-required",
            "critical",
            &file.path,
            &module,
            &provider.displays,
            owner,
        )?;
    }
    if strict_file && file.path == "conductor/equivalence_probe.py" && !exemption_used {
        emit(
            output,
            "stale-exemption",
            "medium",
            &file.path,
            "pytest",
            &[],
            owner,
        )?;
    }
    Ok(())
}

/// Evaluate supplied immutable evidence; caller must bind bytes and provenance.
pub fn evaluate(value: &Value) -> Result<Value, String> {
    serde_json::to_writer(Size(0), value).map_err(|_| "import evidence exceeds 32 MiB")?;
    let input: Input =
        serde_json::from_value(value.clone()).map_err(|_| "invalid import evidence schema")?;
    admit(&input)?;
    let mut manifests = BTreeMap::new();
    for m in &input.manifests {
        manifests.insert(m.path.as_str(), manifest(&m.text)?);
    }
    let mut providers = BTreeMap::new();
    for m in &input.providers.modules {
        let mut displays = m.distributions.clone();
        displays.sort();
        let names = displays
            .iter()
            .map(|s| normalize(s))
            .collect::<Result<BTreeSet<_>, _>>()?;
        providers.insert(m.name.as_str(), Provider { names, displays });
    }
    let env = Environment {
        stdlib: input.providers.stdlib.iter().map(String::as_str).collect(),
        providers,
    };
    let mut output = OutputState::default();
    let mut unmanifested = 0;
    let mut strict_count = 0;
    let mut used_manifests = BTreeSet::new();
    for file in &input.files {
        let owner = nearest(&file.path, &manifests);
        if owner != file.manifest.as_deref() {
            return Err("source manifest does not match nearest supplied manifest".into());
        }
        let Some(owner) = owner else {
            unmanifested += 1;
            continue;
        };
        used_manifests.insert(owner);
        let is_strict = strict(&file.path, &input.strict_trees);
        strict_count += usize::from(is_strict);
        evaluate_file(file, &manifests[owner], owner, is_strict, &env, &mut output)?;
    }
    let result = json!({"findings":output.findings,"files":input.files.iter().map(|f|&f.path).collect::<Vec<_>>(),
        "metrics":{"python_files":input.files.len(),"manifests":used_manifests.len(),"files_without_manifest":unmanifested,
            "base_dependency_files":strict_count,"unresolved_imports":output.unresolved.len(),"python_ast_nodes":output.nodes},"unresolved":output.unresolved,
        "provider_scope":{"stdlib_count":input.providers.stdlib.len(),"module_count":input.providers.modules.len(),
            "provenance":{"schema_version":1,"implementation":input.providers.provenance.implementation,
            "version":input.providers.provenance.version,"platform":input.providers.provenance.platform,"sha256":input.providers.provenance.sha256},
            "complete":false,"runtime_verified":false}});
    serde_json::to_writer(Size(0), &result).map_err(|_| "import result exceeds 32 MiB")?;
    Ok(result)
}

#[cfg(test)]
#[path = "candidate_import_declaration_tests.rs"]
mod tests;
