//! Bounded disk projections bound to graph generation and current source hashes.

use super::{project_context, ContextOutput, GraphStore, SourceFile};
use anyhow::{ensure, Context, Result};
use conductor_native::graph_context::store::current_hash;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::path::Path;

const MAX_CACHE_ENTRIES: usize = 128;
const MAX_CACHE_BYTES: u64 = 128 << 10;

#[derive(Serialize, Deserialize)]
struct CachedProjection {
    dependencies: BTreeMap<String, Option<String>>,
    output: ContextOutput,
}

fn cache_key(
    store: &GraphStore,
    file: &SourceFile,
    symbol: Option<&str>,
    edges: usize,
    depth: usize,
    nodes: usize,
) -> String {
    let raw = format!(
        "{}:v2:{}:{}:{}:{symbol:?}:{edges}:{depth}:{nodes}",
        env!("CARGO_PKG_VERSION"),
        store.generation,
        file.relative,
        file.digest
    );
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}

fn load(path: &Path, host: &Path) -> Result<Option<ContextOutput>> {
    if !path.is_file() {
        return Ok(None);
    }
    ensure!(
        path.metadata()?.len() <= MAX_CACHE_BYTES,
        "graph projection cache exceeds byte budget"
    );
    let cached: CachedProjection =
        serde_json::from_slice(&std::fs::read(path)?).context("decode graph projection cache")?;
    if cached.dependencies.len() > 101 {
        anyhow::bail!("graph projection dependency cache exceeds node cap");
    }
    for (path, expected) in &cached.dependencies {
        if &current_hash(host, path)? != expected {
            return Ok(None);
        }
    }
    let mut output = cached.output;
    output.cache_status = "hit".to_owned();
    Ok(Some(output))
}

fn expand(
    store: &GraphStore,
    host: &Path,
    output: &mut ContextOutput,
    depth: usize,
    nodes: usize,
    edges: usize,
) -> Result<BTreeMap<String, Option<String>>> {
    let mut dependencies =
        BTreeMap::from([(output.file_path.clone(), Some(output.source_hash.clone()))]);
    let initial = store.project(host, &output.file_path, output.symbol.as_deref(), edges)?;
    let mut queue = VecDeque::new();
    for row in initial.callers.iter().chain(&initial.callees) {
        dependencies.insert(row.file_path.clone(), current_hash(host, &row.file_path)?);
    }
    for row in output.callers.iter().chain(&output.callees) {
        queue.push_back((row.file_path.clone(), 1));
    }
    let mut seen = HashSet::from([output.file_path.clone()]);
    while let Some((path, level)) = queue.pop_front() {
        if !seen.insert(path.clone()) || level >= depth {
            continue;
        }
        if seen.len() > nodes || dependencies.len() >= 101 {
            output.truncated = true;
            break;
        }
        let mut projection = store.project(host, &path, None, edges)?;
        for row in projection.callers.iter().chain(&projection.callees) {
            if dependencies.len() < 101 {
                dependencies.insert(row.file_path.clone(), current_hash(host, &row.file_path)?);
            }
        }
        store.validate_projection(host, &path, &mut projection)?;
        for (destination, rows) in [
            (&mut output.callers, projection.callers),
            (&mut output.callees, projection.callees),
        ] {
            for row in rows {
                queue.push_back((row.file_path.clone(), level + 1));
                if !destination.iter().any(|prior| {
                    prior.qualified_name == row.qualified_name && prior.kind == row.kind
                }) {
                    destination.push(row);
                }
            }
        }
    }
    if depth == 0 {
        output.callers.clear();
        output.callees.clear();
    }
    let edge_excess =
        output.callers.len().saturating_sub(edges) + output.callees.len().saturating_sub(edges);
    output.callers.truncate(edges);
    output.callees.truncate(edges);
    let excess = (output.callers.len() + output.callees.len()).saturating_sub(nodes);
    for _ in 0..excess {
        if output.callees.pop().is_none() {
            output.callers.pop();
        }
    }
    output.omitted_relationships_at_least += excess + edge_excess;
    output.truncated |= excess + edge_excess > 0;
    Ok(dependencies)
}

fn publish(directory: &Path, key: &str, projection: &CachedProjection) -> Result<()> {
    std::fs::create_dir_all(directory)?;
    ensure!(
        directory
            .canonicalize()?
            .starts_with(directory.parent().unwrap().parent().unwrap()),
        "graph cache directory escapes host"
    );
    let bytes = serde_json::to_vec(projection)?;
    ensure!(
        bytes.len() as u64 <= MAX_CACHE_BYTES,
        "graph projection exceeds cache byte budget"
    );
    let mut entries = std::fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.retain(|entry| {
        entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "json")
    });
    entries.sort_by_key(|entry| entry.metadata().and_then(|stat| stat.modified()).ok());
    let remove = entries.len().saturating_sub(MAX_CACHE_ENTRIES - 1);
    for entry in entries.into_iter().take(remove) {
        match std::fs::remove_file(entry.path()) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("prune graph projection cache"),
        }
    }
    let temporary = directory.join(format!(".{key}-{}.tmp", std::process::id()));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(temporary, directory.join(format!("{key}.json")))?;
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
    let key = cache_key(store, file, symbol, edges, depth, nodes);
    let directory = host.join(".forge/context-cache");
    if let Some(output) = load(&directory.join(format!("{key}.json")), host)? {
        return Ok(output);
    }
    let mut output = project_context(store, host, file, symbol, edges)?;
    let dependencies = expand(store, host, &mut output, depth, nodes, edges)?;
    super::fit_context(&mut output, super::MAX_CONTEXT_OUTPUT_BYTES)?;
    let cached = CachedProjection {
        dependencies,
        output,
    };
    publish(&directory, &key, &cached)?;
    Ok(cached.output)
}
