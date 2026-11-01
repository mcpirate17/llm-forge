//! Bounded v3 graph cache for hash-validated returned rows, not global completeness.
use super::{project_context, ContextOutput, GraphStore, SourceFile};
use anyhow::{ensure, Context, Result};
use conductor_native::graph_context::{files::SourceRoot, store::Projection};
use forge::graph_context_cache_storage as storage;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

const VERSION: u8 = 3;
const MAX_CACHE_ENTRIES: usize = 128;
const MAX_CACHE_BYTES: usize = 128 << 10;
const MAX_DEPENDENCIES: usize = 101;
#[derive(Clone, Copy)]
struct Bounds {
    edges: usize,
    depth: usize,
    nodes: usize,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CachedProjection {
    cache_version: u8,
    dependency_count: usize,
    dependencies: BTreeMap<String, String>,
    output: ContextOutput,
}
enum Check {
    Verified(String, String),
    Rejected(&'static str),
}
enum Admission {
    Accepted(Vec<String>),
    Rejected(&'static str),
}
fn eligible(status: &str) -> bool {
    matches!(status, "ok" | "indexed (no matching symbols)")
}
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn cache_key(
    store: &GraphStore,
    file: &SourceFile,
    symbol: Option<&str>,
    bounds: Bounds,
) -> String {
    let raw = format!(
        "{}:v3:{}:{}:{}:{symbol:?}:{}:{}:{}",
        env!("CARGO_PKG_VERSION"),
        store.generation,
        file.relative,
        file.digest,
        bounds.edges,
        bounds.depth,
        bounds.nodes
    );
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}
fn checked_hash(store: &GraphStore, sources: &SourceRoot, raw: &str) -> Result<Check> {
    let path = match sources.normalize(raw) {
        Ok(path) => path,
        Err(_) => return Ok(Check::Rejected("unverified (unsafe dependency path)")),
    };
    let path = path
        .to_str()
        .context("graph dependency path must be UTF-8")?;
    let Some(expected) = store.indexed_hash(sources.host(), path)? else {
        return Ok(Check::Rejected(
            "unverified (dependency indexed hash missing or conflicting)",
        ));
    };
    match sources.hash(path) {
        Ok(Some(actual)) if actual == expected => Ok(Check::Verified(path.to_owned(), actual)),
        Ok(_) => Ok(Check::Rejected(
            "stale (dependency source changed or disappeared)",
        )),
        Err(_) => Ok(Check::Rejected(
            "unverified (dependency source unsafe or unreadable)",
        )),
    }
}
/// Admit the entire validated returned rowset atomically. No rejected/removed peer
/// can become a dependency, and no subset can silently hide a proof-budget gap.
fn record_paths(
    store: &GraphStore,
    sources: &SourceRoot,
    dependencies: &mut BTreeMap<String, String>,
    raw_paths: Vec<String>,
) -> Result<Admission> {
    let mut next = dependencies.clone();
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    for raw in raw_paths {
        let relative = match sources.normalize(&raw) {
            Ok(path) => path,
            Err(_) => return Ok(Admission::Rejected("unverified (unsafe dependency path)")),
        };
        let path = relative
            .to_str()
            .context("graph dependency path must be UTF-8")?;
        if !seen.insert(path.to_owned()) {
            continue;
        }
        // The caller has just validated the returned rowset. Reuse this
        // operation's previously admitted dependency, then rehash every unique
        // dependency once more at the final boundary before cache publication.
        if next.contains_key(path) {
            order.push(path.to_owned());
            continue;
        }
        if next.len() >= MAX_DEPENDENCIES {
            return Ok(Admission::Rejected(
                "unverified (expansion dependency budget exceeded)",
            ));
        }
        match checked_hash(store, sources, path)? {
            Check::Rejected(status) => return Ok(Admission::Rejected(status)),
            Check::Verified(path, hash) => {
                order.push(path.clone());
                next.insert(path, hash);
            }
        }
    }
    *dependencies = next;
    Ok(Admission::Accepted(order))
}
fn dependency_status(
    store: &GraphStore,
    sources: &SourceRoot,
    dependencies: &BTreeMap<String, String>,
) -> Result<Option<&'static str>> {
    for (path, expected) in dependencies {
        match checked_hash(store, sources, path)? {
            Check::Verified(_, actual) if &actual == expected => {}
            Check::Verified(_, _) => {
                return Ok(Some("stale (dependency changed during retrieval)"))
            }
            Check::Rejected(status) => return Ok(Some(status)),
        }
    }
    Ok(None)
}
fn output_bounds(output: &ContextOutput, bounds: Bounds) -> bool {
    output.schema_version == 1
        && output.symbols.len() <= 40
        && output.callers.len() <= bounds.edges
        && output.callees.len() <= bounds.edges
        && output.callers.len() + output.callees.len() <= bounds.nodes
        && (bounds.depth != 0 || (output.callers.is_empty() && output.callees.is_empty()))
}
fn load(
    store: &GraphStore,
    host: &Path,
    sources: &SourceRoot,
    key: &str,
    file: &SourceFile,
    symbol: Option<&str>,
    bounds: Bounds,
) -> Result<Option<ContextOutput>> {
    let Some(bytes) = storage::read(host, key, MAX_CACHE_BYTES)? else {
        return Ok(None);
    };
    let text = std::str::from_utf8(&bytes).context("graph cache must be UTF-8")?;
    let value = forge::tool_schema::parse_document_with_limit(text, MAX_CACHE_BYTES)?;
    let cached: CachedProjection =
        serde_json::from_value(value).context("decode graph v3 cache")?;
    let output = &cached.output;
    if cached.cache_version != VERSION
        || !eligible(&output.graph_status)
        || output.generation != store.generation
        || output.file_path != file.relative
        || output.source_hash != file.digest
        || output.symbol.as_deref() != symbol
        || !output_bounds(output, bounds)
        || cached.dependencies.is_empty()
        || cached.dependencies.len() > MAX_DEPENDENCIES
        || cached.dependency_count != cached.dependencies.len()
        || cached.dependencies.get(&file.relative) != Some(&file.digest)
    {
        return Ok(None);
    }
    if serde_json::to_vec(output)?.len() > super::MAX_CONTEXT_OUTPUT_BYTES {
        return Ok(None);
    }
    for (path, hash) in &cached.dependencies {
        let normalized = sources.normalize(path)?;
        if normalized.to_str() != Some(path.as_str()) || !valid_hash(hash) {
            return Ok(None);
        }
    }
    for row in output.callers.iter().chain(&output.callees) {
        let normalized = sources.normalize(&row.file_path)?;
        if !cached.dependencies.contains_key(
            normalized
                .to_str()
                .context("graph endpoint must be UTF-8")?,
        ) {
            return Ok(None);
        }
    }
    if dependency_status(store, sources, &cached.dependencies)?.is_some() {
        return Ok(None);
    }
    let mut output = cached.output;
    output.cache_status = "hit".into();
    Ok(Some(output))
}
fn paths(output: &ContextOutput) -> Vec<String> {
    std::iter::once(output.file_path.clone())
        .chain(
            output
                .callers
                .iter()
                .chain(&output.callees)
                .map(|row| row.file_path.clone()),
        )
        .collect()
}
fn projection_paths(path: &str, projection: &Projection) -> Vec<String> {
    std::iter::once(path.to_owned())
        .chain(
            projection
                .callers
                .iter()
                .chain(&projection.callees)
                .map(|row| row.file_path.clone()),
        )
        .collect()
}
fn enqueue(
    paths: Vec<String>,
    level: usize,
    root: &str,
    queued: &mut BTreeSet<String>,
    queue: &mut VecDeque<(String, usize)>,
) {
    for path in paths {
        if path != root && queued.insert(path.clone()) {
            queue.push_back((path, level));
        }
    }
}
fn mark(output: &mut ContextOutput, status: &str) {
    if eligible(&output.graph_status) {
        output.graph_status = status.to_owned();
    }
}
fn reject_expansion(output: &mut ContextOutput, status: &str, omitted: usize) {
    mark(output, status);
    output.truncated = true;
    output.omitted_relationships_at_least += omitted.max(1);
}
fn nested_status(status: &str) -> &'static str {
    if status.starts_with("stale") {
        "stale (expanded dependency source not current)"
    } else if status.starts_with("unindexed") {
        "unindexed (expanded dependency lacks indexed evidence)"
    } else if status.starts_with("unavailable") {
        "unavailable (expanded graph unavailable)"
    } else {
        "unverified (expanded graph evidence not verified)"
    }
}
fn withhold(output: &mut ContextOutput, status: &str) {
    let removed = output.callers.len() + output.callees.len();
    output.graph_status = status.to_owned();
    output.callers.clear();
    output.callees.clear();
    output.truncated = true;
    output.omitted_relationships_at_least += removed;
}
fn add_rows(output: &mut ContextOutput, projection: Projection, edges: usize) {
    for (destination, rows) in [
        (&mut output.callers, projection.callers),
        (&mut output.callees, projection.callees),
    ] {
        for row in rows {
            if destination.iter().any(|prior| {
                prior.qualified_name == row.qualified_name
                    && prior.kind == row.kind
                    && prior.file_path == row.file_path
            }) {
                continue;
            }
            if destination.len() >= edges {
                output.omitted_relationships_at_least += 1;
                output.truncated = true;
            } else {
                destination.push(row);
            }
        }
    }
}
fn trim(output: &mut ContextOutput, bounds: Bounds) {
    if bounds.depth == 0 {
        output.callers.clear();
        output.callees.clear();
        return;
    }
    let excess = output.callers.len() + output.callees.len()
        - (output.callers.len() + output.callees.len()).min(bounds.nodes);
    for _ in 0..excess {
        if output.callees.pop().is_none() {
            output.callers.pop();
        }
    }
    output.omitted_relationships_at_least += excess;
    output.truncated |= excess > 0;
}
fn expand(
    store: &GraphStore,
    sources: &SourceRoot,
    output: &mut ContextOutput,
    bounds: Bounds,
    dependencies: &mut BTreeMap<String, String>,
) -> Result<()> {
    if !eligible(&output.graph_status) {
        return Ok(());
    }
    let admitted = match record_paths(store, sources, dependencies, paths(output))? {
        Admission::Accepted(paths) => paths,
        Admission::Rejected(status) => {
            withhold(output, status);
            return Ok(());
        }
    };
    let mut queue = VecDeque::new();
    let mut queued = BTreeSet::new();
    enqueue(admitted, 1, &output.file_path, &mut queued, &mut queue);
    let mut seen = BTreeSet::from([output.file_path.clone()]);
    while let Some((path, level)) = queue.pop_front() {
        if level >= bounds.depth || seen.contains(&path) {
            continue;
        }
        if seen.len() >= bounds.nodes {
            output.truncated = true;
            break;
        }
        seen.insert(path.clone());
        let mut projection = store.project(sources.host(), &path, None, bounds.edges)?;
        // Validate before reading/seeding any endpoint, including rows validation removes.
        store.validate_projection(sources.host(), &path, &mut projection)?;
        if !eligible(&projection.graph_status) {
            reject_expansion(
                output,
                nested_status(&projection.graph_status),
                projection.callers.len() + projection.callees.len(),
            );
            break;
        }
        let admitted = match record_paths(
            store,
            sources,
            dependencies,
            projection_paths(&path, &projection),
        )? {
            Admission::Accepted(paths) => paths,
            Admission::Rejected(status) => {
                reject_expansion(
                    output,
                    status,
                    projection.callers.len() + projection.callees.len(),
                );
                break;
            }
        };
        enqueue(
            admitted,
            level + 1,
            &output.file_path,
            &mut queued,
            &mut queue,
        );
        add_rows(output, projection, bounds.edges);
    }
    Ok(())
}
fn verify_source(sources: &SourceRoot, file: &SourceFile) -> Result<()> {
    ensure!(
        sources.hash(&file.relative)?.as_deref() == Some(file.digest.as_str()),
        "source changed during graph context retrieval; retry"
    );
    Ok(())
}
pub(super) fn context(
    store: &GraphStore,
    host: &Path,
    file: &SourceFile,
    symbol: Option<&str>,
    edges: usize,
    depth: usize,
    nodes: usize,
) -> Result<ContextOutput> {
    ensure!(
        (1..=50).contains(&edges) && depth <= 3 && (1..=100).contains(&nodes),
        "invalid graph cache context bounds"
    );
    let _operation = store.operation()?;
    let sources = SourceRoot::open(host)?;
    let bounds = Bounds {
        edges,
        depth,
        nodes,
    };
    let key = cache_key(store, file, symbol, bounds);
    if let Some(output) = load(store, sources.host(), &sources, &key, file, symbol, bounds)? {
        verify_source(&sources, file)?;
        store.verify_generation()?;
        return Ok(output);
    }
    let mut output = project_context(store, sources.host(), file, symbol, edges)?;
    let mut dependencies = BTreeMap::new();
    expand(store, &sources, &mut output, bounds, &mut dependencies)?;
    trim(&mut output, bounds);
    if !dependencies.is_empty() {
        if let Some(status) = dependency_status(store, &sources, &dependencies)? {
            withhold(&mut output, status);
        }
    }
    verify_source(&sources, file)?;
    store.verify_generation()?;
    if !eligible(&output.graph_status) {
        super::fit_context(&mut output, super::MAX_CONTEXT_OUTPUT_BYTES)?;
        return Ok(output);
    }
    super::fit_context(&mut output, super::MAX_CONTEXT_OUTPUT_BYTES)?;
    ensure!(
        !dependencies.is_empty() && dependencies.len() <= MAX_DEPENDENCIES,
        "verified graph cache dependency admission incomplete"
    );
    let cached = CachedProjection {
        cache_version: VERSION,
        dependency_count: dependencies.len(),
        dependencies,
        output,
    };
    let bytes = serde_json::to_vec(&cached)?;
    ensure!(
        bytes.len() <= MAX_CACHE_BYTES,
        "graph projection exceeds cache byte budget"
    );
    storage::publish(sources.host(), &key, &bytes, MAX_CACHE_ENTRIES)?;
    store.verify_generation()?;
    Ok(cached.output)
}
