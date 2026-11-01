//! Read-only graph edges and bounded syntactic caller fallback.

use super::{GraphRelationship, GraphResult};
use std::collections::HashSet;
use std::path::Path;
#[path = "graph_context_syntactic.rs"]
mod syntactic;
pub(super) use syntactic::syntactic_callers;

fn read_graph(
    repo: &Path,
    file_path: &str,
    target: Option<&str>,
) -> Result<(Vec<GraphRelationship>, Vec<GraphRelationship>, String), String> {
    let store = super::store::GraphStore::open(repo, None).map_err(|error| error.to_string())?;
    let _operation = store.operation().map_err(|error| error.to_string())?;
    let mut projection = store
        .project(repo, file_path, target, 50)
        .map_err(|error| error.to_string())?;
    store
        .validate_projection(repo, file_path, &mut projection)
        .map_err(|error| error.to_string())?;
    store
        .verify_generation()
        .map_err(|error| error.to_string())?;
    let convert = |rows: Vec<super::store::Relationship>| {
        rows.into_iter()
            .map(|row| GraphRelationship {
                qualified_name: row.qualified_name,
                kind: row.kind,
                file_path: row.file_path,
            })
            .collect()
    };
    Ok((
        convert(projection.callers),
        convert(projection.callees),
        projection.graph_status,
    ))
}

pub(super) fn query(
    repo: &Path,
    file_path: &str,
    target: Option<&str>,
) -> Result<GraphResult, String> {
    let (mut callers, callees, status) = match read_graph(repo, file_path, target) {
        Ok((callers, callees, status)) => (callers, callees, status),
        Err(error) => (
            Vec::new(),
            Vec::new(),
            format!("unavailable (sqlite error: {error})"),
        ),
    };
    if let Some(symbol) = target {
        let mut seen: HashSet<String> = callers.iter().map(|row| row.file_path.clone()).collect();
        for candidate in syntactic_callers(repo, symbol, file_path)? {
            if seen.insert(candidate.file_path.clone()) {
                callers.push(candidate);
            }
        }
    }
    Ok(GraphResult {
        callers,
        callees,
        status,
    })
}
