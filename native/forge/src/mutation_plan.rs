//! `forge mutation plan` / `forge mutation write`: the native campaign writer.
//!
//! Both start zero interpreters. Scope is this branch's changed files bounded by
//! the lane's live claims, an exact `--only` list, or an explicit `--all-files`
//! maintenance sweep; `conductor_native::mutation_plan::compute_plan` does the
//! pairing and manifest emission, and `mutation_write` publishes the result.

use anyhow::{Context, Result};
use clap::Args;
use conductor_native::mutation_plan::{compute_plan, PlanRequest, PriorityOptions};
use conductor_native::project_paths;
use serde_json::{json, Map, Value};
use std::env;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::civil::civil_from_days;
use crate::identity;
use forge::{mutation_scope, mutation_write};

/// `[tool.conductor]`-resolved path order: see `project_paths::resolve`.
const REGISTRY_INDEX: usize = 1;

#[derive(Args)]
pub struct PriorityArgs {
    /// Rank only changed production/validation sources in this policy's [risk].high.
    #[arg(long)]
    pub priority_policy: Option<String>,
    /// Priority diff base; defaults to `--base`.
    #[arg(long, requires = "priority_policy")]
    pub priority_base: Option<String>,
    /// Optional unverified caller-supplied hints bound to tree and source/test hashes.
    #[arg(long, requires = "priority_policy")]
    pub priority_telemetry: Option<String>,
    /// Select using supplied cost hints; this is an estimate, not an execution cap.
    #[arg(long, requires = "priority_policy")]
    pub priority_budget_seconds: Option<f64>,
}

#[derive(Args)]
pub struct ScopeArgs {
    #[command(flatten)]
    pub priority: Box<PriorityArgs>,
    /// "python" or "rust"
    pub language: String,

    /// Repository root; defaults to the nearest ancestor of the current
    /// directory that contains a `.git` entry.
    #[arg(long)]
    pub repo_root: Option<PathBuf>,

    /// Lane that owns the dirty scope and prefixes generated campaign IDs;
    /// defaults to this process's resolved governance identity.
    #[arg(long)]
    pub owner: Option<String>,

    /// `YYYYMMDD`; defaults to today (UTC).
    #[arg(long)]
    pub day: Option<String>,

    /// Rust workers per crate.
    #[arg(long, default_value_t = 4)]
    pub jobs: i64,

    /// Positive Python workers (default 1).
    #[arg(long = "python-jobs")]
    pub python_jobs: Option<i64>,

    #[arg(long = "run-timeout", default_value_t = 1800)]
    pub run_timeout: i64,

    /// Repository-relative directory campaign manifests are registered
    /// under; defaults to `[tool.conductor].mutation_registry`'s parent
    /// (`pyproject.toml`), else `conductor/mutation_campaigns`.
    #[arg(long)]
    pub campaigns_root: Option<String>,

    /// Exact repository-relative source to plan; repeatable. Omit for the
    /// files this branch changed.
    #[arg(long = "only")]
    pub only: Vec<String>,

    /// Branch base for the changed-file scope.
    #[arg(long, default_value = "origin/main")]
    pub base: String,

    /// Maintenance sweep of the WHOLE TREE. The default, and the only thing an
    /// agent landing a change should run, is the files this branch changed.
    #[arg(long = "all-files", conflicts_with = "only")]
    pub all_files: bool,

    /// Also plan sources an existing campaign already names.
    #[arg(long = "include-covered")]
    pub include_covered: bool,

    /// `SOURCE=TEST`; also score SOURCE with TEST when no test is named
    /// after SOURCE. Repeatable. Python subjects only.
    #[arg(long = "extra-test", value_name = "SOURCE=TEST")]
    pub extra_test: Vec<String>,
}

#[derive(Args)]
pub struct PlanArgs {
    #[command(flatten)]
    pub scope: ScopeArgs,
    /// Also list campaign IDs, full manifests and unpaired subjects.
    #[arg(long)]
    pub verbose: bool,
}

#[derive(Args)]
pub struct WriteArgs {
    #[command(flatten)]
    pub scope: ScopeArgs,
    /// Replace an existing manifest, recorded survivor baseline included.
    #[arg(long)]
    pub force: bool,
}

/// One resolved plan: the planner's result plus how its scope was selected.
struct Planned {
    result: Value,
    scope: String,
    repo_root: PathBuf,
    registry: String,
    campaigns: String,
}

pub(crate) fn resolve_repo_root(explicit: Option<PathBuf>) -> Result<PathBuf> {
    match explicit {
        Some(path) => Ok(path),
        None => find_repo_root(&env::current_dir().context("reading the current directory")?)
            .context("no .git found above the current directory; pass --repo-root"),
    }
}

pub(crate) fn resolve_lane(owner: Option<String>, repo_root: &Path) -> Result<String, String> {
    match owner {
        Some(owner) => Ok(owner),
        None => identity::resolve_owner(Some(repo_root), &env::vars().collect())
            .map_err(|error| format!("cannot resolve campaign owner: {}", error.0)),
    }
}

/// Whole-tree maintenance, exact files, or this lane's changed files.
fn select_scope(
    args: &ScopeArgs,
    repo_root: &Path,
    owner: &str,
) -> Result<(Option<Vec<String>>, String), String> {
    if args.all_files {
        return Ok((None, "all-files".into()));
    }
    if !args.only.is_empty() {
        let scope = mutation_scope::explicit_scope(&args.only, repo_root)?;
        return Ok((Some(scope), "exact --only paths".into()));
    }
    let scope = mutation_scope::branch_scope(&args.base, repo_root, owner)?;
    Ok((Some(scope), format!("changed vs {}", args.base)))
}

