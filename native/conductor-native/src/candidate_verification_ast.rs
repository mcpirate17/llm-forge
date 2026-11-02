//! Collectible Python definitions with decorator-inclusive source identity.

#[path = "candidate_verification_rust_preflight.rs"]
mod rust_preflight;

use ruff_python_ast as ast;
use ruff_text_size::Ranged;
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};
use syn::spanned::Spanned;

fn parse_rust(source: &str) -> Result<syn::File, String> {
    rust_preflight::admit(source)?;
    syn::parse_file(source).map_err(|error| error.to_string())
}

/// Extract actual native tests and their source identity, using language parsers.
/// Rust labels are the names printed by libtest; C labels follow the existing
/// ctest adapter's named-function contract. Unknown frameworks fail explicitly.
pub(super) fn native_test_definitions(
    source: &str,
    path: &str,
    root: Option<&Path>,
) -> Result<Value, String> {
    if path.ends_with(".rs") {
        let file = parse_rust(source)
            .map_err(|error| format!("cannot parse native tests in {path}: {error}"))?;
        let mut definitions = Map::new();
        rust_tests(&file.items, source, "", false, &mut definitions)?;
        if definitions.is_empty() {
            return Ok(Value::Object(definitions));
        }
        let prefix = match root {
            Some(root) => rust_prefix(root, path)?,
            None if path.ends_with("/src/lib.rs")
                || path.ends_with("/src/main.rs")
                || path == "src/lib.rs"
                || path == "src/main.rs" =>
            {
                String::new()
            }
            None => {
                return Err(format!(
                "native Rust module identity unavailable for {path}; repository root is required"
            ))
            }
        };
        definitions.clear();
        rust_tests(&file.items, source, &prefix, false, &mut definitions)?;
        return Ok(Value::Object(definitions));
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .map_err(|error| format!("cannot initialize C/C++ parser: {error}"))?;
    let tree = parser
        .parse(source, None)
        .ok_or("C/C++ parser returned no tree")?;
    if tree.root_node().has_error() {
        return Err(format!("cannot parse native tests in {path}; macro frameworks require an explicit supported test identity"));
    }
    let mut definitions = Map::new();
    c_tests(tree.root_node(), source, &mut definitions);
    Ok(Value::Object(definitions))
}

fn test_cfg(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && matches!(&attr.meta,
        syn::Meta::List(list) if list.tokens.to_string() == "test")
    })
}

