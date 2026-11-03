//! Full native read-only hygiene report; distinct from the cheap session surface.
#[path = "workspace_hygiene_report_git.rs"]
mod facts;
#[path = "workspace_hygiene_render.rs"]
mod render;
use crate::{branch_policy_cli::exposure, workspace_hygiene_inputs as inputs};
use anyhow::{ensure, Context, Result};
use clap::Args;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
#[derive(Args)]
pub struct HygieneArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long)]
    live_ref: Option<String>,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    exit_nonzero_on_exposure: bool,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub live_ref: String,
    pub redundant_branches: Vec<Value>,
    pub stale_worktrees: Vec<String>,
    pub dirty_worktrees: Vec<Value>,
    pub landed_worktrees: Vec<Value>,
    pub landed_ref: String,
    pub expired_claims: Vec<Value>,
    pub idle_claims: Vec<Value>,
    pub unpushed_work: Vec<Value>,
    pub orphan_manifests: Vec<String>,
    pub dangling_manifests: Vec<String>,
    pub untracked_imports: Vec<Value>,
    pub stale_feature_branches: Vec<Value>,
    pub gh_available: bool,
    pub local_only_commit_exposure: Vec<Value>,
    pub stale_dirty_files: Vec<Value>,
    pub worktree_leases: Vec<Value>,
    pub checkout_drift: Value,
    /// Native addition: preserve optional PR query failures rather than hide them.
    pub gh_errors: Vec<Value>,
    #[serde(skip_serializing)]
    pub reap_preview: Vec<Value>,
}
impl Report {
    pub fn has_exposure(&self) -> bool {
        !self.stale_feature_branches.is_empty()
            || !self.local_only_commit_exposure.is_empty()
            || !self.stale_dirty_files.is_empty()
    }
    pub fn is_clean(&self) -> bool {
        self.redundant_branches.is_empty()
            && self.stale_worktrees.is_empty()
            && self.landed_worktrees.is_empty()
            && self.expired_claims.is_empty()
            && self.idle_claims.is_empty()
            && self.unpushed_work.is_empty()
            && self.orphan_manifests.is_empty()
            && self.dangling_manifests.is_empty()
            && self.untracked_imports.is_empty()
            && !self.has_exposure()
            && self
                .worktree_leases
                .iter()
                .all(|row| row["status"] == "leased")
            && self.checkout_drift["behind"].as_u64() == Some(0)
            && self.gh_errors.is_empty()
    }
}
fn rows(value: &Value, key: &str) -> Result<Vec<Value>> {
    value[key]
        .as_array()
        .cloned()
        .with_context(|| format!("exposure report missing {key}"))
}
pub fn build(root: &std::path::Path, live_ref: Option<&str>) -> Result<Report> {
    let source = crate::candidate_source::Source::open(root)?;
    let root = source.root();
    let live_ref = match live_ref {
        Some(value) => value.to_owned(),
        None => facts::git(root, &["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_owned(),
    };
    ensure!(
        !live_ref.is_empty() && live_ref != "HEAD",
        "detached HEAD: pass --live-ref explicitly"
    );
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
    let trees = facts::worktrees(root)?;
    let (stale_worktrees, dirty_worktrees) = facts::worktree_state(&trees)?;
    let landed_ref = crate::workspace_integration::default_integration_ref(root, true)?;
    let protected = facts::default_branch(root)?;
    let redundant_branches = facts::redundant(root, &live_ref, &protected, &dirty_worktrees)?;
    let landed_worktrees = facts::landed(root, &landed_ref, &trees)?;
    let (expired_claims, idle_claims) = inputs::claims(root, now, 90)?;
    let (orphan_manifests, dangling_manifests) = inputs::manifests(root)?;
    let exposed = exposure::data(root, "report", Some(&live_ref), 24.0)?;
    let tree_paths = trees
        .iter()
        .filter_map(|row| row.get("worktree").and_then(Value::as_str))
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    Ok(Report {
        live_ref,
        redundant_branches,
        stale_worktrees,
        dirty_worktrees,
        landed_worktrees,
        landed_ref,
        expired_claims,
        idle_claims,
        unpushed_work: facts::unpushed(root)?,
        orphan_manifests,
        dangling_manifests,
        untracked_imports: inputs::untracked_imports(root)?,
        stale_feature_branches: rows(&exposed, "stale_feature_branches")?,
        gh_available: exposed["gh_available"]
            .as_bool()
            .context("missing gh_available")?,
        local_only_commit_exposure: rows(&exposed, "local_only_commit_exposure")?,
        stale_dirty_files: rows(&exposed, "stale_dirty_files")?,
        worktree_leases: inputs::leases(root, &tree_paths)?,
        checkout_drift: crate::checkout_drift::report(root)?,
        gh_errors: rows(&exposed, "gh_errors")?,
        reap_preview: rows(&exposed, "reap_preview")?,
    })
}
pub fn run(args: HygieneArgs) -> Result<u8> {
    let report = build(&args.repo, args.live_ref.as_deref())?;
    let code = u8::from(args.exit_nonzero_on_exposure && report.has_exposure());
    if args.json {
        println!("{}", bounded_json(&report)?);
    } else {
        println!("{}", render::report(&report)?);
    }
    Ok(code)
}
fn bounded_json(report: &Report) -> Result<String> {
    struct Buffer(Vec<u8>);
    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self
                .0
                .len()
                .checked_add(bytes.len())
                .is_none_or(|n| n > 64 * 1024 * 1024)
            {
                return Err(std::io::Error::other("hygiene JSON exceeds64MiB"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Buffer(Vec::new());
    serde_json::to_writer_pretty(&mut output, report)?;
    Ok(String::from_utf8(output.0)?)
}

pub fn render(report: &Report) -> Result<String> {
    render::report(report)
}
#[cfg(test)]
#[path = "workspace_hygiene_report_tests.rs"]
mod tests;
