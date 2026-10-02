//! Forge-owned syntax graph index. A complete SQLite snapshot is built beside
//! the destination and atomically replaced only after every source parses.

use anyhow::{ensure, Context, Result};
use conductor_native::graph_index::{extract_python, extract_rust, Call, Definition, FileFacts};
use rusqlite::{params, Connection};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use walkdir::{DirEntry, WalkDir};

#[path = "graph_index_cache.rs"]
mod cache;

pub const DEFAULT_DB: &str = ".forge/graph.db";
const MAX_SOURCE_BYTES: u64 = 1 << 20;
const MAX_TOTAL_SOURCE_BYTES: usize = 128 << 20;
const MAX_FILES: usize = 30_000;
const MAX_DEFINITIONS: usize = 100_000;
const MAX_CALLS: usize = 1_000_000;

struct IndexedFile {
    path: String,
    hash: String,
    language: &'static str,
    facts: FileFacts,
}

#[derive(Serialize)]
pub struct IndexReport {
    schema_version: u8,
    authority: &'static str,
    database: String,
    files: usize,
    definitions: usize,
    resolved_calls: usize,
    unresolved_calls: usize,
    dynamic_calls: usize,
    embeddings: &'static str,
    parsed_files: usize,
    reused_files: usize,
    elapsed_ms: u128,
}

fn is_source(entry: &DirEntry) -> bool {
    matches!(
        entry.path().extension().and_then(|s| s.to_str()),
        Some("py" | "rs")
    )
}

fn may_enter(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return true;
    }
    let name = entry.file_name().to_string_lossy();
    !name.starts_with('.')
        && !matches!(
            name.as_ref(),
            "target" | "node_modules" | "__pycache__" | ".venv" | "venv" | "build" | "dist"
        )
}

fn scan(host: &Path, cached: &cache::FactCache) -> Result<(Vec<IndexedFile>, usize)> {
    let mut files = Vec::new();
    let mut total_bytes = 0usize;
    let mut total_definitions = 0usize;
    let mut total_calls = 0usize;
    let mut parsed_files = 0usize;
    for entry in WalkDir::new(host)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(may_enter)
    {
        let entry = entry.with_context(|| format!("walk source tree: {}", host.display()))?;
        if !entry.file_type().is_file() || !is_source(&entry) {
            continue;
        }
        ensure!(
            files.len() < MAX_FILES,
            "source file limit exceeded ({MAX_FILES})"
        );
        let path = entry.path();
        let relative = path
            .strip_prefix(host)?
            .to_string_lossy()
            .replace('\\', "/");
        ensure!(
            relative.len() <= 400 && !relative.contains('\0'),
            "indexed path exceeds 400 bytes or contains NUL: {relative}"
        );
        let metadata = entry.metadata()?;
        ensure!(
            metadata.len() <= MAX_SOURCE_BYTES,
            "source exceeds {MAX_SOURCE_BYTES} bytes: {relative}"
        );
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        File::open(path)
            .with_context(|| format!("open source: {relative}"))?
            .take(MAX_SOURCE_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("read source: {relative}"))?;
        ensure!(
            bytes.len() as u64 <= MAX_SOURCE_BYTES && !bytes.contains(&0),
            "source grew too large or contains NUL: {relative}"
        );
        total_bytes += bytes.len();
        ensure!(
            total_bytes <= MAX_TOTAL_SOURCE_BYTES,
            "source byte budget exceeded ({MAX_TOTAL_SOURCE_BYTES}): {relative}"
        );
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let source =
            String::from_utf8(bytes).with_context(|| format!("source is not UTF-8: {relative}"))?;
        let language = if relative.ends_with(".py") {
            "python"
        } else {
            "rust"
        };
        let facts = match cached.get(&relative) {
            Some((previous, facts)) if previous == &hash => facts.clone(),
            _ => {
                parsed_files += 1;
                if language == "python" {
                    extract_python(&source)
                } else {
                    extract_rust(&source)
                }
                .with_context(|| format!("parse source: {relative}"))?
            }
        };
        total_definitions += facts.definitions.len();
        total_calls += facts.calls.len();
        ensure!(
            total_definitions <= MAX_DEFINITIONS && total_calls <= MAX_CALLS,
            "syntax fact budget exceeded (definitions {MAX_DEFINITIONS}, calls {MAX_CALLS}): {relative}"
        );
        files.push(IndexedFile {
            path: relative,
            hash,
            language,
            facts,
        });
    }
    Ok((files, parsed_files))
}

