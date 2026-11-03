//! Read-only checkout drift shared by hygiene and checkout synchronization.
//!
//! `blocked_by` preserves the hygiene contract: intersection of tracked
//! changes and incoming names, not the actor's more precise conflict verdict.
use crate::workspace_integration::git;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;

/// The legacy report's remote, graph distance, and conservative dirty overlap.
pub fn report(root: &Path) -> Result<Value> {
    let remote = crate::workspace_integration::default_integration_ref(root, true)?;
    let behind: u64 = git(root, &["rev-list", "--count", &format!("HEAD..{remote}")])?
        .trim()
        .parse()
        .context("invalid behind count")?;
    let ahead: u64 = git(root, &["rev-list", "--count", &format!("{remote}..HEAD")])?
        .trim()
        .parse()
        .context("invalid ahead count")?;
    let incoming = git(root, &["diff", "--name-only", &format!("HEAD..{remote}")])?;
    let modified = git(root, &["diff", "--name-only", "HEAD"])?;
    let incoming: BTreeSet<&str> = incoming.lines().filter(|line| !line.is_empty()).collect();
    let modified: BTreeSet<&str> = modified.lines().filter(|line| !line.is_empty()).collect();
    let blocked: Vec<&str> = incoming.intersection(&modified).copied().collect();
    Ok(json!({"remote": remote, "behind": behind, "ahead": ahead,
        "fast_forwardable": behind > 0 && ahead == 0 && blocked.is_empty(),
        "blocked_by": blocked}))
}
