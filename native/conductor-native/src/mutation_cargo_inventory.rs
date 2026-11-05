//! Pure, bounded Cargo discovery metadata over caller-held enumeration.
//! The caller must independently enumerate and reverify the entire package tree;
//! this API does not authenticate completeness, files, compiler, or runtime data.
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use syn::visit::{self, Visit};

/// An independently observed path. Unrelated regular-file contents may be omitted.
#[derive(Debug)]
pub enum Entry {
    Directory,
    Regular(Option<Vec<u8>>),
    SymlinkOrSpecial,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Target {
    manifest_path: String,
    package: String,
    version: String,
    test: String,
    path: String,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Inventory {
    schema_version: u32,
    declaration_sha256: String,
    target: Target,
    files: BTreeMap<String, String>,
    ranked_nodeids: Vec<String>,
    digest: String,
}
impl Target {
    pub fn manifest_path(&self) -> &str {
        &self.manifest_path
    }
    pub fn package(&self) -> &str {
        &self.package
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn test(&self) -> &str {
        &self.test
    }
    pub fn path(&self) -> &str {
        &self.path
    }
}
impl Inventory {
    pub fn declaration_sha256(&self) -> &str {
        &self.declaration_sha256
    }
    pub fn target(&self) -> &Target {
        &self.target
    }
    pub fn files(&self) -> &BTreeMap<String, String> {
        &self.files
    }
    pub fn to_value(&self) -> Result<serde_json::Value, String> {
        serde_json::to_value(self).map_err(|e| e.to_string())
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn target_path(&self) -> &str {
        &self.target.path
    }
    pub fn ranked_nodeids(&self) -> &[String] {
        &self.ranked_nodeids
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
/// Whether a canonical package-relative path is in a fixed excluded output root.
pub fn excluded(path: &str) -> bool {
    matches!(
        path.split('/').next(),
        Some(".git" | "target" | "mutants.out")
    )
}
fn canonical(path: &str) -> Result<(), String> {
    if path.len() > 4096
        || path.split('/').count() > 128
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || crate::mutation_manifest::safe_relative(path, "inventory path")? != path
    {
        return Err("inventory path must be canonical and bounded".into());
    }
    Ok(())
}
/// Whether a canonical path contributes content to the inventory digest.
pub fn relevant(path: &str) -> bool {
    path.ends_with(".rs") || matches!(path, "Cargo.toml" | "Cargo.lock")
}
pub(crate) fn observe(
    entries: &BTreeMap<String, Entry>,
) -> Result<BTreeMap<String, String>, String> {
    if entries.len() > 16384 {
        return Err("inventory exceeds 16384 entries".into());
    }
    let mut files = BTreeMap::new();
    let mut total = 0usize;
    for (path, entry) in entries {
        canonical(path)?;
        if excluded(path) {
            if !path.contains('/') && !matches!(entry, Entry::Directory) {
                return Err("excluded root must be a directory".into());
            }
            continue;
        }
        if path.split('/').any(|c| {
            c == ".cargo"
                || c == "build.rs"
                || c == "Cargo.toml" && path != "Cargo.toml"
                || c == "Cargo.lock" && path != "Cargo.lock"
        }) {
            return Err(format!("unsupported Cargo input: {path}"));
        }
        if let Some((parent, _)) = path.rsplit_once('/') {
            if !matches!(entries.get(parent), Some(Entry::Directory)) {
                return Err(format!("inventory parent missing: {path}"));
            }
        }
        match entry {
            Entry::SymlinkOrSpecial => return Err(format!("unsupported filesystem entry: {path}")),
            Entry::Directory if relevant(path) => return Err(format!("expected file: {path}")),
            Entry::Regular(bytes) if relevant(path) => {
                let bytes = bytes
                    .as_ref()
                    .ok_or_else(|| format!("missing required bytes: {path}"))?;
                total = total
                    .checked_add(bytes.len())
                    .ok_or("inventory byte overflow")?;
                if bytes.len() > 2 * 1024 * 1024 || total > 32 * 1024 * 1024 {
                    return Err("inventory byte limit exceeded".into());
                }
                files.insert(path.clone(), hash(bytes));
            }
            _ => (),
        }
    }
    if files.len() > 4096 {
        return Err("inventory exceeds 4096 relevant files".into());
    }
    Ok(files)
}
fn bytes<'a>(entries: &'a BTreeMap<String, Entry>, path: &str) -> Result<&'a [u8], String> {
    match entries.get(path) {
        Some(Entry::Regular(Some(bytes))) => Ok(bytes),
        _ => Err(format!("required regular input missing: {path}")),
    }
}
fn table<'a>(
    v: &'a toml::Value,
    name: &str,
) -> Result<&'a toml::map::Map<String, toml::Value>, String> {
    v.as_table()
        .ok_or_else(|| format!("{name} must be a table"))
}
fn keys(
    t: &toml::map::Map<String, toml::Value>,
    allowed: &[&str],
    label: &str,
) -> Result<(), String> {
    if t.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("unsupported {label} fields"));
    }
    Ok(())
}
fn string<'a>(t: &'a toml::map::Map<String, toml::Value>, key: &str) -> Result<&'a str, String> {
    t.get(key)
        .and_then(toml::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing {key}"))
}
fn target(
    entries: &BTreeMap<String, Entry>,
    decl: &crate::mutation_rust_value_contract::Declaration,
) -> Result<Target, String> {
    if decl.target().manifest_path() != "Cargo.toml" {
        return Err("only root Cargo.toml is supported".into());
    }
    let manifest: toml::Value = std::str::from_utf8(bytes(entries, "Cargo.toml")?)
        .map_err(|e| e.to_string())?
        .parse::<toml::Value>()
        .map_err(|e| e.to_string())?;
    let root = table(&manifest, "manifest")?;
    keys(root, &["package", "test"], "manifest")?;
    let package = table(root.get("package").ok_or("missing package")?, "package")?;
    keys(package, &["name", "version", "edition"], "package")?;
    let name = string(package, "name")?;
    let version = string(package, "version")?;
    if name != decl.target().package() {
        return Err("package identity mismatch".into());
    }
    let tests = root
        .get("test")
        .and_then(toml::Value::as_array)
        .ok_or("one explicit test target required")?;
    if tests.len() != 1 {
        return Err("one explicit test target required".into());
    }
    let test = table(&tests[0], "test")?;
    keys(test, &["name", "path", "harness"], "test")?;
    let name_test = string(test, "name")?;
    let path = string(test, "path")?;
    canonical(path)?;
    if excluded(path)
        || !path.starts_with("tests/")
        || !path.ends_with(".rs")
        || name_test != decl.target().test()
        || test
            .get("harness")
            .is_some_and(|v| v.as_bool() != Some(true))
    {
        return Err("unsupported test target".into());
    }
    bytes(entries, path)?;
    bytes(entries, "src/lib.rs")?;
    validate_lock(bytes(entries, "Cargo.lock")?, name, version)?;
    Ok(Target {
        manifest_path: "Cargo.toml".into(),
        package: name.into(),
        version: version.into(),
        test: name_test.into(),
        path: path.into(),
    })
}
fn validate_lock(data: &[u8], name: &str, version: &str) -> Result<(), String> {
    let lock: toml::Value = std::str::from_utf8(data)
        .map_err(|e| e.to_string())?
        .parse::<toml::Value>()
        .map_err(|e| e.to_string())?;
    let root = table(&lock, "lock")?;
    keys(root, &["version", "package"], "lock")?;
    let rows = root
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or("lock package missing")?;
    if rows.len() != 1 {
        return Err("lock must contain only root package".into());
    }
    let row = table(&rows[0], "lock package")?;
    keys(row, &["name", "version"], "lock package")?;
    if string(row, "name")? != name || string(row, "version")? != version {
        return Err("lock package mismatch".into());
    }
    Ok(())
}
struct Syntax<'a> {
    target: bool,
    nested: bool,
    allow_test: bool,
    error: Option<String>,
    nodes: Vec<String>,
    path: &'a str,
}
impl<'ast> Visit<'ast> for Syntax<'_> {
    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        if matches!(expr, syn::Expr::Verbatim(_)) {
            self.error = Some("unparsed Rust expression".into());
            return;
        }
        visit::visit_expr(self, expr);
    }
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if matches!(item, syn::Item::Verbatim(_)) {
            self.error = Some("unparsed Rust item".into());
            return;
        }
        visit::visit_item(self, item);
    }
    fn visit_type(&mut self, ty: &'ast syn::Type) {
        if matches!(ty, syn::Type::Verbatim(_)) {
            self.error = Some("unparsed Rust type".into());
            return;
        }
        visit::visit_type(self, ty);
    }
    fn visit_pat(&mut self, pat: &'ast syn::Pat) {
        if matches!(pat, syn::Pat::Verbatim(_)) {
            self.error = Some("unparsed Rust pattern".into());
            return;
        }
        visit::visit_pat(self, pat);
    }
    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        if !(self.allow_test
            && attr.path().is_ident("test")
            && matches!(attr.meta, syn::Meta::Path(_)))
        {
            self.error = Some("conditional or unsupported Rust attribute".into());
        }
    }
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        let test = item.attrs.iter().any(|attr| attr.path().is_ident("test"));
        if test {
            if !self.target
                || self.nested
                || item.attrs.len() != 1
                || item.sig.asyncness.is_some()
                || !item.sig.inputs.is_empty()
                || !item.sig.generics.params.is_empty()
            {
                self.error = Some("unsupported test function".into());
            }
            self.nodes
                .push(format!("{}::{}", self.path, item.sig.ident));
        }
        let previous_nested = self.nested;
        let previous_allowed = self.allow_test;
        self.nested = true;
        self.allow_test = test;
        visit::visit_item_fn(self, item);
        self.nested = previous_nested;
        self.allow_test = previous_allowed;
    }
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if self.target || item.content.is_some() || !item.attrs.is_empty() {
            self.error = Some("unsupported Rust module form".into());
        }
        visit::visit_item_mod(self, item);
    }
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let allowed = self.target
            && mac.path.segments.len() == 1
            && mac.path.segments.first().is_some_and(|s| {
                matches!(
                    s.ident.to_string().as_str(),
                    "assert" | "assert_eq" | "assert_ne"
                )
            });
        if !allowed {
            self.error = Some("unsupported Rust macro".into());
            return;
        }
        use syn::parse::Parser;
        let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
        let Ok(arguments) = parser.parse2(mac.tokens.clone()) else {
            self.error = Some("unsupported assertion arguments".into());
            return;
        };
        let mut nested = Syntax {
            target: self.target,
            nested: true,
            allow_test: false,
            error: None,
            nodes: vec![],
            path: self.path,
        };
        for argument in &arguments {
            nested.visit_expr(argument);
        }
        if nested.error.is_some() || !nested.nodes.is_empty() {
            self.error = Some("unsupported syntax inside assertion".into());
        }
    }
}
fn inspect(
    entries: &BTreeMap<String, Entry>,
    files: &BTreeMap<String, String>,
    target: &Target,
) -> Result<(Vec<String>, BTreeSet<String>), String> {
    let mut nodes = Vec::new();
    let mut owned = BTreeSet::from(["src/lib.rs".to_owned()]);
    for path in files.keys().filter(|path| path.ends_with(".rs")) {
        let text = std::str::from_utf8(bytes(entries, path)?).map_err(|e| e.to_string())?;
        let file = syn::parse_file(text).map_err(|e| format!("invalid Rust source {path}: {e}"))?;
        let mut visitor = Syntax {
            target: path == &target.path,
            nested: false,
            allow_test: false,
            error: None,
            nodes: vec![],
            path,
        };
        visitor.visit_file(&file);
        if let Some(error) = visitor.error {
            return Err(format!("{path}: {error}"));
        }
        for item in &file.items {
            if let syn::Item::Mod(module) = item {
                let stem = if path == "src/lib.rs" {
                    "src".to_string()
                } else {
                    return Err("nested module ownership is unsupported".into());
                };
                let named = format!("{stem}/{}.rs", module.ident);
                let alternative = format!("{stem}/{}/mod.rs", module.ident);
                if !files.contains_key(&named) || files.contains_key(&alternative) {
                    return Err("module file missing or ambiguous".into());
                }
                owned.insert(named);
            }
        }
        nodes.extend(visitor.nodes);
    }
    nodes.sort();
    Ok((nodes, owned))
}
fn frame(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}
/// Validate a caller-enumerated package snapshot; this is not a filesystem capture.
/// Only the root standard-library package and one ordinary integration target are supported.
pub fn build(
    declaration_bytes: &[u8],
    selected_sources: &[String],
    entries: &BTreeMap<String, Entry>,
) -> Result<Inventory, String> {
    let declaration =
        crate::mutation_rust_value_contract::parse(declaration_bytes, selected_sources)?;
    let files = observe(entries)?;
    for source in selected_sources {
        if !files.contains_key(source) {
            return Err("selected source absent from inventory".into());
        }
    }
    let target = target(entries, &declaration)?;
    let (discovered, owned) = inspect(entries, &files, &target)?;
    if selected_sources
        .iter()
        .any(|source| !owned.contains(source))
    {
        return Err("selected sources must belong to the library target".into());
    }
    let ranked: Vec<String> = declaration
        .ranked_tests()
        .iter()
        .map(|test| test.nodeid().to_owned())
        .collect();
    let expected: BTreeSet<_> = ranked.iter().cloned().collect();
    if expected != discovered.iter().cloned().collect() || expected.is_empty() {
        return Err("ranked tests differ from statically discovered target tests".into());
    }
    let declaration_sha256 = hash(declaration_bytes);
    let mut digest = Sha256::new();
    digest.update(b"forge-cargo-value-inventory-v1\0");
    frame(&mut digest, declaration_bytes);
    for field in [
        &target.manifest_path,
        &target.package,
        &target.version,
        &target.test,
        &target.path,
    ] {
        frame(&mut digest, field.as_bytes());
    }
    for (path, sha) in &files {
        frame(&mut digest, path.as_bytes());
        frame(&mut digest, sha.as_bytes());
    }
    let digest = format!("{:x}", digest.finalize());
    Ok(Inventory {
        schema_version: 1,
        declaration_sha256,
        target,
        files,
        ranked_nodeids: ranked,
        digest,
    })
}
#[cfg(test)]
#[path = "mutation_cargo_inventory_tests.rs"]
mod tests;
