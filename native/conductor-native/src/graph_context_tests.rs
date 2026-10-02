//! Conservative reverse dependency test selection over either graph backend.

use super::store::{columns, current_hash, GraphStore};
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet, VecDeque};
use std::path::Path;

const MAX_DEPTH: usize = 12;
const MAX_NODES: usize = 10_000;
const MAX_INVENTORY: usize = 100_000;

pub(super) fn is_test(path: &str) -> bool {
    let extension = Path::new(path)
        .extension()
        .and_then(|part| part.to_str())
        .unwrap_or("");
    if ![
        "py", "rs", "js", "ts", "jsx", "tsx", "c", "cc", "cpp", "h", "hpp",
    ]
    .contains(&extension)
    {
        return false;
    }
    let name = Path::new(path)
        .file_name()
        .and_then(|part| part.to_str())
        .unwrap_or("");
    name.starts_with("test_")
        || name.ends_with("_test.py")
        || name.ends_with("_test.rs")
        || path.split('/').any(|part| part == "tests")
        || [".test.js", ".test.ts", ".spec.js", ".spec.ts"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

struct Inventory {
    tests: BTreeSet<String>,
    sources: BTreeSet<String>,
}

fn inventory(root: &Path) -> Result<Inventory, String> {
    let mut tests = BTreeSet::new();
    let mut sources = BTreeSet::new();
    let mut pending = vec![root.to_path_buf()];
    let mut entries = 0;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            entries += 1;
            if entries > MAX_INVENTORY {
                return Err("test inventory exceeds 100000 entries".to_owned());
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if kind.is_dir()
                && !name.starts_with('.')
                && ![
                    "target",
                    "node_modules",
                    "build",
                    "dist",
                    "__pycache__",
                    "venv",
                ]
                .contains(&name.as_str())
            {
                pending.push(entry.path());
            } else if kind.is_file() {
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|error| error.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/");
                if is_test(&relative) {
                    tests.insert(relative.clone());
                }
                if relative.ends_with(".rs") {
                    if entry.metadata().map_err(|error| error.to_string())?.len() > 1 << 20 {
                        return Err("Rust test inventory source exceeds 1 MiB".to_owned());
                    }
                    let source =
                        std::fs::read_to_string(entry.path()).map_err(|error| error.to_string())?;
                    if source.contains("#[test]")
                        || source.contains("::test]")
                        || source.contains("#[cfg(test)]")
                    {
                        tests.insert(relative);
                    }
                }
                if ["py", "rs"].contains(
                    &entry
                        .path()
                        .extension()
                        .and_then(|part| part.to_str())
                        .unwrap_or(""),
                ) {
                    sources.insert(
                        entry
                            .path()
                            .strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }
    }
    Ok(Inventory { tests, sources })
}

fn validate_inventory(
    store: &GraphStore,
    root: &Path,
    inventory: &Inventory,
    reasons: &mut BTreeSet<String>,
) -> Result<(), String> {
    if !store.metadata.contains_key("content_generation") {
        return Ok(());
    }
    for path in &inventory.sources {
        let expected = store
            .indexed_hash(root, path)
            .map_err(|error| error.to_string())?;
        if expected.is_none()
            || current_hash(root, path).map_err(|error| error.to_string())? != expected
        {
            reasons.insert("working source inventory differs from graph snapshot".to_owned());
            return Ok(());
        }
    }
    let conn = store.conn.as_ref().ok_or("graph database unavailable")?;
    let count: i64 = conn
        .query_row("SELECT count(*) FROM files", [], |row| row.get(0))
        .map_err(|error| error.to_string())?;
    if count as usize != inventory.sources.len() {
        reasons.insert("working source inventory differs from graph snapshot".to_owned());
    }
    Ok(())
}

fn relative(root: &Path, raw: String) -> Option<String> {
    let path = Path::new(&raw);
    if path.is_absolute() {
        path.strip_prefix(root)
            .ok()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
    } else if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        None
    } else {
        Some(raw)
    }
}

fn reverse(store: &GraphStore, root: &Path, target: &str) -> Result<Vec<(String, bool)>, String> {
    let conn = store.conn.as_ref().ok_or("graph database unavailable")?;
    let fields = columns(conn, "nodes").map_err(|error| error.to_string())?;
    let test = if fields.contains("is_test") {
        "COALESCE(source.is_test,0)"
    } else {
        "0"
    };
    let edge_fields = columns(conn, "edges").map_err(|error| error.to_string())?;
    let filter = if edge_fields.contains("kind") {
        "AND (edge.kind IS NULL OR lower(edge.kind)!='contains')"
    } else {
        ""
    };
    let query = format!("SELECT DISTINCT source.file_path,{test} FROM nodes AS target JOIN edges AS edge ON edge.target_qualified=target.qualified_name JOIN nodes AS source ON source.qualified_name=edge.source_qualified WHERE target.file_path IN (?1,?2) {filter} ORDER BY source.file_path LIMIT 10001");
    let mut stmt = conn.prepare(&query).map_err(|error| error.to_string())?;
    let rows = stmt
        .query_map(
            params![target, root.join(target).to_string_lossy()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? != 0)),
        )
        .map_err(|error| error.to_string())?;
    let mut result = Vec::new();
    for row in rows {
        let (path, test) = row.map_err(|error| error.to_string())?;
        if path.len() > 4096 {
            return Err("graph test path exceeds byte budget".to_owned());
        }
        if let Some(path) = relative(root, path) {
            result.push((path, test));
        }
    }
    Ok(result)
}

fn reasons(store: &GraphStore, root: &Path, input: &Value) -> Result<BTreeSet<String>, String> {
    let mut result = BTreeSet::new();
    let expected = input.get("expected_head").and_then(Value::as_str);
    if let Some(expected) = expected {
        if store.metadata.get("git_head_sha").map(String::as_str) != Some(expected) {
            result.insert("graph build revision differs".to_owned());
        }
    } else if !store.metadata.contains_key("content_generation")
        && !store.metadata.contains_key("git_head_sha")
    {
        result.insert("graph provenance unverified".to_owned());
    }
    if !store.metadata.contains_key("coverage_complete") {
        result.insert("graph dynamic/import coverage unverified".to_owned());
    }
    if store
        .metadata
        .get("coverage_complete")
        .is_some_and(|value| value == "false")
        && !store.has_file_coverage
        && !store.metadata.contains_key("unresolved_files")
    {
        result.insert("graph per-file dependency coverage unverified".to_owned());
    }
    if let Some(conn) = &store.conn {
        let fields = columns(conn, "edges").map_err(|error| error.to_string())?;
        if fields.contains("kind") {
            let unknown: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM edges WHERE kind IS NULL OR trim(kind)='')",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            if unknown {
                result.insert("graph edge kinds unverified".to_owned());
            }
        } else {
            result.insert("graph edge kinds unverified".to_owned());
        }
    }
    if !root.is_dir() {
        return Err("test selection repository does not exist".to_owned());
    }
    Ok(result)
}

