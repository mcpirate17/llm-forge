//! Conservative reverse dependency test selection over either graph backend.
//!
//! Only changed code files select tests: a change to a document, fixture or other data
//! selects none, and a changed lock file, build definition (`setup.py`,
//! `CMakeLists.txt`, `*.cmake`, `meson.build`) or C, C++ or CUDA file
//! selects all of them. A test is selected when the graph shows it depends on a
//! changed `.py` or `.rs` file, when its newest recorded run executed or opened a
//! changed code file (`[tool.forge.test_record]`), or when its text names one, which is
//! how a test that imports through a string or runs a script by path is found.

use super::store::{columns, current_hash, GraphStore};
use crate::test_record_config::store_for;
use crate::test_record_store::open_read_only;
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::Path;

const MAX_DEPTH: usize = 12;
const MAX_NODES: usize = 10_000;
const MAX_INVENTORY: usize = 100_000;
/// Extensions of the files a test can import or run; a change to any other file
/// selects no test.
const CODE: &[&str] = &[
    "py", "rs", "sh", "bash", "zsh", "c", "cc", "cpp", "cxx", "h", "hpp", "cu", "cuh", "js", "mjs",
    "cjs", "ts", "jsx", "tsx", "go", "java", "rb", "lua", "pl",
];
/// Lock files: a change swaps dependency code that any test can run, so it selects
/// the full inventory.
const LOCKS: &[&str] = &[
    "uv.lock",
    "Cargo.lock",
    "package-lock.json",
    "poetry.lock",
    "yarn.lock",
    "pnpm-lock.yaml",
];
/// C, C++ and CUDA sources and headers build into a library the tests load, and
/// neither the graph nor the test record links that library back to its sources, so
/// a change selects the full inventory.
const COMPILED: &[&str] = &["c", "cc", "cpp", "cxx", "h", "hpp", "cu", "cuh"];
/// Build definitions, by file name, beside every `*.cmake`: they decide how a library
/// the tests load is built, and nothing links them to those tests, so a change selects
/// the full inventory. A Makefile is left out because most only orchestrate, and
/// `build.rs` because the graph links a maturin crate's files to the tests that import
/// its module.
const BUILD_DEFINITIONS: &[&str] = &["setup.py", "CMakeLists.txt", "meson.build"];
/// Directories the inventory never walks, beside every `.`-prefixed one.
const UNWALKED: &[&str] = &[
    "target",
    "node_modules",
    "build",
    "dist",
    "__pycache__",
    "venv",
];

fn extension(path: &str) -> &str {
    Path::new(path)
        .extension()
        .and_then(|part| part.to_str())
        .unwrap_or("")
}

pub(super) fn is_test(path: &str) -> bool {
    if ![
        "py", "rs", "js", "ts", "jsx", "tsx", "c", "cc", "cpp", "h", "hpp",
    ]
    .contains(&extension(path))
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
    /// How many files in the tree carry each file name.
    names: HashMap<String, usize>,
}

fn inventory(root: &Path) -> Result<Inventory, String> {
    let mut tests = BTreeSet::new();
    let mut sources = BTreeSet::new();
    let mut names = HashMap::new();
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
            if kind.is_dir() && !name.starts_with('.') && !UNWALKED.contains(&name.as_str()) {
                pending.push(entry.path());
            } else if kind.is_file() {
                *names.entry(name).or_insert(0) += 1;
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
                        tests.insert(relative.clone());
                    }
                }
                if ["py", "rs"].contains(&extension(&relative)) {
                    sources.insert(relative);
                }
            }
        }
    }
    Ok(Inventory {
        tests,
        sources,
        names,
    })
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
        result.insert(
            "graph dynamic/import coverage unverified: graph records no coverage_complete \
             (only `forge graph index` writes it; a code-review-graph database cannot)"
                .to_owned(),
        );
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

fn module_name(path: &str) -> String {
    path.trim_end_matches(".py")
        .trim_end_matches("/__init__")
        .replace('/', ".")
}

/// Whether `text` holds `name` whole, not as part of a longer identifier.
fn names(text: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    text.match_indices(name).any(|(at, _)| {
        !text[..at].chars().next_back().is_some_and(word)
            && !text[at + name.len()..].chars().next().is_some_and(word)
    })
}