fn rust_tests(
    items: &[syn::Item],
    source: &str,
    prefix: &str,
    in_tests: bool,
    out: &mut Map<String, Value>,
) -> Result<(), String> {
    for item in items {
        match item {
            syn::Item::Mod(module) => {
                if let Some((_, items)) = &module.content {
                    rust_tests(items, source, &format!("{prefix}{}::", module.ident), in_tests || test_cfg(&module.attrs), out)?;
                } else if test_cfg(&module.attrs) {
                    out.insert(
                        format!("__unsupported_test_module__{prefix}{}", module.ident),
                        Value::String(source[module.span().byte_range()].into()),
                    );
                }
            }
            syn::Item::Fn(function)
                if function.attrs.iter().any(|attribute| {
                    attribute
                        .path()
                        .segments
                        .last()
                        .is_some_and(|s| s.ident == "test")
                }) =>
            {
                out.insert(
                    format!("{prefix}{}", function.sig.ident),
                    Value::String(source[function.span().byte_range()].into()),
                );
            }
            syn::Item::Fn(function) if function.attrs.iter().any(|attribute|
                attribute.path().segments.last().is_some_and(|s| ["rstest","test_case","quickcheck"].iter().any(|name| s.ident == name)) ||
                (attribute.path().is_ident("cfg_attr") && matches!(&attribute.meta, syn::Meta::List(list) if list.tokens.to_string().split(|c:char| !c.is_alphanumeric() && c != '_').any(|s| s == "test")))) => {
                return Err("native Rust test attribute identity unsupported; macro expansion is required".into());
            }
            syn::Item::Macro(item) if !item.mac.path.is_ident("macro_rules") && (in_tests || test_cfg(&item.attrs) || item.mac.path.segments.last().is_some_and(|s| ["proptest", "quickcheck"].iter().any(|name| s.ident == name))) => return Err("native Rust test-generating macro identity unsupported; macro expansion is required".into()),
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn rust_has_tests(source: &str) -> Result<bool, String> {
    let file = parse_rust(source)?;
    struct TestSurfaces(bool);
    impl<'ast> syn::visit::Visit<'ast> for TestSurfaces {
        fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
            self.0 |= test_cfg(&node.attrs);
            syn::visit::visit_item_mod(self, node);
        }
        fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
            self.0 |= node.attrs.iter().any(|attr| {
                attr.path().segments.last().is_some_and(|s| {
                    ["test", "rstest", "test_case", "quickcheck"]
                        .iter()
                        .any(|name| s.ident == name)
                }) || (attr.path().is_ident("cfg_attr") && matches!(&attr.meta, syn::Meta::List(list) if list.tokens.to_string().split(|c:char| !c.is_alphanumeric() && c != '_').any(|s| s == "test")))
            });
            syn::visit::visit_item_fn(self, node);
        }
        fn visit_item_macro(&mut self, node: &'ast syn::ItemMacro) {
            self.0 |= node.mac.path.segments.last().is_some_and(|s| {
                ["proptest", "quickcheck"]
                    .iter()
                    .any(|name| s.ident == name)
            });
            syn::visit::visit_item_macro(self, node);
        }
    }
    let mut surfaces = TestSurfaces(false);
    syn::visit::Visit::visit_file(&mut surfaces, &file);
    Ok(surfaces.0)
}

/// Return a module prefix only when the source is reachable from this exact
/// Cargo target. The caller separately binds the target to its invocation.
pub(crate) fn target_prefix(entry: &Path, target: &Path) -> Result<String, String> {
    target_prefix_with(&AmbientReader, entry, target)
}

fn target_prefix_with(
    reader: &impl RustReader,
    entry: &Path,
    target: &Path,
) -> Result<String, String> {
    let target = reader.canonicalize(target)?;
    let mut prefixes = BTreeSet::new();
    let mut seen = HashSet::new();
    visit_file(
        reader,
        entry,
        entry.parent().ok_or("target has no parent")?,
        "",
        &target,
        &mut seen,
        &mut prefixes,
    )?;
    if prefixes.len() != 1 {
        return Err(format!(
            "source has no unique identity in Cargo target: {prefixes:?}"
        ));
    }
    Ok(prefixes.into_iter().next().unwrap())
}

pub(crate) fn target_test_labels(
    entry: &Path,
    target: &Path,
) -> Result<Map<String, Value>, String> {
    let prefix = target_prefix(entry, target)?;
    let source = fs::read_to_string(target).map_err(|e| e.to_string())?;
    let file = parse_rust(&source)?;
    let mut labels = Map::new();
    rust_tests(&file.items, &source, &prefix, false, &mut labels)?;
    Ok(labels)
}

pub(super) fn native_source_role(root: &Path, path: &str) -> Result<Value, String> {
    source_role_value(root, path, &mut HashMap::new())
}

pub(super) fn native_source_roles(root: &Path, paths: &[String]) -> Result<Value, String> {
    let mut cache = HashMap::new();
    let mut roles = Map::new();
    for path in paths {
        roles.insert(path.clone(), source_role_value(root, path, &mut cache)?);
    }
    Ok(Value::Object(roles))
}

type RoleCache = HashMap<PathBuf, Rc<syn::File>>;

fn source_role_value(root: &Path, path: &str, cache: &mut RoleCache) -> Result<Value, String> {
    match source_role(root, path, cache) {
        Ok(Some(false)) => Ok(
            serde_json::json!({"role":"production","reason":"reachable from a production Cargo target outside test-only cfg"}),
        ),
        Ok(Some(true)) => Ok(
            serde_json::json!({"role":"test-only","reason":"reachable only through test targets or cfg(test) modules"}),
        ),
        Ok(None) => Ok(
            serde_json::json!({"role":"unknown","reason":"source is not reachable from a supported Cargo target"}),
        ),
        Err(error) => Ok(serde_json::json!({"role":"unknown","reason":error})),
    }
}

fn source_role(root: &Path, path: &str, cache: &mut RoleCache) -> Result<Option<bool>, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let target = root.join(path).canonicalize().map_err(|e| e.to_string())?;
    if !target.starts_with(&root) {
        return Err("source escapes repository".into());
    }
    let crate_root = target
        .ancestors()
        .skip(1)
        .find(|d| d.join("Cargo.toml").is_file())
        .ok_or("Cargo manifest unavailable")?;
    let manifest: toml::Value = toml::from_str(
        &fs::read_to_string(crate_root.join("Cargo.toml")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("{e}"))?;
    let mut roots = vec![
        (crate_root.join("src/lib.rs"), false),
        (crate_root.join("src/main.rs"), false),
    ];
    if let Some(path) = manifest
        .get("lib")
        .and_then(|v| v.get("path"))
        .and_then(toml::Value::as_str)
    {
        roots.push((crate_root.join(path), false));
    }
    for (kind, test_only) in [("bin", false), ("test", true), ("bench", true)] {
        if let Some(items) = manifest.get(kind).and_then(toml::Value::as_array) {
            for item in items {
                if let Some(path) = item.get("path").and_then(toml::Value::as_str) {
                    roots.push((crate_root.join(path), test_only));
                }
            }
        }
    }
    for (dir, test_only) in [("src/bin", false), ("tests", true), ("benches", true)] {
        let dir = crate_root.join(dir);
        if dir.is_dir() {
            for item in fs::read_dir(dir).map_err(|e| e.to_string())? {
                let path = item.map_err(|e| e.to_string())?.path();
                if path.extension().is_some_and(|e| e == "rs") {
                    roots.push((path, test_only));
                } else if path.join("main.rs").is_file() {
                    roots.push((path.join("main.rs"), test_only));
                }
            }
        }
    }
    let mut roles = BTreeSet::new();
    for (entry, test_only) in roots.into_iter().filter(|(p, _)| p.is_file()) {
        role_visit(
            &entry,
            entry.parent().ok_or("target has no parent")?,
            &target,
            test_only,
            &mut HashSet::new(),
            &mut roles,
            cache,
        )?;
    }
    if roles.contains(&false) {
        Ok(Some(false))
    } else if roles.contains(&true) {
        Ok(Some(true))
    } else {
        Ok(None)
    }
}

fn role_visit(
    file: &Path,
    module_dir: &Path,
    target: &Path,
    test_only: bool,
    seen: &mut HashSet<PathBuf>,
    roles: &mut BTreeSet<bool>,
    cache: &mut RoleCache,
) -> Result<(), String> {
    let file = file.canonicalize().map_err(|e| e.to_string())?;
    let syntax = if let Some(parsed) = cache.get(&file) {
        Rc::clone(parsed)
    } else {
        let text = fs::read_to_string(&file).map_err(|e| e.to_string())?;
        let parsed = Rc::new(parse_rust(&text)?);
        cache.insert(file.clone(), Rc::clone(&parsed));
        parsed
    };
    if file == target {
        if syntax.attrs.iter().any(|a| a.path().is_ident("cfg_attr") || (a.path().is_ident("cfg") && !test_cfg(std::slice::from_ref(a)) && matches!(&a.meta, syn::Meta::List(list) if list.tokens.to_string().split(|c:char| !c.is_alphanumeric() && c != '_').any(|s| s == "test")))) {
            return Err("conditional test crate attributes require expanded Cargo configuration".into());
        }
        roles.insert(test_only || test_cfg(&syntax.attrs));
        return Ok(());
    }
    if !seen.insert(file.clone()) {
        return Err("ambiguous or cyclic module graph".into());
    }
    role_items(
        &syntax.items,
        (module_dir, file.parent().ok_or("module has no parent")?),
        target,
        test_only,
        seen,
        roles,
        cache,
    )
}

fn role_items(
    items: &[syn::Item],
    directories: (&Path, &Path),
    target: &Path,
    test_only: bool,
    seen: &mut HashSet<PathBuf>,
    roles: &mut BTreeSet<bool>,
    cache: &mut RoleCache,
) -> Result<(), String> {
    let (module_dir, attr_dir) = directories;
    for item in items {
        let syn::Item::Mod(module) = item else {
            continue;
        };
        if module.attrs.iter().any(|a| a.path().is_ident("cfg") && !test_cfg(std::slice::from_ref(a)) && matches!(&a.meta, syn::Meta::List(list) if list.tokens.to_string().split(|c:char| !c.is_alphanumeric() && c != '_').any(|s| s == "test"))) {
            return Err("compound test cfg requires expanded Cargo configuration".into());
        }
        let child_test = test_only || test_cfg(&module.attrs);
        let name = module.ident.to_string();
        if let Some((_, nested)) = &module.content {
            role_items(
                nested,
                (&module_dir.join(&name), &attr_dir.join(&name)),
                target,
                child_test,
                seen,
                roles,
                cache,
            )?;
            continue;
        }
        let explicit = module.attrs.iter().find_map(|a| match &a.meta {
            syn::Meta::NameValue(v) if a.path().is_ident("path") => match &v.value {
                syn::Expr::Lit(v) => match &v.lit {
                    syn::Lit::Str(s) => Some(attr_dir.join(s.value())),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        });
        let path = explicit.unwrap_or_else(|| {
            let flat = module_dir.join(format!("{name}.rs"));
            if flat.is_file() {
                flat
            } else {
                module_dir.join(&name).join("mod.rs")
            }
        });
        if path.is_file() {
            let next = if path.file_name().is_some_and(|n| n == "mod.rs") {
                path.parent().unwrap().to_path_buf()
            } else {
                path.with_extension("")
            };
            role_visit(&path, &next, target, child_test, seen, roles, cache)?;
        }
    }
    Ok(())
}

// Captured keys must describe a complete, bounded, symlink-free tree rooted at
// `root`; absence is meaningful for Cargo target discovery. The caller owns
// capture and must reject incomplete directory inventories before invoking us.
trait RustReader {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, String>;
    fn is_file(&self, path: &Path) -> bool;
    fn is_dir(&self, path: &Path) -> bool;
    fn entries(&self, path: &Path) -> Result<Vec<PathBuf>, String>;
    fn read(&self, path: &Path) -> Result<String, String>;
    fn requires_modules(&self) -> bool {
        false
    }
}

struct AmbientReader;
impl RustReader for AmbientReader {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, String> {
        path.canonicalize().map_err(|e| e.to_string())
    }
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }
    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }
    fn entries(&self, path: &Path) -> Result<Vec<PathBuf>, String> {
        fs::read_dir(path)
            .map_err(|e| e.to_string())?
            .map(|entry| entry.map(|e| e.path()).map_err(|e| e.to_string()))
            .collect()
    }
    fn read(&self, path: &Path) -> Result<String, String> {
        fs::read_to_string(path).map_err(|e| e.to_string())
    }
}

struct CapturedReader<'a> {
    root: &'a Path,
    files: &'a BTreeMap<PathBuf, Vec<u8>>,
}
impl CapturedReader<'_> {
    fn path(&self, path: &Path) -> Result<PathBuf, String> {
        use std::path::Component;
        let mut result = PathBuf::new();
        for part in path.components() {
            match part {
                Component::RootDir => result.push(part.as_os_str()),
                Component::Normal(name) => result.push(name),
                Component::CurDir => {}
                Component::ParentDir if result.starts_with(self.root) && result != self.root => {
                    result.pop();
                }
                _ => return Err(format!("invalid captured Rust path: {}", path.display())),
            }
        }
        if !result.starts_with(self.root) {
            return Err(format!(
                "Rust path escapes captured root: {}",
                path.display()
            ));
        }
        Ok(result)
    }
}
impl RustReader for CapturedReader<'_> {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, String> {
        let path = self.path(path)?;
        if !self.files.contains_key(&path) {
            return Err(format!("captured Rust file missing: {}", path.display()));
        }
        Ok(path)
    }
    fn is_file(&self, path: &Path) -> bool {
        self.path(path)
            .is_ok_and(|path| self.files.contains_key(&path))
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.path(path).is_ok_and(|path| {
            self.files
                .keys()
                .any(|file| file.starts_with(&path) && file != &path)
        })
    }
    fn entries(&self, path: &Path) -> Result<Vec<PathBuf>, String> {
        let path = self.path(path)?;
        let mut entries = BTreeSet::new();
        for file in self.files.keys() {
            if let Ok(rest) = file.strip_prefix(&path) {
                if let Some(name) = rest.components().next() {
                    entries.insert(path.join(name));
                }
            }
        }
        Ok(entries.into_iter().collect())
    }
    fn read(&self, path: &Path) -> Result<String, String> {
        let path = self.canonicalize(path)?;
        String::from_utf8(self.files[&path].clone()).map_err(|e| e.to_string())
    }
    fn requires_modules(&self) -> bool {
        true
    }
}

