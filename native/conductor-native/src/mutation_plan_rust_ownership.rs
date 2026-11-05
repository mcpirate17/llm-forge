//! External Cargo targets cannot be reassigned to the package holding their files.
//! The current campaign format requires package-relative sources without `..`;
//! refuse external-only modules until that representation supports them safely.
//! This bounded syntactic walk does not resolve glob-imported macros or expand
//! procedural code; compiler and engine discovery remain separate evidence.

use super::{collect_files, is_skipped_relative, relative_posix, RustCrate};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

struct Package {
    root: PathBuf,
    name: String,
    entries: BTreeSet<PathBuf>,
    external: BTreeSet<PathBuf>,
    all_targets_resolved: bool,
}

pub(super) struct Ownership {
    root: PathBuf,
    packages: BTreeMap<PathBuf, Package>,
    external_files: BTreeMap<PathBuf, BTreeSet<String>>,
}

fn source_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let resolved = path.canonicalize().map_err(|error| {
        format!(
            "cannot resolve Rust target/module {}: {error}",
            path.display()
        )
    })?;
    if !resolved.starts_with(root) || !resolved.is_file() {
        return Err(format!(
            "Rust target/module is outside the repository or not a file: {}",
            path.display()
        ));
    }
    Ok(resolved)
}

fn add_target(package: &mut Package, root: &Path, path: &Path) -> Result<(), String> {
    let path = source_path(root, path)?;
    if !path.starts_with(&package.root) {
        package.external.insert(path.clone());
    }
    package.entries.insert(path);
    Ok(())
}

fn package_targets(root: &Path, manifest: &Path) -> Result<Option<Package>, String> {
    let text = fs::read_to_string(manifest).map_err(|error| error.to_string())?;
    let parsed: toml::Table = text
        .parse()
        .map_err(|error| format!("cannot parse {}: {error}", manifest.display()))?;
    let Some(declaration) = parsed.get("package") else {
        return Ok(None);
    };
    let name = declaration
        .get("name")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("{} has no package name", manifest.display()))?;
    let directory = manifest
        .parent()
        .ok_or("Cargo manifest has no directory")?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let mut package = Package {
        root: directory,
        name: name.into(),
        entries: BTreeSet::new(),
        external: BTreeSet::new(),
        all_targets_resolved: true,
    };
    if let Some(lib) = parsed.get("lib") {
        let path = lib
            .get("path")
            .and_then(toml::Value::as_str)
            .unwrap_or("src/lib.rs");
        let target = package.root.join(path);
        add_target(&mut package, root, &target)?;
    } else if declaration.get("autolib").and_then(toml::Value::as_bool) != Some(false) {
        let target = package.root.join("src/lib.rs");
        if target.is_file() {
            add_target(&mut package, root, &target)?;
        }
    }
    if declaration.get("autobins").and_then(toml::Value::as_bool) != Some(false) {
        let main = package.root.join("src/main.rs");
        if main.is_file() {
            add_target(&mut package, root, &main)?;
        }
        for path in collect_files(&package.root.join("src/bin"), "rs") {
            let relative = path
                .strip_prefix(package.root.join("src/bin"))
                .map_err(|error| error.to_string())?;
            if relative.components().count() == 1
                || (relative.components().count() == 2
                    && path.file_name().is_some_and(|name| name == "main.rs"))
            {
                add_target(&mut package, root, &path)?;
            }
        }
    }
    for kind in ["bin", "example", "test", "bench"] {
        for target in parsed
            .get(kind)
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(path) = target.get("path").and_then(toml::Value::as_str) {
                let path = package.root.join(path);
                add_target(&mut package, root, &path)?;
            } else {
                // A pathless declaration is not evidence of an external target.
                // Preserve ordinary source discovery, but never skip a package
                // on the strength of only its explicitly external declarations.
                package.all_targets_resolved = false;
            }
        }
    }
    Ok(Some(package))
}

impl Ownership {
    pub(super) fn require_contained_sources(
        &self,
        scope: Option<&BTreeSet<String>>,
        subjects: &[RustCrate],
    ) -> Result<(), String> {
        for (file, owners) in &self.external_files {
            let relative = relative_posix(&self.root, file);
            if scope.is_some_and(|scope| !scope.contains(&relative)) {
                continue;
            }
            if !subjects
                .iter()
                .any(|subject| subject.files.contains(&relative))
            {
                return Err(format!("Rust source {relative} belongs to external Cargo target package {}; external package-relative mutation sources are unsupported", owners.iter().cloned().collect::<Vec<_>>().join(", ")));
            }
        }
        Ok(())
    }

    pub(super) fn discover(root: &Path, manifests: &[PathBuf]) -> Result<Self, String> {
        let root = root.canonicalize().map_err(|error| error.to_string())?;
        let mut result = Self {
            root,
            packages: BTreeMap::new(),
            external_files: BTreeMap::new(),
        };
        for manifest in manifests {
            if is_skipped_relative(&relative_posix(&result.root, manifest)) {
                continue;
            }
            let Some(package) = package_targets(&result.root, manifest)? else {
                continue;
            };
            for entry in &package.external {
                for file in module_files(&result.root, entry, false)? {
                    result
                        .external_files
                        .entry(file)
                        .or_default()
                        .insert(package.name.clone());
                }
            }
            result.packages.insert(manifest.clone(), package);
        }
        Ok(result)
    }

