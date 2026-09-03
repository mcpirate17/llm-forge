//! One pass over the repository's test files, answering every later question from memory.
//!
//! The Python gate asked two questions per unit of work and paid a full repository
//! scan for each. `drivers_for` re-walked and re-parsed every `test_*.py` once per
//! module probed, and `refine_unexercised` spawned one `git grep` per function it
//! could not reach. Both are O(repository) per question when the answer for every
//! question is available from a single pass.
//!
//! The index also fixes what that scan could not see. Python's dominant import
//! idiom, `from pkg import module`, parses as `ImportFrom(module="pkg",
//! names=["module"])`; matching only on the `module` field never resolves it, so
//! the module's real driver tests were invisible. Binding the *names* as well as
//! the module makes the match a strict superset of the old one -- it can add a
//! driver, never remove one.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tree_sitter::Node;

/// Directories that never hold a repository's own tests and can cost more to walk
/// than everything else combined.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".venv",
    "venv",
    "node_modules",
    "target",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    "site-packages",
    ".eggs",
    "build",
    "dist",
];

/// Paths are interned: an identifier appearing in 800 files stores 800 `u32`s, not
/// 800 copies of a path. The name index over this repository holds ~10^6 postings.
pub struct TestIndex {
    files: Vec<String>,
    imports: HashMap<String, Vec<u32>>,
    names: HashMap<String, Vec<u32>>,
}

impl TestIndex {
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn import_key_count(&self) -> usize {
        self.imports.len()
    }

    pub fn name_key_count(&self) -> usize {
        self.names.len()
    }

    fn resolve(&self, ids: Option<&Vec<u32>>) -> Vec<String> {
        let mut out: Vec<String> = match ids {
            Some(v) => v.iter().map(|i| self.files[*i as usize].clone()).collect(),
            None => Vec::new(),
        };
        out.sort();
        out
    }

    /// Test files that import `dotted`.
    pub fn drivers_for_dotted(&self, dotted: &str) -> Vec<String> {
        self.resolve(self.imports.get(dotted))
    }

    /// Test files that import the module at repository-relative path `module`.
    pub fn drivers_for(&self, module: &str) -> Vec<String> {
        self.drivers_for_dotted(&dotted_for(module))
    }

    /// Test files in which `name` appears as a whole word -- the `git grep -w -F`
    /// question, asked of the index instead of a subprocess.
    pub fn named_by(&self, name: &str) -> Vec<String> {
        self.resolve(self.names.get(name))
    }
}

/// `conductor/slop_gate.py` -> `conductor.slop_gate`; a package's `__init__.py`
/// resolves to the package itself, which is what an importer names.
pub fn dotted_for(module: &str) -> String {
    let stem = module.strip_suffix(".py").unwrap_or(module);
    let stem = stem.strip_suffix("/__init__").unwrap_or(stem);
    stem.replace('/', ".")
}

fn is_test_file(name: &str) -> bool {
    name.starts_with("test_") && name.ends_with(".py")
}

/// Every `test_*.py` under `root`, repository-relative, in a deterministic order.
pub fn walk_tests(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let ty = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ty.is_dir() {
                if !SKIP_DIRS.contains(&name.as_ref()) {
                    stack.push(entry.path());
                }
            } else if ty.is_file() && is_test_file(&name) {
                if let Ok(rel) = entry.path().strip_prefix(root) {
                    out.push(rel.to_path_buf());
                }
            }
        }
    }
    out.sort();
    out
}