/// Computes identities using only a complete captured tree. No filesystem reads.
pub(crate) fn captured_rust_test_labels(
    root: &Path,
    path: &Path,
    files: &BTreeMap<PathBuf, Vec<u8>>,
) -> Result<Map<String, Value>, String> {
    const MAX_FILES: usize = 4_096;
    const MAX_BYTES: usize = 32 * 1024 * 1024;
    if !root.is_absolute() || files.len() > MAX_FILES {
        return Err("invalid or oversized captured Rust tree".into());
    }
    let reader = CapturedReader { root, files };
    let mut total = 0usize;
    for (key, bytes) in files {
        if reader.path(key)? != *key {
            return Err(format!(
                "noncanonical captured Rust path: {}",
                key.display()
            ));
        }
        if bytes.len() > 1024 * 1024 {
            return Err(format!("captured Rust file too large: {}", key.display()));
        }
        total = total
            .checked_add(bytes.len())
            .ok_or("captured Rust tree too large")?;
        if total > MAX_BYTES {
            return Err("captured Rust tree too large".into());
        }
    }
    let target = reader.canonicalize(&root.join(path))?;
    let source = reader.read(&target)?;
    let syntax = parse_rust(&source)?;
    let mut labels = Map::new();
    rust_tests(&syntax.items, &source, "", false, &mut labels)?;
    if labels.is_empty() {
        return Ok(labels);
    }
    let relative = target.strip_prefix(root).map_err(|e| e.to_string())?;
    let relative = relative.to_str().ok_or("non-UTF-8 Rust path")?;
    let prefix = rust_prefix_with(&reader, root, relative)?;
    labels.clear();
    rust_tests(&syntax.items, &source, &prefix, false, &mut labels)?;
    Ok(labels)
}