type Definitions = BTreeMap<(String, String), String>;

fn symbol_id(path: &str, qualified: &str, count: usize) -> String {
    if count == 0 {
        format!("{path}::{qualified}")
    } else {
        format!("{path}::{qualified}#{count}")
    }
}

fn definitions(files: &[IndexedFile]) -> (Definitions, Vec<(String, Definition, String)>) {
    let mut ids = Definitions::new();
    let mut rows = Vec::new();
    let mut seen = BTreeMap::<(String, String), usize>::new();
    for file in files {
        for definition in &file.facts.definitions {
            let key = (file.path.clone(), definition.qualified.clone());
            let count = seen.entry(key.clone()).or_default();
            let id = symbol_id(&file.path, &definition.qualified, *count);
            *count += 1;
            // Duplicate definitions are inherently ambiguous for static call
            // resolution. Keep both nodes, but remove the ambiguous lookup.
            if *count == 1 {
                ids.insert(key, id.clone());
            } else {
                ids.remove(&key);
            }
            rows.push((file.path.clone(), definition.clone(), id));
        }
    }
    (ids, rows)
}

fn lexical_target(path: &str, call: &Call, ids: &Definitions, separator: &str) -> Option<String> {
    let mut scope = call.caller.as_str();
    let target = if let Some(method) = call.target.strip_prefix(&format!("self{separator}")) {
        scope = scope
            .rsplit_once(separator)
            .map_or("", |(parent, _)| parent);
        method
    } else {
        call.target.as_str()
    };
    loop {
        let qualified = if scope.is_empty() {
            target.to_owned()
        } else {
            format!("{scope}{separator}{target}")
        };
        if let Some(id) = ids.get(&(path.to_owned(), qualified)) {
            return Some(id.clone());
        }
        let Some((parent, _)) = scope.rsplit_once(separator) else {
            break;
        };
        scope = parent;
    }
    ids.get(&(path.to_owned(), target.to_owned())).cloned()
}

type ModuleLookup = BTreeMap<(String, String), Option<String>>;

fn module_lookup(files: &[IndexedFile]) -> ModuleLookup {
    let mut lookup = BTreeMap::new();
    for file in files {
        let mut suffix = file.path.as_str();
        loop {
            let key = (file.language.to_owned(), suffix.to_owned());
            lookup
                .entry(key)
                .and_modify(|slot| *slot = None)
                .or_insert_with(|| Some(file.path.clone()));
            let Some((_, rest)) = suffix.split_once('/') else {
                break;
            };
            suffix = rest;
        }
    }
    lookup
}

fn unique_module(lookup: &ModuleLookup, language: &str, candidates: &[String]) -> Option<String> {
    let mut found = None;
    for candidate in candidates {
        match lookup.get(&(language.to_owned(), candidate.clone())) {
            Some(Some(path)) if found.is_none() => found = Some(path.clone()),
            Some(_) => return None,
            None => {}
        }
    }
    found
}

fn python_module_file(lookup: &ModuleLookup, from: &str, module: &str) -> Option<String> {
    let relative_level = module.bytes().take_while(|b| *b == b'.').count();
    let rest = &module[relative_level..];
    let dotted = if relative_level == 0 {
        rest.replace('.', "/")
    } else {
        let parent = Path::new(from).parent()?;
        let mut base = parent.to_path_buf();
        for _ in 1..relative_level {
            base = base.parent()?.to_path_buf();
        }
        base.join(rest.replace('.', "/"))
            .to_string_lossy()
            .replace('\\', "/")
    };
    let candidates = [format!("{dotted}.py"), format!("{dotted}/__init__.py")];
    unique_module(lookup, "python", &candidates)
}

