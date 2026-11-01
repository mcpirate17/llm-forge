//! Deterministic single-file edits. Preparation is read-only; commit is explicit.
#[path = "patch_diff.rs"]
mod diff;

use crate::source_file::{Snapshot, SourceRoot};
use anyhow::{ensure, Context, Result};
use conductor_native::syntax::{self, Language, MAX_SOURCE_BYTES};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub path: PathBuf,
    pub expected_sha256: String,
    pub edit: Edit,
    pub language: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Edit {
    Replace {
        search: String,
        replacement: String,
        #[serde(default = "one")]
        expected_matches: usize,
    },
    UnifiedDiff {
        patch: String,
        #[serde(default)]
        max_offset_lines: usize,
    },
}

fn one() -> usize {
    1
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub path: PathBuf,
    pub previous_sha256: String,
    pub candidate_sha256: String,
    pub previous_bytes: usize,
    pub candidate_bytes: usize,
    pub validation: &'static str,
    pub applied: bool,
    pub git_snapshot: Option<String>,
}

pub struct Plan {
    snapshot: Snapshot,
    candidate: String,
    validation: &'static str,
}

/// Restore only an exact retained snapshot, guarded by the current file hash.
/// Its original syntax may have been invalid; recovery promises original bytes.
pub fn prepare_restore(
    root: &SourceRoot,
    path: &std::path::Path,
    expected_sha256: &str,
    git_snapshot: &str,
) -> Result<Plan> {
    let snapshot = root.read(path)?;
    ensure!(snapshot.sha256 == expected_sha256, "source hash changed");
    let candidate = root.recovery_blob(git_snapshot)?;
    ensure!(
        candidate != snapshot.content,
        "recovery does not change source"
    );
    Ok(Plan {
        snapshot,
        candidate,
        validation: "exact_git_recovery",
    })
}

pub fn prepare(root: &SourceRoot, request: &Request) -> Result<Plan> {
    ensure!(
        request.expected_sha256.len() == 64
            && request
                .expected_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit()),
        "expected_sha256 must be a complete SHA-256 digest"
    );
    let snapshot = root.read(&request.path)?;
    ensure!(
        snapshot.sha256 == request.expected_sha256,
        "source hash changed"
    );
    let candidate = match &request.edit {
        Edit::Replace {
            search,
            replacement,
            expected_matches,
        } => replace(&snapshot.content, search, replacement, *expected_matches)?,
        Edit::UnifiedDiff {
            patch,
            max_offset_lines,
        } => diff::apply(&snapshot.content, patch, &request.path, *max_offset_lines)?,
    };
    ensure!(
        candidate.len() <= MAX_SOURCE_BYTES && !candidate.contains('\0'),
        "candidate size/NUL limit exceeded"
    );
    ensure!(
        candidate != snapshot.content,
        "patch does not change source"
    );
    let language = request
        .language
        .as_deref()
        .map(Language::from_name)
        .transpose()?
        .or_else(|| Language::from_path(&request.path));
    let validation = match language {
        Some(language) => {
            syntax::parse(&candidate, language).context("candidate syntax rejected")?;
            "native_ast"
        }
        None => match request.path.extension().and_then(|s| s.to_str()) {
            Some("json") => {
                serde_json::from_str::<serde_json::Value>(&candidate)
                    .context("candidate JSON rejected")?;
                "json"
            }
            Some("toml") => {
                toml::from_str::<toml::Value>(&candidate).context("candidate TOML rejected")?;
                "toml"
            }
            _ => "utf8_text",
        },
    };
    Ok(Plan {
        snapshot,
        candidate,
        validation,
    })
}

fn replace(source: &str, search: &str, replacement: &str, expected: usize) -> Result<String> {
    ensure!(
        !search.is_empty()
            && search.len() <= MAX_SOURCE_BYTES
            && replacement.len() <= MAX_SOURCE_BYTES,
        "replacement/search byte bounds exceeded or empty search"
    );
    ensure!(
        (1..=256).contains(&expected),
        "expected_matches must be 1..256"
    );
    let actual = source.match_indices(search).count();
    ensure!(
        actual == expected,
        "ambiguous/missing search: expected {expected} matches, found {actual}"
    );
    let length = source.len() - search.len() * actual + replacement.len() * actual;
    ensure!(
        length <= MAX_SOURCE_BYTES,
        "replacement exceeds source byte limit"
    );
    Ok(source.replace(search, replacement))
}

impl Plan {
    pub fn report(&self) -> Report {
        Report {
            path: self.snapshot.path.clone(),
            previous_sha256: self.snapshot.sha256.clone(),
            candidate_sha256: syntax::fingerprint(&self.candidate),
            previous_bytes: self.snapshot.content.len(),
            candidate_bytes: self.candidate.len(),
            validation: self.validation,
            applied: false,
            git_snapshot: None,
        }
    }

    pub fn apply(self, root: &SourceRoot) -> Result<Report> {
        let mut report = self.report();
        report.git_snapshot = Some(root.replace(&self.snapshot, &self.candidate)?);
        report.applied = true;
        Ok(report)
    }
}