fn rust_prefix(root: &Path, path: &str) -> Result<String, String> {
    rust_prefix_with(&AmbientReader, root, path)
}

fn rust_prefix_with(reader: &impl RustReader, root: &Path, path: &str) -> Result<String, String> {
    let target = reader
        .canonicalize(&root.join(path))
        .map_err(|error| format!("native module source unavailable {path}: {error}"))?;
    let crate_root = target
        .ancestors()
        .skip(1)
        .find(|dir| reader.is_file(&dir.join("Cargo.toml")))
        .ok_or_else(|| {
            format!("native Rust module identity unavailable: no Cargo.toml above {path}")
        })?;
    let manifest: toml::Value = toml::from_str(&reader.read(&crate_root.join("Cargo.toml"))?)
        .map_err(|error| error.to_string())?;
    let mut roots = BTreeSet::from([
        crate_root.join("src/lib.rs"),
        crate_root.join("src/main.rs"),
    ]);
    if let Some(lib) = manifest
        .get("lib")
        .and_then(|lib| lib.get("path"))
        .and_then(toml::Value::as_str)
    {
        roots.insert(crate_root.join(lib));
    }
    for key in ["bin", "test", "bench"] {
        if let Some(targets) = manifest.get(key).and_then(toml::Value::as_array) {
            for entry in targets {
                if let Some(path) = entry.get("path").and_then(toml::Value::as_str) {
                    roots.insert(crate_root.join(path));
                }
            }
        }
    }
    for dir in ["tests", "benches", "src/bin"] {
        let directory = crate_root.join(dir);
        if reader.is_dir(&directory) {
            for entry in reader.entries(&directory)? {
                if entry.extension().is_some_and(|ext| ext == "rs") {
                    roots.insert(entry);
                } else if reader.is_file(&entry.join("main.rs")) {
                    roots.insert(entry.join("main.rs"));
                }
            }
        }
    }
    let mut prefixes = BTreeSet::new();
    for entry in roots.into_iter().filter(|p| reader.is_file(p)) {
        let mut seen = HashSet::new();
        visit_file(
            reader,
            &entry,
            entry.parent().unwrap(),
            "",
            &target,
            &mut seen,
            &mut prefixes,
        )?;
    }
    if prefixes.len() != 1 {
        return Err(format!(
            "native Rust module identity unavailable or ambiguous for {path}: {prefixes:?}"
        ));
    }
    Ok(prefixes.into_iter().next().unwrap())
}