fn imported_python_target(
    file: &IndexedFile,
    call: &Call,
    lookup: &ModuleLookup,
    ids: &Definitions,
) -> Option<String> {
    for import in &file.facts.imports {
        let remaining = if call.target == import.alias {
            ""
        } else if let Some(rest) = call.target.strip_prefix(&format!("{}.", import.alias)) {
            rest
        } else {
            continue;
        };
        let Some(module_file) = python_module_file(lookup, &file.path, &import.module) else {
            continue;
        };
        let qualified = match &import.member {
            Some(member) if remaining.is_empty() => member.clone(),
            Some(member) => format!("{member}.{remaining}"),
            None if !remaining.is_empty() => remaining.to_owned(),
            None => continue,
        };
        if let Some(id) = ids.get(&(module_file, qualified)) {
            return Some(id.clone());
        }
    }
    None
}

fn explicit_rust_target(call: &Call, lookup: &ModuleLookup, ids: &Definitions) -> Option<String> {
    let target = call.target.strip_prefix("crate::")?;
    let (module, name) = target.rsplit_once("::")?;
    let module = module.replace("::", "/");
    let candidates = [format!("{module}.rs"), format!("{module}/mod.rs")];
    let path = unique_module(lookup, "rust", &candidates)?;
    ids.get(&(path, name.to_owned())).cloned()
}

fn resolve(
    file: &IndexedFile,
    call: &Call,
    lookup: &ModuleLookup,
    ids: &Definitions,
) -> Option<String> {
    let separator = if file.language == "python" { "." } else { "::" };
    lexical_target(&file.path, call, ids, separator).or_else(|| {
        if file.language == "python" {
            imported_python_target(file, call, lookup, ids)
        } else {
            explicit_rust_target(call, lookup, ids)
        }
    })
}

fn write_snapshot(conn: &mut Connection, files: &[IndexedFile]) -> Result<(usize, usize)> {
    conn.execute_batch(
        "PRAGMA foreign_keys=ON;
         CREATE TABLE files (
           file_path TEXT PRIMARY KEY, file_hash TEXT NOT NULL, language TEXT NOT NULL, facts_json TEXT NOT NULL, unresolved INTEGER NOT NULL);
         CREATE TABLE nodes (
           qualified_name TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL,
           file_path TEXT NOT NULL REFERENCES files(file_path),
           line_start INTEGER NOT NULL, line_end INTEGER NOT NULL,
           signature TEXT, file_hash TEXT NOT NULL, is_test INTEGER NOT NULL);
         CREATE TABLE edges (
           source_qualified TEXT NOT NULL REFERENCES nodes(qualified_name),
           target_qualified TEXT NOT NULL REFERENCES nodes(qualified_name),
           kind TEXT NOT NULL, line INTEGER NOT NULL,
           PRIMARY KEY(source_qualified,target_qualified,kind,line));
         CREATE TABLE index_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE INDEX nodes_file_path ON nodes(file_path);
         CREATE INDEX edges_source ON edges(source_qualified);
         CREATE INDEX edges_target ON edges(target_qualified);",
    )?;
    let (ids, definitions) = definitions(files);
    let lookup = module_lookup(files);
    let hashes = files
        .iter()
        .map(|file| (file.path.as_str(), file.hash.as_str()))
        .collect::<BTreeMap<_, _>>();
    let tx = conn.transaction()?;
    write_files(&tx, files)?;
    for (path, definition, id) in &definitions {
        let hash = hashes
            .get(path.as_str())
            .context("definition references missing file")?;
        tx.execute(
            "INSERT INTO nodes VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                id,
                definition.name,
                definition.kind,
                path,
                definition.line_start as i64,
                definition.line_end as i64,
                definition.signature,
                hash,
                is_test_path(path)
                    || definition.name.starts_with("test_")
                    || definition.kind == "Test"
            ],
        )?;
    }
    let (resolved, unresolved, unresolved_files) = write_call_edges(&tx, files, &ids, &lookup)?;
    write_metadata(&tx, files, resolved, unresolved, &unresolved_files)?;
    tx.commit()?;
    Ok((resolved, unresolved))
}

