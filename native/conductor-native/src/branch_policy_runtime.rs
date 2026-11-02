//! Live branch policy. Never creates/deletes branches, worktrees or remote refs.
//! Bind/unbind mutate only the locked, atomic common-Git-dir binding store.
#[path = "branch_policy_store.rs"]
mod store;
use crate::branch_policy::{self as policy, Binding};
use anyhow::{ensure, Context, Result};
use git2::Repository;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
pub const STALE_PUSH_HOURS: f64 = 6.0;
pub const STALE_PR_HOURS: f64 = 24.0;
const MAX_REFS: usize = 100_000;

pub fn now() -> Result<String> {
    let micros = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros())?;
    Ok(chrono::DateTime::from_timestamp_micros(micros)
        .context("clock outside supported calendar")?
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, false))
}
fn today(now: &str) -> Result<String> {
    policy::parse_stamp(now).map_err(|_| anyhow::anyhow!("invalid current timestamp"))?;
    let stamp = chrono::DateTime::parse_from_rfc3339(now)
        .context("current timestamp must include timezone")?;
    Ok(stamp
        .with_timezone(&chrono::Utc)
        .format("%Y%m%d")
        .to_string())
}
pub fn validate_name(name: &str, now: &str) -> Result<Value> {
    let (raw, agent, topic, date) =
        policy::validate_name(name, &today(now)?).map_err(anyhow::Error::msg)?;
    Ok(json!({"raw":raw,"agent":agent,"topic":topic,"date":date}))
}
fn repository(root: &Path) -> Result<Repository> {
    Repository::discover(root).context("open explicit branch-policy Git repository")
}
pub fn binding_store_path(root: &Path) -> Result<PathBuf> {
    Ok(repository(root)?
        .commondir()
        .canonicalize()?
        .join("governance/branch-bindings.json"))
}
pub fn integration_branches(root: &Path) -> Result<Vec<String>> {
    let repository = repository(root)?;
    let root = repository
        .workdir()
        .unwrap_or(repository.commondir())
        .canonicalize()?;
    let root = root.as_path();
    let mut names =
        vec![crate::project_paths::integration_branch(root).map_err(anyhow::Error::msg)?];
    names.extend(
        crate::project_paths::retired_integration_branches(root).map_err(anyhow::Error::msg)?,
    );
    Ok(names)
}
pub fn refs(repo: &Repository, prefix: &str) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for reference in repo.references()? {
        let reference = reference?;
        let name = reference
            .name()
            .context("branch policy requires UTF-8 ref names")?;
        if name.starts_with(prefix) {
            names.push(name.to_owned());
        }
        ensure!(
            names.len() <= MAX_REFS,
            "branch-policy ref inventory exceeds100000 entries"
        );
    }
    names.sort();
    Ok(names)
}
pub fn load_bindings(root: &Path) -> Result<Vec<Binding>> {
    store::load(&binding_store_path(root)?)
}
pub fn bind(root: &Path, branch: &str, claim: &str, owner: &str, now: &str) -> Result<Value> {
    let parsed = validate_name(branch, now)?;
    ensure!(
        parsed["agent"] == owner,
        "branch {} agent segment {} does not match owner {}",
        policy::python_repr(branch),
        policy::python_repr(parsed["agent"].as_str().unwrap()),
        policy::python_repr(owner)
    );
    let path = binding_store_path(root)?;
    store::edit(&path, |rows| {
        let live = refs(&repository(root)?, "refs/heads/")?
            .into_iter()
            .map(|name| name[11..].to_owned())
            .collect::<Vec<_>>();
        if let Some(reason) = policy::second_branch_conflict(&live, rows, branch, claim) {
            anyhow::bail!(reason);
        }
        rows.retain(|row| row.branch != branch);
        let row = Binding {
            branch: branch.into(),
            claim_id: claim.into(),
            owner: owner.into(),
            created_at: now.into(),
            last_push_at: None,
            pr_number: None,
        };
        let result = row.to_row();
        rows.push(row);
        Ok(result)
    })
}
pub fn unbind(root: &Path, branch: &str) -> Result<bool> {
    let path = binding_store_path(root)?;
    // Missing bindings do not create metadata, preserving no-op write behavior.
    if !load_bindings(root)?.iter().any(|row| row.branch == branch) {
        return Ok(false);
    }
    store::edit(&path, |rows| {
        let old = rows.len();
        rows.retain(|row| row.branch != branch);
        Ok(rows.len() != old)
    })
}
pub fn audit(root: &Path, now: &str) -> Result<Vec<String>> {
    let repository = repository(root)?;
    let live = refs(&repository, "refs/heads/")?;
    let integration = integration_branches(root)?;
    let mut findings = Vec::new();
    let mut naming = Vec::new();
    for reference in &live {
        let name = &reference[11..];
        if integration.iter().any(|line| line == name) {
            let remote = format!("refs/remotes/origin/{name}");
            match repository.find_reference(&remote) {
                Ok(old) => {
                    let old = old.peel_to_commit()?.id();
                    let new = repository.find_reference(reference)?.peel_to_commit()?.id();
                    if old != new && !repository.graph_descendant_of(new, old)? {
                        findings.push(format!("rule 1: integration branch {} has diverged from origin/{name}; landing it would rewrite the integration line", policy::python_repr(name)));
                    }
                }
                Err(error) if error.code() == git2::ErrorCode::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        } else if let Err(error) = validate_name(name, now) {
            naming.push(format!(
                "rule 2: branch {} is misnamed: {error}",
                policy::python_repr(name)
            ));
        }
    }
    findings.extend(naming);
    let mut claims: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in load_bindings(root)? {
        if live.contains(&format!("refs/heads/{}", row.branch)) {
            claims.entry(row.claim_id).or_default().push(row.branch);
        }
    }
    for (claim, mut branches) in claims {
        if branches.len() > 1 {
            branches.sort();
            findings.push(format!(
                "rule 3: claim {claim} has {} live branches ({}); one claim, one branch",
                branches.len(),
                branches.join(", ")
            ));
        }
    }
    Ok(findings)
}
pub fn local_only_commits(root: &Path, branch: &str) -> Result<Vec<Value>> {
    let repository = repository(root)?;
    let mut walk = repository.revwalk()?;
    walk.set_sorting(git2::Sort::TIME)?;
    walk.push(repository.revparse_single(branch)?.peel_to_commit()?.id())?;
    for reference in refs(&repository, "refs/")?
        .into_iter()
        .filter(|name| name.starts_with("refs/remotes/") || name.starts_with("refs/snapshots/"))
    {
        walk.hide(
            repository
                .find_reference(&reference)?
                .peel_to_commit()?
                .id(),
        )?;
    }
    let mut rows = Vec::new();
    for oid in walk {
        let oid = oid?;
        let commit = repository.find_commit(oid)?;
        let subject = std::str::from_utf8(commit.summary_bytes().unwrap_or(b""))?;
        rows.push(json!({"sha":oid.to_string(),"subject":subject}));
        ensure!(
            rows.len() <= 100_000,
            "local-only commit inventory exceeds100000 entries"
        );
    }
    Ok(rows)
}
pub fn status(root: &Path, now: &str) -> Result<Value> {
    let mut rows = Vec::new();
    for binding in load_bindings(root)? {
        let age = policy::stamp_age_hours(
            binding
                .last_push_at
                .as_deref()
                .unwrap_or(&binding.created_at),
            now,
        )
        .map_err(anyhow::Error::msg)?;
        let mut row = binding.to_row();
        row["age_hours"] = json!(format!("{age:.2}").parse::<f64>()?);
        row["stale"] = json!(age > STALE_PUSH_HOURS);
        rows.push(row);
    }
    Ok(json!({"bindings":rows}))
}
pub fn dispatch(operation: &str, request: &Value) -> Result<Value> {
    let root = Path::new(request["repo"].as_str().unwrap_or("."));
    let moment = request["now"]
        .as_str()
        .map(str::to_owned)
        .map(Ok)
        .unwrap_or_else(now)?;
    let field = |key: &str| {
        request[key]
            .as_str()
            .with_context(|| format!("missing {key}"))
    };
    match operation {
        "check-branch" => validate_name(field("name")?, &moment),
        "bindings" => Ok(json!(load_bindings(root)?
            .iter()
            .map(Binding::to_row)
            .collect::<Vec<_>>())),
        "bind" => {
            let branch = field("branch")?;
            let parsed = validate_name(branch, &moment)?;
            bind(
                root,
                branch,
                field("claim")?,
                request["owner"]
                    .as_str()
                    .unwrap_or(parsed["agent"].as_str().unwrap()),
                &moment,
            )
        }
        "unbind" => Ok(json!(unbind(root, field("branch")?)?)),
        "audit" => Ok(json!(audit(root, &moment)?)),
        "status" => status(root, &moment),
        "integration-branches" => Ok(json!(integration_branches(root)?)),
        "local-only-commits" => Ok(json!(local_only_commits(
            root,
            request["branch"].as_str().unwrap_or("HEAD")
        )?)),
        "thresholds" => {
            Ok(json!({"stale_push_hours":STALE_PUSH_HOURS,"stale_pr_hours":STALE_PR_HOURS}))
        }
        _ => anyhow::bail!("unknown branch policy operation {operation}"),
    }
}
#[cfg(feature = "python")]
#[pyo3::pyfunction]
fn branch_policy_runtime_native(operation: &str, request: &str) -> pyo3::PyResult<String> {
    let value: Value = serde_json::from_str(request)
        .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))?;
    let response = dispatch(operation, &value)
        .map_err(|error| pyo3::exceptions::PyRuntimeError::new_err(format!("{error:#}")))?;
    serde_json::to_string(&response)
        .map_err(|error| pyo3::exceptions::PyValueError::new_err(error.to_string()))
}
#[cfg(feature = "python")]
pub(crate) fn register(module: &pyo3::Bound<'_, pyo3::types::PyModule>) -> pyo3::PyResult<()> {
    use pyo3::types::PyModuleMethods;
    module.add_function(pyo3::wrap_pyfunction!(
        branch_policy_runtime_native,
        module
    )?)?;
    Ok(())
}