fn visit_file(
    reader: &impl RustReader,
    file: &Path,
    module_dir: &Path,
    prefix: &str,
    target: &Path,
    seen: &mut HashSet<PathBuf>,
    found: &mut BTreeSet<String>,
) -> Result<(), String> {
    let file = reader.canonicalize(file)?;
    if file == target {
        found.insert(prefix.into());
        return Ok(());
    }
    if seen.len() >= 4_096 || prefix.len() > 4_096 {
        return Err("native Rust module graph exceeds traversal bounds".into());
    }
    if !seen.insert(file.clone()) {
        return Err("native Rust module graph has repeated or cyclic external modules; identity is unsupported".into());
    }
    let source = reader.read(&file)?;
    let syntax = parse_rust(&source)?;
    visit_items(
        reader,
        &syntax.items,
        (module_dir, file.parent().unwrap()),
        prefix,
        target,
        seen,
        found,
    )
}

fn visit_items(
    reader: &impl RustReader,
    items: &[syn::Item],
    directories: (&Path, &Path),
    prefix: &str,
    target: &Path,
    seen: &mut HashSet<PathBuf>,
    found: &mut BTreeSet<String>,
) -> Result<(), String> {
    if prefix.len() > 4_096 {
        return Err("native Rust module nesting exceeds traversal bounds".into());
    }
    let (module_dir, attribute_dir) = directories;
    for item in items {
        let syn::Item::Mod(module) = item else {
            continue;
        };
        if module.attrs.iter().any(|a| a.path().is_ident("cfg_attr")) {
            return Err(
                "conditional Rust module attributes require expanded Cargo configuration".into(),
            );
        }
        if module
            .attrs
            .iter()
            .any(|a| a.path().is_ident("cfg") && !test_cfg(std::slice::from_ref(a)))
        {
            return Err(
                "conditional Rust module paths require expanded Cargo configuration".into(),
            );
        }
        let name = module.ident.to_string();
        let next_prefix = format!("{prefix}{name}::");
        if let Some((_, items)) = &module.content {
            visit_items(
                reader,
                items,
                (&module_dir.join(&name), &attribute_dir.join(&name)),
                &next_prefix,
                target,
                seen,
                found,
            )?;
            continue;
        }
        let explicit = module.attrs.iter().find_map(|attr| {
            if !attr.path().is_ident("path") {
                return None;
            }
            if let syn::Meta::NameValue(value) = &attr.meta {
                if let syn::Expr::Lit(value) = &value.value {
                    if let syn::Lit::Str(path) = &value.lit {
                        return Some(attribute_dir.join(path.value()));
                    }
                }
            }
            None
        });
        let path = explicit.unwrap_or_else(|| {
            let flat = module_dir.join(format!("{name}.rs"));
            if reader.is_file(&flat) {
                flat
            } else {
                module_dir.join(&name).join("mod.rs")
            }
        });
        if reader.is_file(&path) {
            let next_dir = if path.file_name().is_some_and(|n| n == "mod.rs") {
                path.parent().unwrap().to_path_buf()
            } else {
                path.with_extension("")
            };
            visit_file(reader, &path, &next_dir, &next_prefix, target, seen, found)?;
        } else if reader.requires_modules() {
            return Err(format!("captured Rust module missing: {}", path.display()));
        }
    }
    Ok(())
}