fn priority_options(args: &ScopeArgs) -> Option<PriorityOptions> {
    let priority = &args.priority;
    priority
        .priority_policy
        .clone()
        .map(|policy_path| PriorityOptions {
            policy_path,
            base_ref: priority
                .priority_base
                .clone()
                .unwrap_or_else(|| args.base.clone()),
            telemetry_path: priority.priority_telemetry.clone(),
            budget_seconds: priority.priority_budget_seconds,
        })
}

fn plan(args: ScopeArgs) -> Result<Planned, String> {
    let repo_root = resolve_repo_root(args.repo_root.clone()).map_err(|error| error.to_string())?;
    let owner = resolve_lane(args.owner.clone(), &repo_root)?;
    let (only_sources, scope) = select_scope(&args, &repo_root, &owner)?;
    let extra_tests = mutation_scope::extra_tests(&args.extra_test, &repo_root)?;
    let registry = resolved_registry(&repo_root)?;
    let campaigns = match args.campaigns_root.clone() {
        Some(value) => value,
        None => registry_parent(&registry),
    };
    let request = PlanRequest {
        priority: priority_options(&args),
        language: args.language,
        repo_root: repo_root.to_string_lossy().into_owned(),
        owner,
        day: args.day.unwrap_or_else(today_utc),
        jobs: args.jobs,
        python_jobs: args.python_jobs,
        run_timeout_seconds: args.run_timeout,
        campaigns_root: campaigns.clone(),
        only_sources,
        include_covered: args.include_covered,
        extra_tests,
    };
    Ok(Planned {
        result: compute_plan(&request)?,
        scope,
        repo_root,
        registry,
        campaigns,
    })
}

fn count(result: &Value, key: &str) -> usize {
    result
        .get(key)
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

/// The compact report: what a plan would write and what it refuses to cover.
fn summarise(planned: &Planned, verbose: bool) -> Map<String, Value> {
    let result = &planned.result;
    let mut payload = Map::new();
    payload.insert("status".into(), json!("READY"));
    payload.insert("scope".into(), json!(planned.scope));
    payload.insert("language".into(), result["language"].clone());
    payload.insert("campaigns".into(), json!(count(result, "manifests")));
    payload.insert(
        "subjects_without_a_named_test".into(),
        json!(count(result, "unpaired")),
    );
    payload.insert(
        "lines_without_a_named_test".into(),
        result["unpaired_lines"].clone(),
    );
    payload.insert(
        "already_covered".into(),
        json!(count(result, "already_covered")),
    );
    payload.insert(
        "crates_with_no_tests".into(),
        json!(count(result, "untested")),
    );
    if verbose {
        let ids: Vec<Value> = result["manifests"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|manifest| manifest["campaign_id"].clone())
            .collect();
        payload.insert("campaign_ids".into(), Value::Array(ids));
        payload.insert("manifests".into(), result["manifests"].clone());
        payload.insert("unpaired".into(), result["unpaired"].clone());
        payload.insert(
            "already_covered_subjects".into(),
            result["already_covered"].clone(),
        );
    }
    if let Some(priority) = result.get("priority") {
        payload.insert("priority".into(), priority.clone());
    }
    payload
}

fn report(outcome: Result<Map<String, Value>, String>) -> Result<i32> {
    let (payload, code) = match outcome {
        Ok(payload) => (Value::Object(payload), 0),
        Err(message) => (json!({"status": "REFUSED", "error": message}), 4),
    };
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(code)
}

/// `plan`: report what would be written; writes nothing.
pub fn run(args: PlanArgs) -> Result<i32> {
    let verbose = args.verbose;
    report(plan(args.scope).map(|planned| summarise(&planned, verbose)))
}

/// `write`: one manifest and one registry fragment per planned subject.
pub fn write(args: WriteArgs) -> Result<i32> {
    let force = args.force;
    report(plan(args.scope).and_then(|planned| {
        let manifests = planned.result["manifests"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let written = mutation_write::write_manifests(
            &manifests,
            &planned.repo_root,
            &planned.registry,
            &planned.campaigns,
            force,
        )?;
        let mut payload = summarise(&planned, false);
        payload.insert("written".into(), json!(written));
        Ok(payload)
    }))
}

pub(crate) fn find_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = start;
    loop {
        if current.join(".git").exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
}

fn today_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (year, month, day) = civil_from_days(secs as i64 / 86_400);
    format!("{year:04}{month:02}{day:02}")
}

/// The configured registry path; the environment override and project
/// manifest resolve through the shared native project paths.
pub(crate) fn resolved_registry(repo_root: &Path) -> Result<String, String> {
    project_paths::resolve(repo_root).map(|paths| paths[REGISTRY_INDEX].0.clone())
}

pub(crate) fn registry_parent(registry_path: &str) -> String {
    Path::new(registry_path)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| ".".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_parent_strips_the_filename() {
        assert_eq!(
            registry_parent("conductor/mutation_campaigns/registry.json"),
            "conductor/mutation_campaigns"
        );
    }

    #[test]
    fn registry_parent_defaults_to_dot_for_a_bare_filename() {
        assert_eq!(registry_parent("registry.json"), ".");
    }

    #[test]
    fn today_utc_is_eight_digits() {
        let value = today_utc();
        assert_eq!(value.len(), 8);
        assert!(value.chars().all(|c| c.is_ascii_digit()));
    }
}