/// Whole words, as `git grep -w -F` understands them: maximal runs of `[A-Za-z0-9_]`.
///
/// Deliberately a byte scan and not a parse. `git grep` matches inside strings and
/// comments, and a name reached only through `getattr(mod, "thing")` is exactly the
/// kind of indirect reference this question exists to catch.
fn words(src: &[u8], into: &mut HashSet<String>) {
    let mut start: Option<usize> = None;
    for (i, b) in src.iter().enumerate() {
        let wordish = b.is_ascii_alphanumeric() || *b == b'_';
        match (wordish, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                into.insert(String::from_utf8_lossy(&src[s..i]).into_owned());
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        into.insert(String::from_utf8_lossy(&src[s..]).into_owned());
    }
}

/// The dotted package a relative import resolves against.
///
/// `from . import x` in `conductor/test_handoff.py` means `conductor.x`; each extra
/// leading dot climbs one package. A prefix that climbs past the root yields nothing
/// rather than a bare or negative-depth name.
fn resolve_relative(rel: &Path, level: usize) -> Option<String> {
    let mut parts: Vec<&str> = rel
        .parent()
        .map(|p| {
            p.components()
                .map(|c| c.as_os_str().to_str().unwrap_or(""))
                .collect()
        })
        .unwrap_or_default();
    parts.retain(|p| !p.is_empty() && *p != ".");
    if level == 0 || level - 1 > parts.len() {
        return None;
    }
    parts.truncate(parts.len() - (level - 1));
    Some(parts.join("."))
}

fn dotted_text(src: &[u8], n: Node) -> String {
    String::from_utf8_lossy(&src[n.byte_range()]).into_owned()
}

/// The module a `dotted_name` or `aliased_import` names.
fn imported_name(src: &[u8], n: Node) -> Option<String> {
    match n.kind() {
        "dotted_name" => Some(dotted_text(src, n)),
        "aliased_import" => n.child_by_field_name("name").map(|c| dotted_text(src, c)),
        _ => None,
    }
}

/// Register `base` and, for a `from` import, `base.name` for every name bound.
///
/// Binding the names is the fix: `from conductor import slop_gate` is the form that
/// was invisible, and it is by a wide margin the most common one in this repository.
fn collect_imports(src: &[u8], rel: &Path, node: Node, out: &mut HashSet<String>) {
    match node.kind() {
        "import_statement" => {
            let mut cur = node.walk();
            for child in node.named_children(&mut cur) {
                if let Some(name) = imported_name(src, child) {
                    out.insert(name);
                }
            }
        }
        // `from __future__ import annotations` is its own node kind, not an
        // import_from_statement. Nothing probes `__future__`, but leaving it out
        // would make "the index resolves every import the old matcher did" false
        // by a special case, and an invariant with a special case is not one.
        "future_import_statement" => {
            out.insert("__future__".to_string());
            let mut cur = node.walk();
            for child in node.named_children(&mut cur) {
                if let Some(name) = imported_name(src, child) {
                    out.insert(format!("__future__.{name}"));
                }
            }
        }
        "import_from_statement" => {
            let module = node.child_by_field_name("module_name");
            let base = match module {
                Some(m) if m.kind() == "relative_import" => {
                    let mut cur = m.walk();
                    let mut level = 0usize;
                    let mut tail: Option<String> = None;
                    for child in m.children(&mut cur) {
                        match child.kind() {
                            "import_prefix" => {
                                level = src[child.byte_range()]
                                    .iter()
                                    .filter(|b| **b == b'.')
                                    .count()
                            }
                            "dotted_name" => tail = Some(dotted_text(src, child)),
                            _ => {}
                        }
                    }
                    match (resolve_relative(rel, level), tail) {
                        (Some(pkg), Some(t)) if pkg.is_empty() => Some(t),
                        (Some(pkg), Some(t)) => Some(format!("{pkg}.{t}")),
                        (Some(pkg), None) if !pkg.is_empty() => Some(pkg),
                        _ => None,
                    }
                }
                Some(m) => imported_name(src, m),
                None => None,
            };
            let Some(base) = base else { return };
            out.insert(base.clone());
            let mut cur = node.walk();
            for child in node.named_children(&mut cur) {
                if Some(child.id()) == module.map(|m| m.id()) {
                    continue;
                }
                if let Some(name) = imported_name(src, child) {
                    out.insert(format!("{base}.{name}"));
                }
            }
        }
        _ => {}
    }
}

fn imports_of(src: &[u8], rel: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let source = String::from_utf8_lossy(src);
    let Some(tree) = crate::engine::parse(&source) else {
        return out;
    };
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        collect_imports(src, rel, n, &mut out);
        // An import inside a function or a `try` is still an import; nothing here
        // prunes, because a conditional import is exactly how optional native
        // paths are loaded in this repository.
        let mut cur = n.walk();
        for child in n.named_children(&mut cur) {
            stack.push(child);
        }
    }
    out
}

/// Build the index with one pass over the repository's test files.
pub fn build(root: &Path) -> TestIndex {
    let paths = walk_tests(root);
    let mut files: Vec<String> = Vec::with_capacity(paths.len());
    let mut imports: HashMap<String, Vec<u32>> = HashMap::new();
    let mut names: HashMap<String, Vec<u32>> = HashMap::new();

    for rel in &paths {
        let Ok(src) = std::fs::read(root.join(rel)) else {
            continue;
        };
        let id = files.len() as u32;
        files.push(rel.to_string_lossy().into_owned());

        for key in imports_of(&src, rel) {
            imports.entry(key).or_default().push(id);
        }
        let mut ws = HashSet::new();
        words(&src, &mut ws);
        for w in ws {
            names.entry(w).or_default().push(id);
        }
    }
    TestIndex {
        files,
        imports,
        names,
    }
}