fn c_tests(node: tree_sitter::Node<'_>, source: &str, out: &mut Map<String, Value>) {
    if node.kind() == "function_definition" {
        if let Some(declarator) = node.child_by_field_name("declarator") {
            let mut name = declarator;
            while let Some(inner) = name.child_by_field_name("declarator") {
                name = inner;
            }
            let label = &source[name.byte_range()];
            if label.starts_with("test_") {
                out.insert(
                    label.into(),
                    Value::String(source[node.byte_range()].into()),
                );
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        c_tests(child, source, out);
    }
}

struct Lines<'a> {
    text: Vec<&'a str>,
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    fn new(source: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        );
        Self {
            text: source.lines().collect(),
            starts,
        }
    }

    fn number(&self, offset: usize) -> usize {
        self.starts.partition_point(|start| *start <= offset)
    }

    fn definition(&self, function: &ast::StmtFunctionDef) -> String {
        let start = self.number(function.range().start().to_usize());
        let end = self.number(function.range().end().to_usize().saturating_sub(1));
        self.text[start.saturating_sub(1)..end.min(self.text.len())].join("\n")
    }
}

fn add_function(
    definitions: &mut Map<String, Value>,
    lines: &Lines<'_>,
    function: &ast::StmtFunctionDef,
    class: Option<&str>,
) {
    let name = function.name.as_str();
    if !name.starts_with("test_") {
        return;
    }
    let label = class.map_or_else(|| name.to_owned(), |class| format!("{class}::{name}"));
    definitions.insert(label, Value::String(lines.definition(function)));
}