fn write_files(tx: &rusqlite::Transaction<'_>, files: &[IndexedFile]) -> Result<()> {
    for file in files {
        tx.execute(
            "INSERT INTO files VALUES (?1,?2,?3,?4,?5)",
            params![
                file.path,
                file.hash,
                file.language,
                serde_json::to_string(&file.facts)?,
                file.facts.dynamic_calls > 0
            ],
        )?;
        tx.execute(
            "INSERT INTO nodes VALUES (?1,?2,'File',?2,1,1,NULL,?3,?4)",
            params![
                format!("{}::<module>", file.path),
                file.path,
                file.hash,
                is_test_path(&file.path)
            ],
        )?;
    }
    Ok(())
}

fn write_call_edges(
    tx: &rusqlite::Transaction<'_>,
    files: &[IndexedFile],
    ids: &Definitions,
    lookup: &ModuleLookup,
) -> Result<(usize, usize, BTreeSet<String>)> {
    let mut resolved = 0;
    let mut unresolved = 0;
    let mut distinct = BTreeSet::new();
    let mut unresolved_files = BTreeSet::new();
    for file in files {
        for call in &file.facts.calls {
            let source = ids.get(&(file.path.clone(), call.caller.clone()));
            let target = resolve(file, call, lookup, ids);
            if let (Some(source), Some(target)) = (source, target) {
                if distinct.insert((source.clone(), target.clone(), call.line)) {
                    tx.execute(
                        "INSERT INTO edges VALUES (?1,?2,'CALLS',?3)",
                        params![source, target, call.line as i64],
                    )?;
                    resolved += 1;
                }
            } else {
                unresolved += 1;
                unresolved_files.insert(file.path.clone());
            }
        }
        if file.facts.dynamic_calls > 0 {
            unresolved_files.insert(file.path.clone());
        }
        if unresolved_files.contains(&file.path) {
            tx.execute(
                "UPDATE files SET unresolved=1 WHERE file_path=?1",
                [&file.path],
            )?;
        }
        write_import_edges(tx, file, lookup)?;
    }
    Ok((resolved, unresolved, unresolved_files))
}

struct TempDb(PathBuf);

fn is_test_path(path: &str) -> bool {
    path.split('/').any(|part| part == "tests")
        || Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("test_") || name.ends_with("_test.py"))
}

fn write_import_edges(
    tx: &rusqlite::Transaction<'_>,
    file: &IndexedFile,
    lookup: &ModuleLookup,
) -> Result<()> {
    for import in &file.facts.imports {
        if let Some(target) = python_module_file(lookup, &file.path, &import.module) {
            tx.execute(
                "INSERT OR IGNORE INTO edges VALUES (?1,?2,'IMPORTS',0)",
                params![
                    format!("{}::<module>", file.path),
                    format!("{target}::<module>")
                ],
            )?;
        }
    }
    Ok(())
}

fn write_metadata(
    tx: &rusqlite::Transaction<'_>,
    files: &[IndexedFile],
    resolved: usize,
    unresolved: usize,
    unresolved_files: &BTreeSet<String>,
) -> Result<()> {
    let mut digest = Sha256::new();
    digest.update(cache::PARSER_VERSION);
    for file in files {
        digest.update(&file.path);
        digest.update(&file.hash);
    }
    let entries = [
        ("schema_version", "3".to_owned()),
        ("parser_version", cache::PARSER_VERSION.to_owned()),
        ("content_generation", format!("{:x}", digest.finalize())),
        ("coverage_complete", unresolved_files.is_empty().to_string()),
        ("unresolved_files_count", unresolved_files.len().to_string()),
        (
            "unresolved_files_sha256",
            format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(unresolved_files)?)
            ),
        ),
        ("resolved_calls", resolved.to_string()),
        ("unresolved_calls", unresolved.to_string()),
        (
            "dynamic_calls",
            files
                .iter()
                .map(|file| file.facts.dynamic_calls)
                .sum::<usize>()
                .to_string(),
        ),
    ];
    for (key, value) in entries {
        tx.execute("INSERT INTO index_meta VALUES (?1,?2)", params![key, value])?;
    }
    Ok(())
}