    pub(super) fn has_only_external_targets(&self, manifest: &Path) -> bool {
        self.packages.get(manifest).is_some_and(|package| {
            package.all_targets_resolved
                && !package.entries.is_empty()
                && package.entries == package.external
        })
    }

    pub(super) fn unsupported_sources(
        &self,
        manifest: &Path,
        files: &[String],
    ) -> Result<BTreeMap<String, String>, String> {
        let mut candidates = BTreeMap::new();
        for file in files {
            let canonical = source_path(&self.root, &self.root.join(file))?;
            if let Some(owners) = self.external_files.get(&canonical) {
                candidates.insert(
                    file.clone(),
                    owners.iter().cloned().collect::<Vec<_>>().join(", "),
                );
            }
        }
        if candidates.is_empty() {
            return Ok(candidates);
        }
        let package = self
            .packages
            .get(manifest)
            .ok_or("Rust package target inventory unavailable")?;
        let mut local = BTreeSet::new();
        for entry in package.entries.difference(&package.external) {
            local.extend(module_files(&self.root, entry, true)?);
        }
        candidates.retain(|file, _| {
            !self
                .root
                .join(file)
                .canonicalize()
                .is_ok_and(|path| local.contains(&path))
        });
        Ok(candidates)
    }
}

pub(super) fn require_supported(
    subject: &RustCrate,
    sources: Option<&[String]>,
) -> Result<(), String> {
    for (path, owner) in &subject.unsupported_sources {
        if sources.is_none_or(|sources| sources.contains(path)) {
            return Err(format!("Rust source {path} belongs to external Cargo target package {owner}, not {}; external package-relative mutation sources are unsupported", subject.package));
        }
    }
    Ok(())
}

const MAX_MODULE_STATES: usize = 4096;
const MAX_MODULE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_GRAPH_BYTES: usize = 64 * 1024 * 1024;
type PendingModules = Vec<(PathBuf, Option<PathBuf>)>;

fn read_module(path: &Path, total: &mut usize) -> Result<syn::File, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_MODULE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    *total = total
        .checked_add(bytes.len())
        .ok_or("Rust module byte count overflow")?;
    if bytes.len() as u64 > MAX_MODULE_BYTES || *total > MAX_GRAPH_BYTES {
        return Err("Rust module graph exceeds 2 MiB per file or 64 MiB total".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|error| error.to_string())?;
    syn::parse_file(text).map_err(|error| error.to_string())
}

