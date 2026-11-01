//! Evidence for bounded returned graph rows, never global graph completeness.
use super::{valid_hash, GraphStore, Projection};
use crate::graph_context::files::SourceRoot;
use anyhow::{Context, Result};
use rusqlite::params;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

// Each source admits at most 1 MiB. Include the target in this aggregate bound.
const MAX_HASHED_FILES: usize = 128;

enum Evidence {
    Hash(String),
    Unknown(&'static str),
}

impl GraphStore {
    /// Missing, malformed or conflicting evidence cannot narrow test selection.
    pub fn indexed_hash(&self, host: &Path, relative: &str) -> Result<Option<String>> {
        let _operation = self.operation()?;
        let Some(conn) = &self.conn else {
            return Ok(None);
        };
        let snapshot = conn.unchecked_transaction()?;
        let sources = SourceRoot::open(host)?;
        let path = sources.normalize(relative)?;
        let relative = path.to_str().context("graph path must be UTF-8")?;
        let result = match self.evidence(sources.host(), relative)? {
            Evidence::Hash(hash) => Ok(Some(hash)),
            Evidence::Unknown(_) => Ok(None),
        };
        snapshot.commit()?;
        result
    }

    fn evidence(&self, host: &Path, relative: &str) -> Result<Evidence> {
        let Some(conn) = &self.conn else {
            return Ok(Evidence::Unknown("graph unavailable"));
        };
        let _operation = conn.operation()?;
        let absolute = host.join(relative);
        let absolute = absolute.to_str().context("graph path must be UTF-8")?;
        let mut found: Option<String> = None;
        for (table, available) in [("files", self.has_files), ("nodes", self.has_hash)] {
            if !available {
                continue;
            }
            // DISTINCT plus two values suffices to detect all disagreement;
            // NULL/invalid evidence remains visible, including mixed path aliases.
            let sql = format!("SELECT DISTINCT CASE WHEN length(CAST(file_hash AS BLOB))=64 THEN lower(file_hash) ELSE NULL END FROM {table} WHERE file_path IN (?1,?2) LIMIT 2");
            let mut statement = conn.prepare(&sql).context("query graph hash evidence")?;
            let rows = statement.query_map(params![absolute, relative], |row| {
                row.get::<_, Option<String>>(0)
            })?;
            for row in rows {
                let Some(hash) = row?.filter(|hash| valid_hash(hash)) else {
                    return Ok(Evidence::Unknown("missing or malformed indexed hash"));
                };
                if found.as_ref().is_some_and(|prior| prior != &hash) {
                    return Ok(Evidence::Unknown("conflicting indexed hashes"));
                }
                found = Some(hash);
            }
        }
        Ok(found.map_or(Evidence::Unknown("indexed hash missing"), Evidence::Hash))
    }

    /// Rehash only bounded returned endpoints. Later edits and undiscovered new
    /// callers remain possible; successful validation does not prove completeness.
    pub fn validate_projection(
        &self,
        host: &Path,
        relative: &str,
        projection: &mut Projection,
    ) -> Result<()> {
        let _operation = self.operation()?;
        let Some(conn) = &self.conn else {
            return Ok(());
        };
        let snapshot = conn.unchecked_transaction()?;
        self.validate_current(host, relative, projection)?;
        snapshot.commit()?;
        self.verify_generation()
    }

    fn validate_current(
        &self,
        host: &Path,
        relative: &str,
        projection: &mut Projection,
    ) -> Result<()> {
        let sources = SourceRoot::open(host)?;
        let target = sources.normalize(relative)?;
        let target = target.to_str().context("graph path must be UTF-8")?;
        let expected = match self.evidence(sources.host(), target)? {
            Evidence::Hash(hash) => hash,
            Evidence::Unknown(reason) => {
                if !projection.graph_status.starts_with("unindexed") {
                    projection.graph_status = format!("unverified ({reason})");
                }
                clear(projection);
                return Ok(());
            }
        };
        match sources.hash(target) {
            Ok(Some(actual)) if actual == expected => {}
            result => {
                projection.graph_status = match result {
                    Err(error) => format!("unverified (source read failed: {error:#})"),
                    _ => "stale (indexed source hash differs)".into(),
                };
                clear(projection);
                return Ok(());
            }
        }
        projection.indexed_hashes = vec![expected];
        let mut checked = BTreeMap::from([(target.to_owned(), None)]);
        let mut invalid = BTreeSet::new();
        let mut reason = None;
        for row in projection.callers.iter().chain(&projection.callees) {
            let verdict = self.endpoint(&sources, &row.file_path, &mut checked)?;
            if let Some(problem) = verdict {
                reason = Some(problem);
                invalid.insert(row.file_path.clone());
            }
        }
        projection
            .callers
            .retain(|row| !invalid.contains(&row.file_path));
        projection
            .callees
            .retain(|row| !invalid.contains(&row.file_path));
        projection.graph_status = match reason {
            Some(reason) => reason.to_owned(),
            None if projection.symbols.is_empty() => "indexed (no matching symbols)".into(),
            None => "ok".into(),
        };
        projection.relationship_limit_reached |= !invalid.is_empty();
        Ok(())
    }

    fn endpoint(
        &self,
        sources: &SourceRoot,
        raw: &str,
        checked: &mut BTreeMap<String, Option<&'static str>>,
    ) -> Result<Option<&'static str>> {
        let Ok(path) = sources.normalize(raw) else {
            return Ok(Some("unverified (unsafe relationship endpoint)"));
        };
        let path = path.to_str().context("graph endpoint must be UTF-8")?;
        if let Some(verdict) = checked.get(path) {
            return Ok(*verdict);
        }
        if checked.len() >= MAX_HASHED_FILES {
            return Ok(Some("unverified (relationship source byte budget)"));
        }
        let verdict = match self.evidence(sources.host(), path)? {
            Evidence::Unknown(_) => Some("unverified (relationship hash missing or conflicting)"),
            Evidence::Hash(expected) => match sources.hash(path) {
                Ok(Some(actual)) if actual == expected => None,
                Ok(_) => Some("stale (relationship endpoint differs)"),
                Err(_) => Some("unverified (relationship source unsafe or unreadable)"),
            },
        };
        checked.insert(path.to_owned(), verdict);
        Ok(verdict)
    }
}

fn clear(projection: &mut Projection) {
    projection.symbols.clear();
    projection.callers.clear();
    projection.callees.clear();
    projection.indexed_hashes.clear();
}

#[cfg(test)]
#[path = "graph_context_evidence_tests.rs"]
mod tests;