fn visit_dependencies(
    store: &GraphStore,
    root: &Path,
    paths: &[String],
    reasons: &mut BTreeSet<String>,
) -> Result<BTreeSet<String>, String> {
    let mut selected = BTreeSet::new();
    let mut seen = HashSet::new();
    let mut queue: VecDeque<(String, usize)> =
        paths.iter().cloned().map(|path| (path, 0)).collect();
    let unresolved: HashSet<String> = store
        .metadata
        .get("unresolved_files")
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_default();
    while let Some((path, depth)) = queue.pop_front() {
        if !seen.insert(path.clone()) {
            continue;
        }
        if seen.len() > MAX_NODES {
            reasons.insert("reverse dependency node cap reached".to_owned());
            break;
        }
        let unknown = store
            .file_unresolved(root, &path)
            .map_err(|error| error.to_string())?
            .unwrap_or_else(|| unresolved.contains(&path));
        if unknown {
            reasons.insert("affected source has unresolved or dynamic dependencies".to_owned());
        }
        match store
            .indexed_hash(root, &path)
            .map_err(|error| error.to_string())?
        {
            Some(hash)
                if current_hash(root, &path)
                    .map_err(|error| error.to_string())?
                    .as_deref()
                    == Some(&hash) => {}
            _ => {
                reasons.insert("affected source unindexed or dirty".to_owned());
            }
        }
        if is_test(&path) && root.join(&path).is_file() {
            selected.insert(path.clone());
        }
        let callers = reverse(store, root, &path)?;
        if depth >= MAX_DEPTH && !callers.is_empty() {
            reasons.insert("reverse dependency depth cap reached".to_owned());
            continue;
        }
        for (caller, test) in callers {
            if test && root.join(&caller).is_file() {
                selected.insert(caller.clone());
            }
            queue.push_back((caller, depth + 1));
        }
    }
    Ok(selected)
}

pub(super) fn select(input: &Value) -> Result<Value, String> {
    let root = Path::new(super::string_field(input, "repo")?)
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let raw_paths = input
        .get("paths")
        .and_then(Value::as_array)
        .ok_or("test selection paths must be an array")?;
    if raw_paths.len() > 1024 {
        return Err("test selection accepts at most 1024 source paths".to_owned());
    }
    let paths = raw_paths
        .iter()
        .map(|path| {
            let raw = path.as_str().ok_or("test source path must be a string")?;
            relative(&root, raw.to_owned()).ok_or("test source path escapes repository")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let store = GraphStore::open(&root, None)
        .map_err(|error| format!("code-review graph unreadable: {error}"))?;
    if store.conn.is_none() {
        return Err("code-review graph missing or incompatible; run forge graph index".to_owned());
    }
    let mut reasons = reasons(&store, &root, input)?;
    let inventory = inventory(&root)?;
    validate_inventory(&store, &root, &inventory, &mut reasons)?;
    let selected = visit_dependencies(&store, &root, &paths, &mut reasons)?;
    let complete = reasons.is_empty();
    let paths = if complete {
        selected
    } else {
        selected.union(&inventory.tests).cloned().collect()
    };
    store
        .verify_generation()
        .map_err(|error| error.to_string())?;
    Ok(json!({"paths": paths, "complete": complete,
        "scope": if complete {"transitive-reverse-dependencies"} else {"full-test-inventory-fallback"},
        "reasons": reasons, "generation": store.generation, "metadata": store.metadata}))
}