/// The tests whose text names a changed code file by its path, its dotted module, or
/// a file name no other inventoried file carries. The graph never sees a test that
/// runs a script by path or imports a module through a string.
fn tests_naming(
    present: &Path,
    inventory: &Inventory,
    paths: &[String],
) -> Result<BTreeSet<String>, String> {
    let mut wanted = Vec::new();
    for path in paths {
        wanted.push(path.clone());
        if path.ends_with(".py") {
            let module = module_name(path.strip_prefix("src/").unwrap_or(path));
            if module.contains('.') {
                wanted.push(module);
            }
        }
        let name = path.rsplit('/').next().unwrap_or(path);
        // The changed file is itself counted unless it is gone or sits where the
        // inventory never walks.
        let walked = !path
            .split('/')
            .any(|part| part.starts_with('.') || UNWALKED.contains(&part));
        let own = usize::from(walked && present.join(path).is_file());
        if name != path && inventory.names.get(name).copied().unwrap_or(0) <= own {
            wanted.push(name.to_owned());
        }
    }
    let mut found = BTreeSet::new();
    if wanted.is_empty() {
        return Ok(found);
    }
    for test in &inventory.tests {
        let bytes =
            std::fs::read(present.join(test)).map_err(|error| format!("{test}: {error}"))?;
        let text = String::from_utf8_lossy(&bytes);
        if wanted.iter().any(|name| names(&text, name)) {
            found.insert(test.clone());
        }
    }
    Ok(found)
}

