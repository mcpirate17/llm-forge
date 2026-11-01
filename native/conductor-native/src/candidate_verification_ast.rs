//! Collectible Python definitions with decorator-inclusive source identity.

use ruff_python_ast as ast;
use ruff_text_size::Ranged;
use serde_json::{Map, Value};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};
use syn::spanned::Spanned;

/// Extract actual native tests and their source identity, using language parsers.
/// Rust labels are the names printed by libtest; C labels follow the existing
/// ctest adapter's named-function contract. Unknown frameworks fail explicitly.
pub(super) fn native_test_definitions(
    source: &str,
    path: &str,
    root: Option<&Path>,
) -> Result<Value, String> {
    if path.ends_with(".rs") {
        let file = syn::parse_file(source)
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
    let file = syn::parse_file(source).map_err(|error| error.to_string())?;
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
    let target = target.canonicalize().map_err(|e| e.to_string())?;
    let mut prefixes = BTreeSet::new();
    let mut seen = HashSet::new();
    visit_file(
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
    let file = syn::parse_file(&source).map_err(|e| e.to_string())?;
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
        let parsed = Rc::new(syn::parse_file(&text).map_err(|e| e.to_string())?);
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

fn rust_prefix(root: &Path, path: &str) -> Result<String, String> {
    let target = root
        .join(path)
        .canonicalize()
        .map_err(|error| format!("native module source unavailable {path}: {error}"))?;
    let crate_root = target
        .ancestors()
        .skip(1)
        .find(|dir| dir.join("Cargo.toml").is_file())
        .ok_or_else(|| {
            format!("native Rust module identity unavailable: no Cargo.toml above {path}")
        })?;
    let manifest: toml::Value = toml::from_str(
        &fs::read_to_string(crate_root.join("Cargo.toml")).map_err(|error| error.to_string())?,
    )
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
        if directory.is_dir() {
            for entry in fs::read_dir(directory).map_err(|error| error.to_string())? {
                let entry = entry.map_err(|error| error.to_string())?.path();
                if entry.extension().is_some_and(|ext| ext == "rs") {
                    roots.insert(entry);
                } else if entry.join("main.rs").is_file() {
                    roots.insert(entry.join("main.rs"));
                }
            }
        }
    }
    let mut prefixes = BTreeSet::new();
    for entry in roots.into_iter().filter(|p| p.is_file()) {
        let mut seen = HashSet::new();
        visit_file(
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
    file: &Path,
    module_dir: &Path,
    prefix: &str,
    target: &Path,
    seen: &mut HashSet<PathBuf>,
    found: &mut BTreeSet<String>,
) -> Result<(), String> {
    let file = file.canonicalize().map_err(|error| error.to_string())?;
    if file == target {
        found.insert(prefix.into());
        return Ok(());
    }
    if !seen.insert(file.clone()) {
        return Err("native Rust module graph has repeated or cyclic external modules; identity is unsupported".into());
    }
    let source = fs::read_to_string(&file).map_err(|error| error.to_string())?;
    let syntax = syn::parse_file(&source).map_err(|error| error.to_string())?;
    visit_items(
        &syntax.items,
        (module_dir, file.parent().unwrap()),
        prefix,
        target,
        seen,
        found,
    )
}

fn visit_items(
    items: &[syn::Item],
    directories: (&Path, &Path),
    prefix: &str,
    target: &Path,
    seen: &mut HashSet<PathBuf>,
    found: &mut BTreeSet<String>,
) -> Result<(), String> {
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
            if flat.is_file() {
                flat
            } else {
                module_dir.join(&name).join("mod.rs")
            }
        });
        if path.is_file() {
            let next_dir = if path.file_name().is_some_and(|n| n == "mod.rs") {
                path.parent().unwrap().to_path_buf()
            } else {
                path.with_extension("")
            };
            visit_file(&path, &next_dir, &next_prefix, target, seen, found)?;
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
    let module = ruff_python_parser::parse_module(source)
        .map_err(|error| format!("cannot parse test definitions in {path}: {error}"))?
        .into_syntax();
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
