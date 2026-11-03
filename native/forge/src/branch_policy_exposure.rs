//! Full read-only branch/workspace exposure reporting; no cleanup/apply operations.
#[path = "branch_policy_reap.rs"]
mod reap;
use crate::ownership::{self, OwnershipClaim};
use anyhow::{Context, Result};
use conductor_native::branch_policy_runtime as core;
use git2::{Repository, StatusOptions};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) fn probe(
    root: &Path,
    program: &Path,
    args: &[&str],
    seconds: u64,
) -> Result<std::process::Output> {
    let mut command = Command::new(program);
    command.current_dir(root).args(args);
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(key);
    }
    crate::process_probe::run(&mut command, Duration::from_secs(seconds), 1024 * 1024)
}
pub(super) fn git(root: &Path, args: &[&str]) -> Result<String> {
    crate::workspace_integration::git(root, args)
}

pub(super) fn gh() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_else(|| "/bin:/usr/bin".into());
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join("gh");
        let candidate = if candidate.is_absolute() {
            candidate
        } else {
            std::env::current_dir().ok()?.join(candidate)
        };
        let name = std::ffi::CString::new(candidate.as_os_str().as_bytes()).ok()?;
        if std::fs::metadata(&candidate).is_ok_and(|metadata| metadata.is_file())
            && unsafe { libc::access(name.as_ptr(), libc::X_OK) } == 0
        {
            return Some(candidate);
        }
    }
    None
}
pub(super) fn prs(root: &Path, branch: &str, all: bool, program: &Path) -> Result<Vec<Value>> {
    let mut args = vec!["pr", "list", "--head", branch];
    if all {
        args.extend(["--state", "all"]);
    }
    args.extend(["--json", if all { "state" } else { "number,state" }]);
    let output = probe(root, program, &args, 15)?;
    anyhow::ensure!(
        output.status.success(),
        "gh PR lookup failed for {branch}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let rows: Vec<Value> =
        serde_json::from_slice(&output.stdout).context("gh PR response must be a JSON array")?;
    anyhow::ensure!(
        rows.len() <= 10000
            && rows
                .iter()
                .all(|row| row.is_object() && row["state"].is_string()),
        "gh PR response has invalid rows"
    );
    Ok(rows)
}
fn claim_for(claims: &[OwnershipClaim], path: &str, now: f64) -> Result<String> {
    for claim in claims {
        if claim.active(now)?
            && claim
                .paths
                .iter()
                .any(|candidate| ownership::paths_overlap(candidate, path))
        {
            return Ok(claim.claim_id.clone());
        }
    }
    Ok("UNCLAIMED".into())
}
fn commits(root: &Path, claims: &[OwnershipClaim], now: f64) -> Result<Value> {
    let mut rows = core::local_only_commits(root, "HEAD")?;
    for row in &mut rows {
        // Preserve Git's configured merge/rename presentation, exactly as the
        // original report did; NUL framing retains literal UTF-8 path names.
        let paths = git(
            root,
            &[
                "show",
                "-z",
                "--name-only",
                "--format=",
                row["sha"].as_str().context("missing commit hash")?,
            ],
        )?;
        let mut claim = "UNCLAIMED".to_owned();
        for path in paths.split('\0').filter(|path| !path.is_empty()) {
            let hit = claim_for(claims, path, now)?;
            if hit != "UNCLAIMED" {
                claim = hit;
                break;
            }
        }
        row["claim_id"] = json!(claim);
    }
    Ok(json!(rows))
}
fn dirty(root: &Path, claims: &[OwnershipClaim], now: f64, stale_hours: f64) -> Result<Value> {
    let repository = Repository::discover(root)?;
    let mut options = StatusOptions::new();
    options
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true);
    let statuses = repository.statuses(Some(&mut options))?;
    anyhow::ensure!(
        statuses.len() <= 100000,
        "dirty inventory exceeds100000 entries"
    );
    let mut rows = Vec::new();
    for entry in statuses.iter() {
        let path = match entry
            .index_to_workdir()
            .or_else(|| entry.head_to_index())
            .and_then(|delta| delta.new_file().path())
        {
            Some(path) => path,
            None => Path::new(entry.path().context("dirty path must be UTF-8")?),
        };
        let full = root.join(path);
        let metadata = match std::fs::metadata(&full) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let modified = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)?
            .as_secs_f64();
        let hours = (now - modified) / 3600.0;
        if hours > stale_hours {
            let path = path.to_str().context("dirty path must be UTF-8")?;
            rows.push(json!({"path":path,"age_hours":format!("{hours:.1}"),"claim_id":claim_for(claims,path,now)?}));
        }
    }
    rows.sort_by(|a, b| {
        b["age_hours"]
            .as_str()
            .unwrap()
            .parse::<f64>()
            .unwrap()
            .total_cmp(&a["age_hours"].as_str().unwrap().parse::<f64>().unwrap())
    });
    Ok(json!(rows))
}
fn branch_created(repository: &Repository, reference: &str, live: &str) -> Result<Option<f64>> {
    let mut walk = repository.revwalk()?;
    walk.set_sorting(git2::Sort::TIME)?;
    walk.push(repository.find_reference(reference)?.peel_to_commit()?.id())?;
    walk.hide(repository.revparse_single(live)?.peel_to_commit()?.id())?;
    let mut created = None;
    for (index, oid) in walk.enumerate() {
        anyhow::ensure!(index < 100000, "branch history exceeds100000 entries");
        created = Some(repository.find_commit(oid?)?.time().seconds() as f64);
    }
    Ok(created)
}
fn branches(root: &Path, live: &str, now: f64) -> Result<(Value, bool, Vec<String>)> {
    let repository = Repository::discover(root)?;
    let names = core::integration_branches(root)?;
    let bindings: BTreeMap<_, _> = core::load_bindings(root)?
        .into_iter()
        .map(|row| (row.branch.clone(), row))
        .collect();
    let program = gh();
    let mut rows = Vec::new();
    let mut errors = Vec::new();
    for reference in core::refs(&repository, "refs/heads/")? {
        let name = &reference[11..];
        if name == live || names.iter().any(|line| line == name) {
            continue;
        }
        let Some(created) = branch_created(&repository, &reference, live)? else {
            continue;
        };
        let binding = bindings.get(name);
        let (pushed, source) = if let Some(stamp) =
            binding.and_then(|row| row.last_push_at.as_deref())
        {
            (
                Some(crate::instant::parse(stamp).context("invalid recorded push timestamp")?),
                "recorded push",
            )
        } else {
            match repository.find_reference(&format!("refs/remotes/origin/{name}")) {
                Ok(remote) => (
                    Some(remote.peel_to_commit()?.time().seconds() as f64),
                    "remote-tip commit date (approx)",
                ),
                Err(error) if error.code() == git2::ErrorCode::NotFound => (None, "never pushed"),
                Err(error) => return Err(error.into()),
            }
        };
        let mut reasons = Vec::new();
        let hours = pushed.map(|stamp| (now - stamp) / 3600.0);
        if hours.is_none_or(|hours| hours > core::STALE_PUSH_HOURS) {
            let seen = hours
                .map(|hours| format!("{hours:.1}h ago"))
                .unwrap_or("never".into());
            reasons.push(format!("no push in 6h (last push {seen}, source={source})"));
        }
        if let Some(program) = &program {
            match prs(root, name, false, program) {
                Ok(prs) if prs.is_empty() && (now - created) / 3600.0 > core::STALE_PR_HOURS => {
                    reasons.push(format!(
                        "no PR in 24h (created {:.1}h ago)",
                        (now - created) / 3600.0
                    ))
                }
                Ok(_) => (),
                Err(error) => errors.push(format!("{error:#}")),
            }
        }
        if !reasons.is_empty() {
            rows.push(json!({"branch":name,"claim_id":binding.map(|row|row.claim_id.as_str()).unwrap_or("UNCLAIMED"),"owner":binding.map(|row|row.owner.as_str()).unwrap_or("UNCLAIMED"),"reasons":reasons}));
        }
    }
    Ok((json!(rows), program.is_some(), errors))
}
pub(crate) fn data(
    root: &Path,
    operation: &str,
    live: Option<&str>,
    stale_hours: f64,
) -> Result<Value> {
    anyhow::ensure!(
        stale_hours.is_finite() && stale_hours >= 0.0,
        "stale-hours must be finite and nonnegative"
    );
    let repository = Repository::discover(root)?;
    let root = repository
        .workdir()
        .context("exposure requires a working tree")?
        .canonicalize()?;
    let root = root.as_path();
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
    match operation {
        "reap-preview" => Ok(json!(reap::preview(root)?)),
        "branches" => {
            let live = match live {
                Some(live) => live.to_owned(),
                None => repository.head()?.shorthand().unwrap_or("HEAD").to_owned(),
            };
            let (rows, available, errors) = branches(root, &live, now)?;
            Ok(json!({"rows":rows,"gh_available":available,"gh_errors":errors}))
        }
        "commits" => commits(root, &ownership::load_claims(root)?.0, now),
        "dirty" => dirty(root, &ownership::load_claims(root)?.0, now, stale_hours),
        "report" => {
            let live = match live {
                Some(live) => live.to_owned(),
                None => repository.head()?.shorthand().unwrap_or("HEAD").to_owned(),
            };
            let (branches, available, errors) = branches(root, &live, now)?;
            let claims = ownership::load_claims(root)?.0;
            Ok(
                json!({"stale_feature_branches":branches,"gh_available":available,"gh_errors":errors,
                "local_only_commit_exposure":commits(root,&claims,now)?,"stale_dirty_files":dirty(root,&claims,now,stale_hours)?,"reap_preview":reap::preview(root)?}),
            )
        }
        _ => anyhow::bail!("unknown native exposure operation {operation}"),
    }
}
pub(super) fn report(root: &Path) -> Result<Value> {
    data(root, "report", None, 24.0)
}
pub(crate) fn render(report: &Value) -> Result<String> {
    let rows = |key: &str| {
        report[key]
            .as_array()
            .with_context(|| format!("exposure report missing {key}"))
    };
    let text = |row: &Value, key: &str| {
        row[key]
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("invalid exposure field {key}"))
    };
    let branches = rows("stale_feature_branches")?;
    let available = report["gh_available"]
        .as_bool()
        .context("missing gh_available")?;
    let note = if !available {
        "  [gh NOT on PATH: PR staleness skipped]"
    } else {
        ""
    };
    let mut lines = vec![format!(
        "EXPOSED BRANCHES -- stale by push or PR ({}){note}",
        branches.len()
    )];
    for row in branches {
        lines.push(format!(
            "  {}  claim={}  owner={}",
            text(row, "branch")?,
            text(row, "claim_id")?,
            text(row, "owner")?
        ));
        for reason in row["reasons"]
            .as_array()
            .context("invalid branch reasons")?
        {
            lines.push(format!(
                "      {}",
                reason.as_str().context("invalid branch reason")?
            ));
        }
    }
    if let Some(errors) = report["gh_errors"].as_array() {
        for error in errors {
            lines.push(format!(
                "  [PR staleness unavailable: {}]",
                error.as_str().context("invalid PR error")?
            ));
        }
    }
    let commits = rows("local_only_commit_exposure")?;
    lines.push(format!(
        "EXPOSED LOCAL-ONLY COMMITS -- on no pushed ref ({})",
        commits.len()
    ));
    for row in commits {
        let sha = text(row, "sha")?;
        lines.push(format!(
            "  {}  claim={}  {}",
            sha.get(..8).context("invalid commit hash")?,
            text(row, "claim_id")?,
            text(row, "subject")?
        ));
    }
    let files = rows("stale_dirty_files")?;
    lines.push(format!(
        "EXPOSED STALE DIRTY FILES -- >24h uncommitted ({})",
        files.len()
    ));
    for row in files {
        lines.push(format!(
            "  {:>6}h  claim={}  {}",
            text(row, "age_hours")?,
            text(row, "claim_id")?,
            text(row, "path")?
        ));
    }
    let trees = rows("reap_preview")?;
    lines.push(format!(
        "EXPOSED WORKTREES -- next reap archives then removes ({})",
        trees.len()
    ));
    for row in trees {
        lines.push(format!(
            "  {}  [{}]  {}",
            text(row, "worktree")?,
            text(row, "branch")?,
            text(row, "reason")?
        ));
    }
    Ok(lines.join("\n"))
}