fn git_head(host: &Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(host)
        .output()
        .context("read graph build revision")?;
    if output.status.success() {
        return Ok(String::from_utf8(output.stdout)?.trim().to_owned());
    }
    ensure!(
        output.status.code() == Some(128),
        "git rev-parse failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::new())
}

fn existing_counts(db: &Path, head: &str) -> Result<Option<(usize, usize)>> {
    let conn = Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut statement = conn.prepare("SELECT key,value FROM index_meta")?;
    let metadata = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<BTreeMap<String, String>>>()?;
    if metadata.get("git_head_sha").map(String::as_str) != Some(head) {
        return Ok(None);
    }
    let number = |key: &str| -> Result<usize> {
        metadata
            .get(key)
            .with_context(|| format!("cached graph missing {key}"))?
            .parse()
            .context("invalid cached graph call count")
    };
    Ok(Some((
        number("resolved_calls")?,
        number("unresolved_calls")?,
    )))
}

fn report(
    db: &Path,
    files: &[IndexedFile],
    parsed_files: usize,
    counts: (usize, usize),
    started: std::time::Instant,
) -> IndexReport {
    IndexReport {
        schema_version: 3,
        authority: "forge-native-structural-graph",
        database: db.display().to_string(),
        files: files.len(),
        definitions: files.iter().map(|file| file.facts.definitions.len()).sum(),
        resolved_calls: counts.0,
        unresolved_calls: counts.1,
        dynamic_calls: files.iter().map(|file| file.facts.dynamic_calls).sum(),
        embeddings: "absent",
        parsed_files,
        reused_files: files.len() - parsed_files,
        elapsed_ms: started.elapsed().as_millis(),
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn index(host: &Path, explicit_db: Option<&Path>) -> Result<IndexReport> {
    let started = std::time::Instant::now();
    let db = explicit_db
        .map(|path| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                host.join(path)
            }
        })
        .unwrap_or_else(|| host.join(DEFAULT_DB));
    let previous = cache::load(&db)?;
    let (files, parsed_files) = scan(host, &previous)?;
    let head = git_head(host)?;
    if parsed_files == 0 && files.len() == previous.len() && db.is_file() {
        if let Some(counts) = existing_counts(&db, &head)? {
            return Ok(report(&db, &files, 0, counts, started));
        }
    }
    let parent = db
        .parent()
        .context("graph database has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create graph directory: {}", parent.display()))?;
    if explicit_db.is_none() {
        ensure!(
            parent.canonicalize()?.starts_with(host),
            "default graph directory resolves outside host: {}",
            parent.display()
        );
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let temporary = TempDb(parent.join(format!(".graph-{}-{sequence}.tmp", std::process::id())));
    ensure!(
        !temporary.0.exists(),
        "temporary graph database already exists: {}",
        temporary.0.display()
    );
    let mut conn = Connection::open(&temporary.0)
        .with_context(|| format!("create graph snapshot: {}", temporary.0.display()))?;
    let (resolved_calls, unresolved_calls) = write_snapshot(&mut conn, &files)?;
    conn.execute("INSERT INTO index_meta VALUES ('git_head_sha',?1)", [head])?;
    conn.close().map_err(|(_, error)| error)?;
    fs::rename(&temporary.0, &db)
        .with_context(|| format!("publish graph snapshot: {}", db.display()))?;
    Ok(report(
        &db,
        &files,
        parsed_files,
        (resolved_calls, unresolved_calls),
        started,
    ))
}
