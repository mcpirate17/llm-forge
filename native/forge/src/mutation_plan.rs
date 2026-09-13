//! `forge mutation plan`: native entry point for conductor-native's mutation
//! plan computation (`conductor_native::mutation_plan::compute_plan`).
//!
//! This binary calls `compute_plan` directly -- no pyo3 call, no Python
//! interpreter -- so `forge mutation plan` starts zero interpreters. Scope
//! resolution (branch diff via `--base`, owner identity, `--all-files`) stays
//! in the Python CLI (`conductor.mutation_campaign_generate`) per the task
//! brief: this binary takes the same lower-level inputs
//! `conductor_native::mutation_plan::PlanRequest` does, one flag per field.

use anyhow::{bail, Context, Result};
use clap::Args;
use conductor_native::mutation_plan::{compute_plan, PlanRequest};
use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::civil::civil_from_days;

#[derive(Args)]
pub struct PlanArgs {
    /// "python" or "rust"
    pub language: String,

    /// Repository root; defaults to the nearest ancestor of the current
    /// directory that contains a `.git` entry.
    #[arg(long)]
    pub repo_root: Option<PathBuf>,

    /// Lane that owns the generated campaign IDs.
    #[arg(long, default_value = "claude")]
    pub owner: String,

    /// `YYYYMMDD`; defaults to today (UTC).
    #[arg(long)]
    pub day: Option<String>,

    #[arg(long, default_value_t = 4)]
    pub jobs: i64,

    #[arg(long = "run-timeout", default_value_t = 1800)]
    pub run_timeout: i64,

    /// Repository-relative directory campaign manifests are registered
    /// under; defaults to `[tool.conductor].mutation_registry`'s parent
    /// (`pyproject.toml`), else `conductor/mutation_campaigns`.
    #[arg(long)]
    pub campaigns_root: Option<String>,

    /// Repository-relative source path to plan; repeatable. Omit for the
    /// whole tree.
    #[arg(long = "only")]
    pub only: Vec<String>,

    /// Also plan sources an existing campaign already names.
    #[arg(long = "include-covered")]
    pub include_covered: bool,

    /// `SOURCE=TEST`; also score SOURCE with TEST when no test is named
    /// after SOURCE. Repeatable. Python subjects only.
    #[arg(long = "extra-test", value_name = "SOURCE=TEST")]
    pub extra_test: Vec<String>,
}

pub fn run(args: PlanArgs) -> Result<i32> {
    let repo_root = match args.repo_root {
        Some(path) => path,
        None => find_repo_root(&env::current_dir().context("reading the current directory")?)
            .context("no .git found above the current directory; pass --repo-root")?,
    };
    let campaigns_root = match args.campaigns_root {
        Some(value) => value,
        None => default_campaigns_root(&repo_root)?,
    };
    let day = args.day.unwrap_or_else(today_utc);
    let extra_tests = parse_extra_tests(&args.extra_test)?;
    let only_sources = if args.only.is_empty() {
        None
    } else {
        Some(args.only)
    };

    let request = PlanRequest {
        language: args.language,
        repo_root: repo_root.to_string_lossy().into_owned(),
        owner: args.owner,
        day,
        jobs: args.jobs,
        run_timeout_seconds: args.run_timeout,
        campaigns_root,
        only_sources,
        include_covered: args.include_covered,
        extra_tests,
    };

    match compute_plan(&request) {
        Ok(value) => {
            println!("{}", serde_json::to_string_pretty(&value)?);
            Ok(0)
        }
        Err(message) => {
            let refused = serde_json::json!({"status": "REFUSED", "error": message});
            println!("{}", serde_json::to_string_pretty(&refused)?);
            Ok(4)
        }
    }
}

fn parse_extra_tests(pairs: &[String]) -> Result<BTreeMap<String, Vec<String>>> {
    let mut extra_tests: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for pair in pairs {
        let Some((source, test)) = pair.split_once('=') else {
            bail!("--extra-test must be SOURCE=TEST, got {pair:?}");
        };
        if source.is_empty() || test.is_empty() {
            bail!("--extra-test must be SOURCE=TEST, got {pair:?}");
        }
        extra_tests
            .entry(source.to_string())
            .or_default()
            .push(test.to_string());
    }
    Ok(extra_tests)
}

fn find_repo_root(start: &Path) -> Option<PathBuf> {
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

/// `[tool.conductor].mutation_registry`'s parent directory, mirroring
/// `conductor.project_paths.campaigns_relative`: `$CONDUCTOR_MUTATION_REGISTRY`
/// wins, then `pyproject.toml`, then the default.
fn default_campaigns_root(repo_root: &Path) -> Result<String> {
    if let Ok(env_value) = env::var("CONDUCTOR_MUTATION_REGISTRY") {
        let trimmed = env_value.trim();
        if !trimmed.is_empty() {
            return Ok(registry_parent(trimmed));
        }
    }
    let pyproject = repo_root.join("pyproject.toml");
    if pyproject.is_file() {
        let text = std::fs::read_to_string(&pyproject)
            .with_context(|| format!("reading {}", pyproject.display()))?;
        let parsed: toml::Value = text
            .parse()
            .with_context(|| format!("parsing {}", pyproject.display()))?;
        if let Some(value) = parsed
            .get("tool")
            .and_then(|table| table.get("conductor"))
            .and_then(|table| table.get("mutation_registry"))
            .and_then(|value| value.as_str())
        {
            return Ok(registry_parent(value));
        }
    }
    Ok("conductor/mutation_campaigns".to_string())
}

fn registry_parent(registry_path: &str) -> String {
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
    fn parse_extra_tests_groups_by_source() {
        let pairs = vec![
            "a/x.py=a/test_x.py".to_string(),
            "a/x.py=a/test_x_extra.py".to_string(),
        ];
        let parsed = parse_extra_tests(&pairs).expect("parses");
        assert_eq!(
            parsed.get("a/x.py"),
            Some(&vec![
                "a/test_x.py".to_string(),
                "a/test_x_extra.py".to_string()
            ])
        );
    }

    #[test]
    fn parse_extra_tests_rejects_missing_equals() {
        let pairs = vec!["a/x.py".to_string()];
        assert!(parse_extra_tests(&pairs).is_err());
    }

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