pub(super) fn python_test_definitions(source: &str, path: &str) -> Result<Value, String> {
    let document = crate::candidate_python::Document::parse(source)
        .map_err(|error| format!("cannot parse test definitions in {path}: {error}"))?;
    let module = document.module();
    let lines = Lines::new(source);
    let mut definitions = Map::new();
    for statement in &module.body {
        match statement {
            ast::Stmt::FunctionDef(function) => {
                add_function(&mut definitions, &lines, function, None);
            }
            ast::Stmt::ClassDef(class) if class.name.as_str().starts_with("Test") => {
                for child in &class.body {
                    if let ast::Stmt::FunctionDef(function) = child {
                        add_function(
                            &mut definitions,
                            &lines,
                            function,
                            Some(class.name.as_str()),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    Ok(Value::Object(definitions))
}

#[cfg(test)]
mod captured_reader_tests {
    use super::*;

    fn files(entries: &[(&str, &str)]) -> BTreeMap<PathBuf, Vec<u8>> {
        entries
            .iter()
            .map(|(path, source)| (PathBuf::from(path), source.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn captured_tree_finds_nested_identity_and_uses_captured_source() {
        let root = Path::new("/capture");
        let inventory = files(&[
            (
                "/capture/Cargo.toml",
                "[package]\nname='x'\nversion='0.1.0'",
            ),
            ("/capture/src/lib.rs", "mod outer;"),
            ("/capture/src/outer.rs", "mod inner;"),
            ("/capture/src/outer/inner.rs", "#[test] fn works() {}"),
        ]);
        let labels =
            captured_rust_test_labels(root, Path::new("src/outer/inner.rs"), &inventory).unwrap();
        assert_eq!(
            labels.keys().collect::<Vec<_>>(),
            vec!["outer::inner::works"]
        );
        assert_eq!(labels["outer::inner::works"], "#[test] fn works() {}");
    }

    #[test]
    fn captured_tree_rejects_missing_manifest_module_and_escape() {
        let root = Path::new("/capture");
        let target = ("/capture/src/test.rs", "#[test] fn works() {}");
        assert!(
            captured_rust_test_labels(root, Path::new("src/test.rs"), &files(&[target])).is_err()
        );
        let inventory = files(&[
            (
                "/capture/Cargo.toml",
                "[package]\nname='x'\nversion='0.1.0'",
            ),
            ("/capture/src/lib.rs", "mod missing; mod test;"),
            target,
        ]);
        assert!(
            captured_rust_test_labels(root, Path::new("src/test.rs"), &inventory)
                .unwrap_err()
                .contains("module missing")
        );
        assert!(captured_rust_test_labels(root, Path::new("../outside.rs"), &inventory).is_err());
    }

    #[test]
    fn captured_target_prefix_matches_ambient_traversal() {
        let root = std::env::temp_dir().join(format!("forge-captured-ast-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("src");
        std::fs::create_dir(&src).unwrap();
        let lib = src.join("lib.rs");
        let test = src.join("child.rs");
        std::fs::write(&lib, "mod child;").unwrap();
        std::fs::write(&test, "#[test] fn works() {}").unwrap();
        let inventory = BTreeMap::from([
            (lib.clone(), b"mod child;".to_vec()),
            (test.clone(), b"#[test] fn works() {}".to_vec()),
        ]);
        let reader = CapturedReader {
            root: &root,
            files: &inventory,
        };
        assert_eq!(
            target_prefix(&lib, &test).unwrap(),
            target_prefix_with(&reader, &lib, &test).unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
#[test]
fn captured_excessive_expression_nesting_refuses_before_syn() {
    let source = format!(
        "#[test] fn test_deep() {{ let _ = {}0{}; }}",
        "(".repeat(600),
        ")".repeat(600)
    );
    let root = Path::new("/captured");
    let inventory = BTreeMap::from([(root.join("src/lib.rs"), source.into_bytes())]);
    let error = captured_rust_test_labels(root, Path::new("src/lib.rs"), &inventory).unwrap_err();
    assert!(
        error.contains("depth") || error.contains("budget"),
        "{error}"
    );
}

#[cfg(test)]
mod rust_preflight_api_tests {
    use super::*;
    #[test]
    fn every_ambient_entry_point_rejects_deep_source_before_syn() {
        let source = format!(
            "#[test] fn f() {{ let _ = {}0{}; }}",
            "(".repeat(600),
            ")".repeat(600)
        );
        assert!(native_test_definitions(&source, "src/lib.rs", None)
            .unwrap_err()
            .contains("depth"));
        assert!(rust_has_tests(&source).unwrap_err().contains("depth"));
        let root =
            std::env::temp_dir().join(format!("forge-rust-preflight-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("lib.rs");
        std::fs::write(&target, &source).unwrap();
        assert!(target_test_labels(&target, &target)
            .unwrap_err()
            .contains("depth"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