fn module_files(
    root: &Path,
    entry: &Path,
    unconditional: bool,
) -> Result<BTreeSet<PathBuf>, String> {
    let mut files = BTreeSet::new();
    let mut states = BTreeSet::new();
    let mut total = 0;
    let mut pending = vec![(entry.to_path_buf(), None)];
    while let Some((named, directory)) = pending.pop() {
        let path = source_path(root, &named)?;
        let physical = path.parent().ok_or("Rust module has no parent")?;
        let directory = directory.unwrap_or_else(|| physical.to_path_buf());
        if !states.insert((path.clone(), directory.clone())) {
            continue;
        }
        if states.len() > MAX_MODULE_STATES || directory.as_os_str().as_encoded_bytes().len() > 4096
        {
            return Err("Rust target module graph exceeds state or path bounds".into());
        }
        let syntax = read_module(&path, &mut total).map_err(|error| {
            format!(
                "cannot parse Rust target module {}: {error}",
                path.display()
            )
        })?;
        let active = ownership_attributes(&syntax.attrs, unconditional).map_err(|error| {
            format!(
                "cannot inspect Rust target module {}: {error}",
                path.display()
            )
        })?;
        if !active {
            continue;
        }
        files.insert(path.clone());
        module_items(
            &syntax.items,
            &directory,
            physical,
            physical,
            unconditional,
            0,
            &mut pending,
        )
        .map_err(|error| {
            format!(
                "cannot inspect Rust target module {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(files)
}

/// Unknown conditions cannot prove local ownership. Lint-only attributes are
/// inert; conditional paths and procedural attributes need compiler expansion.
fn ownership_attributes(attrs: &[syn::Attribute], unconditional: bool) -> Result<bool, String> {
    for attribute in attrs {
        let name = attribute.path();
        if name.is_ident("cfg_attr") {
            if !inert_lint_cfg_attr(attribute) {
                return Err(
                    "conditional Rust module attributes require expanded Cargo target ownership"
                        .into(),
                );
            }
        } else if ![
            "cfg", "path", "allow", "warn", "deny", "forbid", "expect", "doc",
        ]
        .iter()
        .any(|allowed| name.is_ident(allowed))
        {
            return Err("Rust module attributes require expanded Cargo target ownership".into());
        }
    }
    Ok(!unconditional
        || !attrs.iter().any(|attribute| {
            attribute.path().is_ident("cfg")
                && !matches!(&attribute.meta,
            syn::Meta::List(list) if list.tokens.to_string() == "test")
        }))
}

fn item_macro(
    item: &syn::ItemMacro,
    physical: &Path,
    pending: &mut PendingModules,
) -> Result<(), String> {
    if item.mac.path.is_ident("macro_rules") {
        if item.ident.as_ref().is_some_and(|name| name == "include") {
            return Err("shadowed include! requires expanded Cargo target ownership".into());
        }
        return Ok(());
    }
    if !item.mac.path.is_ident("include") {
        return Err("item-generating Rust macros require expanded Cargo target ownership".into());
    }
    let path = syn::parse2::<syn::LitStr>(item.mac.tokens.clone())
        .map_err(|_| "Rust target ownership requires literal include! paths")?;
    // rustc resolves modules inside included text relative to that file, rather
    // than the caller's inline module directory.
    pending.push((physical.join(path.value()), None));
    Ok(())
}

/// Reject explicit bindings that can shadow the built-in include! macro.
/// Glob imports and procedural expansion remain outside this syntactic walk.
fn imports_include(tree: &syn::UseTree) -> bool {
    match tree {
        syn::UseTree::Path(path) => imports_include(&path.tree),
        syn::UseTree::Name(name) => name.ident == "include",
        syn::UseTree::Rename(rename) => rename.rename == "include",
        syn::UseTree::Group(group) => group.items.iter().any(imports_include),
        syn::UseTree::Glob(_) => false,
    }
}

fn module_items(
    items: &[syn::Item],
    directory: &Path,
    attribute_directory: &Path,
    physical_directory: &Path,
    unconditional: bool,
    depth: usize,
    pending: &mut PendingModules,
) -> Result<(), String> {
    if depth > 128 {
        return Err("Rust inline module nesting exceeds 128 levels".into());
    }
    for item in items {
        if let syn::Item::Use(item) = item {
            if imports_include(&item.tree) {
                return Err("imported include! requires expanded Cargo target ownership".into());
            }
        }
        if let syn::Item::Macro(item) = item {
            if ownership_attributes(&item.attrs, unconditional)? {
                item_macro(item, physical_directory, pending)?;
            }
            continue;
        }
        let syn::Item::Mod(module) = item else {
            continue;
        };
        if !ownership_attributes(&module.attrs, unconditional)? {
            continue;
        }
        let name = module.ident.to_string();
        if let Some((_, items)) = &module.content {
            if module
                .attrs
                .iter()
                .any(|attribute| attribute.path().is_ident("path"))
            {
                return Err(
                    "inline Rust module path attributes are unsupported for target ownership"
                        .into(),
                );
            }
            let inline_directory = directory.join(&name);
            module_items(
                items,
                &inline_directory,
                &inline_directory,
                physical_directory,
                unconditional,
                depth + 1,
                pending,
            )?;
            continue;
        }
        let path = module_path(module, directory, attribute_directory)?;
        let explicit = module
            .attrs
            .iter()
            .any(|attribute| attribute.path().is_ident("path"));
        let next = if explicit || path.file_name().is_some_and(|name| name == "mod.rs") {
            None
        } else {
            Some(path.with_extension(""))
        };
        pending.push((path, next));
    }
    Ok(())
}

/// Lint levels cannot enable a module or change its path, irrespective of the
/// condition's value. Other conditional attributes require Cargo expansion.
fn inert_lint_cfg_attr(attribute: &syn::Attribute) -> bool {
    use syn::parse::Parser;
    let syn::Meta::List(list) = &attribute.meta else {
        return false;
    };
    let Ok(arguments) = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated
        .parse2(list.tokens.clone())
    else {
        return false;
    };
    arguments.len() >= 2
        && arguments.iter().skip(1).all(|argument| {
            matches!(argument, syn::Meta::List(list) if
                ["allow", "warn", "deny", "forbid", "expect"]
                    .iter().any(|name| list.path.is_ident(name)))
        })
}

fn module_path(
    module: &syn::ItemMod,
    directory: &Path,
    attribute_directory: &Path,
) -> Result<PathBuf, String> {
    let name = module.ident.to_string();
    let attributes: Vec<_> = module
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("path"))
        .collect();
    let explicit = attributes
        .first()
        .and_then(|attribute| match &attribute.meta {
            syn::Meta::NameValue(value) => match &value.value {
                syn::Expr::Lit(value) => match &value.lit {
                    syn::Lit::Str(value) => Some(value.value()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        });
    if attributes.len() > 1 || (!attributes.is_empty() && explicit.is_none()) {
        return Err("Rust module target ownership requires one literal path attribute".into());
    }
    let path = if let Some(path) = explicit {
        attribute_directory.join(path)
    } else {
        let flat = directory.join(format!("{name}.rs"));
        let nested = directory.join(&name).join("mod.rs");
        if flat.is_file() && nested.is_file() {
            return Err(format!("ambiguous Rust module {name}"));
        }
        if flat.is_file() {
            flat
        } else {
            nested
        }
    };
    Ok(path)
}