/// Lines of the files that can register pytest plugins (`-p`, `pytest11` entry points,
/// `pytest_plugins`, a `plugin` key); a plugin is code every test runs.
fn pytest_config(root: &Path) -> String {
    ["pyproject.toml", "pytest.ini", "setup.cfg", "tox.ini"]
        .iter()
        .filter_map(|name| std::fs::read_to_string(root.join(name)).ok())
        .flat_map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter(|line| {
            let line = line.to_lowercase();
            line.contains("plugin") || line.contains("pytest11") || line.contains(" -p ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The test files whose newest recorded run executed or opened one of `paths` in a
/// fixture or the test itself, in the store `[tool.forge.test_record]` names; none
/// without one. Any recorded content of the file counts, since a changed file differs
/// from it.
fn recorded_tests(
    root: &Path,
    present: &Path,
    paths: &[String],
) -> Result<BTreeSet<String>, String> {
    let mut selected = BTreeSet::new();
    if paths.is_empty() {
        return Ok(selected);
    }
    let Some(store) = store_for(root)? else {
        return Ok(selected);
    };
    let Some(conn) = open_read_only(&store).map_err(|error| format!("{error:#}"))? else {
        return Ok(selected);
    };
    let wanted: HashSet<&str> = paths.iter().map(String::as_str).collect();
    let mut stmt = conn
        .prepare(
            "SELECT run.path, window.digest, window.lines, window.deps FROM runs AS run \
             JOIN run_windows AS link ON link.run=run.id \
             JOIN windows AS window ON window.digest=link.window \
             WHERE window.kind IN ('fixture','test') \
             AND run.id IN (SELECT max(id) FROM runs GROUP BY nodeid)",
        )
        .map_err(|error| error.to_string())?;
    let mut rows = stmt.query([]).map_err(|error| error.to_string())?;
    let mut uses: HashMap<String, bool> = HashMap::new();
    while let Some(row) = rows.next().map_err(|error| error.to_string())? {
        let path: String = row.get(0).map_err(|error| error.to_string())?;
        let digest: String = row.get(1).map_err(|error| error.to_string())?;
        let hit = match uses.get(&digest) {
            Some(hit) => *hit,
            None => {
                let decode = |column: usize| -> Result<Vec<u8>, String> {
                    let blob: Vec<u8> = row.get(column).map_err(|error| error.to_string())?;
                    zstd::decode_all(blob.as_slice()).map_err(|error| error.to_string())
                };
                let lines: Vec<(String, String, Vec<usize>)> =
                    serde_json::from_slice(&decode(2)?).map_err(|error| error.to_string())?;
                let deps: Vec<(String, String, String)> =
                    serde_json::from_slice(&decode(3)?).map_err(|error| error.to_string())?;
                let hit = lines
                    .iter()
                    .any(|(label, _, _)| wanted.contains(label.as_str()))
                    || deps
                        .iter()
                        .any(|(kind, label, _)| kind == "file" && wanted.contains(label.as_str()));
                uses.insert(digest, hit);
                hit
            }
        };
        if hit && present.join(&path).is_file() {
            selected.insert(path);
        }
    }
    Ok(selected)
}

fn visit_dependencies(
    store: &GraphStore,
    root: &Path,
    present: &Path,
    paths: &[String],
    reasons: &mut BTreeSet<String>,
) -> Result<BTreeSet<String>, String> {
    let mut selected = BTreeSet::new();
    let mut seen = HashSet::new();
    let mut queue: VecDeque<(String, usize)> =
        paths.iter().cloned().map(|path| (path, 0)).collect();
    let config = pytest_config(root);
    let unresolved: HashSet<String> = store
        .metadata
        .get("unresolved_files")
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or_default();
    while let Some((path, depth)) = queue.pop_front() {
        if !seen.insert(path.clone()) {
            continue;
        }
        if Path::new(&path)
            .file_name()
            .is_some_and(|name| name == "conftest.py")
        {
            reasons.insert(
                "conftest.py is reachable from a changed path and applies to every test below it"
                    .to_owned(),
            );
        }
        if path.ends_with(".py")
            && module_name(&path).contains('.')
            && config.contains(&module_name(&path))
        {
            reasons.insert("a pytest plugin named in the pytest configuration is reachable from a changed path".to_owned());
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
        if is_test(&path) && present.join(&path).is_file() {
            selected.insert(path.clone());
        }
        let callers = reverse(store, root, &path)?;
        if depth >= MAX_DEPTH && !callers.is_empty() {
            reasons.insert("reverse dependency depth cap reached".to_owned());
            continue;
        }
        for (caller, test) in callers {
            if test && present.join(&caller).is_file() {
                selected.insert(caller.clone());
            }
            queue.push_back((caller, depth + 1));
        }
    }
    Ok(selected)
}

/// A candidate snapshot is trusted only when its source inventory can be checked
/// against the graph: every py/rs file outside the declared changed paths must match
/// the graph's recorded hash, and none may be missing. Otherwise the diff is wider
/// than declared or the graph never saw it, selection cannot be shown sound, and the
/// full inventory runs. Test files the graph has never seen (new or renamed) have no
/// reverse dependencies to reach them, so they are selected explicitly. Returns the
/// changed paths still to walk and the explicitly added tests.
/// Tracked `.py`/`.rs` paths of a git checkout; `None` when `root` is not one.
fn tracked_sources(root: &Path) -> Result<Option<HashSet<String>>, String> {
    if !root.join(".git").exists() {
        return Ok(None);
    }
    let output = std::process::Command::new("git")
        .args(["ls-files", "-z", "--", "*.py", "*.rs"])
        .current_dir(root)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(Some(
        output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|raw| !raw.is_empty())
            .map(|raw| String::from_utf8_lossy(raw).into_owned())
            .collect(),
    ))
}

fn candidate_delta(
    store: &GraphStore,
    (root, candidate_root): (&Path, &Path),
    (base, candidate): (&Inventory, &Inventory),
    paths: &[String],
    reasons: &mut BTreeSet<String>,
) -> (Vec<String>, BTreeSet<String>) {
    let declared: HashSet<&String> = paths.iter().collect();
    let mut added = BTreeSet::new();
    let mut walk = Vec::new();
    for path in paths {
        let unknown = !matches!(store.indexed_hash(root, path), Ok(Some(_)));
        if unknown && candidate.tests.contains(path) {
            added.insert(path.clone());
        } else if unknown && candidate.sources.contains(path) && !base.sources.contains(path) {
            // A new module: nothing in the graph can depend on it yet, and the
            // files that import it are themselves declared changes.
        } else {
            walk.push(path.clone());
        }
    }
    for path in candidate.sources.iter().filter(|p| !declared.contains(p)) {
        match (
            store.indexed_hash(root, path),
            current_hash(candidate_root, path),
        ) {
            (Ok(Some(expected)), Ok(Some(actual))) if expected == actual => {}
            (Ok(None), Ok(Some(_))) if candidate.tests.contains(path) => {
                added.insert(path.clone());
            }
            (Ok(None), Ok(Some(_))) => {
                reasons
                    .insert("candidate adds a source file the graph has never indexed".to_owned());
            }
            (Ok(Some(_)), Ok(Some(_))) => {
                reasons.insert(
                    "candidate source differs from the graph outside the declared changed paths"
                        .to_owned(),
                );
            }
            _ => {
                reasons
                    .insert("candidate source could not be checked against the graph".to_owned());
            }
        }
    }
    // A candidate snapshot holds tracked files only, so base files that git
    // ignores or has never tracked (scratch trees, generated output) cannot be
    // "removed" by it.
    let tracked = match tracked_sources(root) {
        Ok(tracked) => tracked,
        Err(error) => {
            reasons.insert(format!("tracked source listing failed: {error}"));
            None
        }
    };
    if base.sources.iter().any(|path| {
        !candidate.sources.contains(path)
            && !declared.contains(path)
            && tracked.as_ref().is_none_or(|set| set.contains(path))
    }) {
        reasons.insert(
            "candidate removes a source file outside the declared changed paths".to_owned(),
        );
    }
    (walk, added)
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
    let inventory_root = match input.get("inventory_root") {
        Some(value) => Path::new(
            value
                .as_str()
                .ok_or("inventory root must be a path string")?,
        )
        .canonicalize()
        .map_err(|error| format!("candidate inventory unavailable: {error}"))?,
        None => root.clone(),
    };
    let root_inventory = inventory(&root)?;
    validate_inventory(&store, &root, &root_inventory, &mut reasons)?;
    let (inventory, walk, mut selected) = if inventory_root != root {
        let candidate = inventory(&inventory_root)?;
        let (walk, added) = candidate_delta(
            &store,
            (&root, &inventory_root),
            (&root_inventory, &candidate),
            &paths,
            &mut reasons,
        );
        (candidate, walk, added)
    } else {
        (root_inventory, paths, BTreeSet::new())
    };
    if walk
        .iter()
        .any(|path| LOCKS.contains(&path.rsplit('/').next().unwrap_or(path)))
    {
        reasons.insert("a dependency lock file changed, so any test can run new code".to_owned());
    }
    if walk.iter().any(|path| {
        let name = path.rsplit('/').next().unwrap_or(path);
        BUILD_DEFINITIONS.contains(&name) || name.ends_with(".cmake")
    }) {
        reasons.insert(
            "a build definition file changed, and nothing links what it builds to the tests that load it"
                .to_owned(),
        );
    }
    let code: Vec<String> = walk
        .into_iter()
        .filter(|path| CODE.contains(&extension(path)))
        .collect();
    if code.iter().any(|path| COMPILED.contains(&extension(path))) {
        reasons.insert(
            "a C, C++ or CUDA file changed, and nothing links the library it builds to the tests that load it"
                .to_owned(),
        );
    }
    let graphed: Vec<String> = code
        .iter()
        .filter(|path| ["py", "rs"].contains(&extension(path)))
        .cloned()
        .collect();
    selected.extend(visit_dependencies(
        &store,
        &root,
        &inventory_root,
        &graphed,
        &mut reasons,
    )?);
    selected.extend(recorded_tests(&root, &inventory_root, &code)?);
    selected.extend(tests_naming(&inventory_root, &inventory, &code)?);
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

#[cfg(test)]
mod tests {
    use super::{inventory, recorded_tests, tests_naming};
    use crate::test_record_store::{self as store, RunRow, SessionRow, WindowRow};
    use serde_json::json;
    use std::collections::BTreeSet;

    fn window(digest: &str, kind: &'static str, label: &str) -> WindowRow {
        WindowRow {
            digest: digest.to_owned(),
            kind,
            key: digest.to_owned(),
            opaque: None,
            deps: json!([]),
            lines: json!([[label, "oid", [1]]]),
        }
    }

    fn run(path: &str, windows: &[&str]) -> RunRow {
        RunRow {
            nodeid: format!("{path}::test"),
            path: path.to_owned(),
            outcome: "passed".to_owned(),
            opaque: None,
            seconds: 0.1,
            windows: windows.iter().map(|digest| (*digest).to_owned()).collect(),
        }
    }

    #[test]
    fn a_test_whose_newest_run_executed_a_changed_file_is_selected() {
        let root =
            std::env::temp_dir().join(format!("graph-recorded-tests-{}", std::process::id()));
        std::fs::create_dir_all(root.join("tests")).unwrap();
        let changed = ["pkg/m.py".to_owned()];
        assert!(recorded_tests(&root, &root, &changed).unwrap().is_empty());
        std::fs::write(
            root.join("pyproject.toml"),
            "[tool.forge.test_record]\nstore = \"record.sqlite\"\n",
        )
        .unwrap();
        for test in [
            "test_body",
            "test_fixture",
            "test_collect",
            "test_moved_on",
            "test_opened",
            "test_listed",
        ] {
            std::fs::write(root.join(format!("tests/{test}.py")), "").unwrap();
        }
        let session = SessionRow {
            started: 0.0,
            finished: 1.0,
            root: "/r".to_owned(),
            env_digest: "env".to_owned(),
            env_data: json!({}),
            argv: "[]".to_owned(),
            unstable: 0,
        };
        let windows = [
            window("body", "test", "pkg/m.py"),
            window("fixture", "fixture", "pkg/m.py"),
            window("collect", "collect", "pkg/m.py"),
            window("other", "test", "pkg/other.py"),
            WindowRow {
                deps: json!([["file", "pkg/m.py", "oid"]]),
                ..window("opened", "test", "pkg/other.py")
            },
            WindowRow {
                deps: json!([["list", "pkg/m.py", "digest"]]),
                ..window("listed", "test", "pkg/other.py")
            },
        ];
        let db = root.join("record.sqlite");
        store::write(
            &db,
            &session,
            &windows,
            &[
                run("tests/test_body.py", &["body"]),
                run("tests/test_fixture.py", &["fixture", "other"]),
                // Collection imports are not a use.
                run("tests/test_collect.py", &["collect", "other"]),
                run("tests/test_moved_on.py", &["body"]),
                // Opening a file is a use; listing its directory is not.
                run("tests/test_opened.py", &["opened"]),
                run("tests/test_listed.py", &["listed"]),
                // Deleted since it ran.
                run("tests/test_deleted.py", &["body"]),
            ],
        )
        .unwrap();
        // Only the newest run of a test counts.
        store::write(
            &db,
            &session,
            &[],
            &[run("tests/test_moved_on.py", &["other"])],
        )
        .unwrap();
        assert_eq!(
            recorded_tests(&root, &root, &changed).unwrap(),
            BTreeSet::from([
                "tests/test_body.py".to_owned(),
                "tests/test_fixture.py".to_owned(),
                "tests/test_opened.py".to_owned()
            ])
        );
        assert!(recorded_tests(&root, &root, &["pkg/none.py".to_owned()])
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_test_naming_a_changed_file_by_path_module_or_unique_name_is_selected() {
        let root = std::env::temp_dir().join(format!("graph-tests-naming-{}", std::process::id()));
        for (path, text) in [
            ("scripts/run.sh", ""),
            ("src/pkg/tool.py", ""),
            ("src/pkg/common.py", ""),
            ("other/common.py", ""),
            ("tests/test_path.py", "run(['bash', 'scripts/run.sh'])"),
            ("tests/test_module.py", "runpy.run_module('pkg.tool')"),
            ("tests/test_name.py", "ROOT / 'scripts' / 'run.sh'"),
            (".hooks/start.sh", ""),
            ("tests/test_hook.py", "ROOT / '.hooks' / 'start.sh'"),
            // A name two files carry, and names inside longer ones, are not a use.
            ("tests/test_shared.py", "'common.py'"),
            ("tests/test_longer.py", "'pkg.toolkit', 'scripts/run.shx'"),
        ] {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), text).unwrap();
        }
        let inventory = inventory(&root).unwrap();
        let changed = [
            "scripts/run.sh",
            "src/pkg/tool.py",
            "src/pkg/common.py",
            ".hooks/start.sh",
        ]
        .map(str::to_owned);
        assert_eq!(
            tests_naming(&root, &inventory, &changed).unwrap(),
            BTreeSet::from(
                [
                    "tests/test_hook.py",
                    "tests/test_module.py",
                    "tests/test_name.py",
                    "tests/test_path.py"
                ]
                .map(str::to_owned)
            )
        );
        assert!(tests_naming(&root, &inventory, &[]).unwrap().is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
