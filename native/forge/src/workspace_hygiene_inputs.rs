//! Read-only full-report inputs. Report idleness deliberately ignores activity leases.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

// Native report contract: reject nonregular/symlink JSON, >4 MiB documents,
// >100,000 inventory rows, and >32 MiB aggregate bytes per input lane.
// Legacy Python followed JSON symlinks and imposed no resource limits.
const JSON_LIMIT: usize = 4 * 1024 * 1024;
const TOTAL_LIMIT: usize = 32 * 1024 * 1024;
const ROW_LIMIT: usize = 100_000;

fn charge(total: &mut usize, amount: usize) -> Result<()> {
    *total = total
        .checked_add(amount)
        .filter(|v| *v <= TOTAL_LIMIT)
        .context("hygiene input exceeds 32 MiB aggregate limit")?;
    Ok(())
}

fn timestamp(raw: &str) -> Result<f64> {
    let (epoch, aware) =
        conductor_native::branch_policy::stamp_epoch_seconds(raw).map_err(anyhow::Error::msg)?;
    if !aware {
        bail!("can't subtract offset-naive and offset-aware datetimes");
    }
    Ok(epoch)
}

/// Expiry uses the declared expiry, never ownership's activity or hard deadline.
pub(crate) fn claims(root: &Path, now: f64, idle_minutes: u64) -> Result<(Vec<Value>, Vec<Value>)> {
    let (stored, _) = crate::ownership::load_claims(root)?;
    anyhow::ensure!(
        stored.len() <= ROW_LIMIT,
        "claim inventory exceeds 100000 rows"
    );
    let mut path_count = 0usize;
    let mut total = 0;
    let mut expired = Vec::new();
    let mut idle = Vec::new();
    for claim in stored {
        path_count = path_count
            .checked_add(claim.paths.len())
            .filter(|count| *count <= ROW_LIMIT)
            .context("claim paths exceed 100000 entries")?;
        for path in &claim.paths {
            charge(&mut total, path.len())?;
        }
        if timestamp(&claim.expires_at)? < now {
            expired.push(json!({"claim_id":claim.claim_id,"owner":claim.owner,
                "expired_at":claim.expires_at}));
            continue;
        }
        let mut touched: Option<f64> = None;
        for path in &claim.paths {
            let path = root.join(path);
            if !path.exists() {
                continue;
            }
            let modified = path.metadata()?.modified()?;
            let seconds = match modified.duration_since(UNIX_EPOCH) {
                Ok(age) => age.as_secs_f64(),
                Err(age) => -age.duration().as_secs_f64(),
            };
            touched = Some(touched.map_or(seconds, |previous| previous.max(seconds)));
        }
        let Some(touched) = touched else {
            continue;
        };
        let minutes = (now - touched.max(timestamp(&claim.created_at)?)) / 60.0;
        if minutes < idle_minutes as f64 {
            continue;
        }
        idle.push(json!({"claim_id":claim.claim_id,"owner":claim.owner,
            "idle_minutes":format!("{minutes:.0}"),"expires_at":claim.expires_at,
            "paths":claim.paths.join(", ")}));
    }
    // Python sorts the rounded string, stably, rather than the original duration.
    idle.sort_by(|a, b| {
        let key = |v: &Value| v["idle_minutes"].as_str().unwrap().parse::<f64>().unwrap();
        key(b).total_cmp(&key(a))
    });
    Ok((expired, idle))
}

fn read_json(path: &Path, total: &mut usize) -> Result<Value> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file(),
        "JSON input must be a regular file: {}",
        path.display()
    );
    anyhow::ensure!(
        metadata.len() <= JSON_LIMIT as u64,
        "JSON input exceeds 4 MiB: {}",
        path.display()
    );
    anyhow::ensure!(
        metadata.len() as usize <= TOTAL_LIMIT.saturating_sub(*total),
        "hygiene input exceeds 32 MiB aggregate limit"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(JSON_LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= JSON_LIMIT, "JSON input grew beyond 4 MiB");
    charge(total, bytes.len())?;
    serde_json::from_slice(&bytes).with_context(|| format!("invalid JSON at {}", path.display()))
}

fn directory_entries(path: &Path, total: &mut usize) -> Result<Vec<fs::DirEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        anyhow::ensure!(
            entries.len() < ROW_LIMIT,
            "directory inventory exceeds 100000 entries"
        );
        let entry = entry?;
        charge(total, entry.file_name().as_encoded_bytes().len())?;
        entries.push(entry);
    }
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

/// The report's tolerant row projection, distinct from campaign validation.
fn registered_manifests(registry: &Path, total: &mut usize) -> Result<BTreeSet<String>> {
    let payload = read_json(registry, total)?;
    if !payload.is_object() {
        bail!("{} is not a registry object", registry.display());
    }
    let rows = payload["campaigns"]
        .as_array()
        .context("registry.campaigns must be a list")?;
    anyhow::ensure!(rows.len() <= ROW_LIMIT, "registry exceeds 100000 rows");
    let mut paths: BTreeSet<String> = rows
        .iter()
        .filter_map(|row| {
            row.get("manifest")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    let directory = registry
        .parent()
        .context("registry has no parent")?
        .join("registry.d");
    if directory.is_dir() {
        let fragments = directory_entries(&directory, total)?;
        for entry in fragments {
            if entry.path().extension().is_some_and(|s| s == "json") {
                let fragment = read_json(&entry.path(), total)?;
                let manifest = fragment
                    .get("manifest")
                    .and_then(Value::as_str)
                    .context("registry.d fragment has no manifest string")?;
                paths.insert(manifest.to_owned());
                anyhow::ensure!(
                    paths.len() <= ROW_LIMIT,
                    "manifest inventory exceeds 100000 rows"
                );
            }
        }
    }
    Ok(paths)
}

pub(crate) fn manifests(root: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let paths = conductor_native::project_paths::resolve(root).map_err(anyhow::Error::msg)?;
    let relative = &paths[1].0; // Shared ProjectPaths field order: mutation_registry.
    let registry = root.join(relative);
    if !registry.is_file() {
        bail!("registry not found: {relative}");
    }
    let mut total = 0;
    let registered = registered_manifests(&registry, &mut total)?;
    let directory = Path::new(relative)
        .parent()
        .context("registry has no parent")?;
    let mut on_disk = BTreeSet::new();
    // Python ProjectPaths.campaigns_relative is exactly registry_relative.parent.
    for entry in directory_entries(&root.join(directory), &mut total)? {
        let path = entry.path();
        if path.is_file()
            && path.extension().is_some_and(|s| s == "json")
            && entry.file_name() != "registry.json"
        {
            let relative = directory.join(entry.file_name());
            on_disk.insert(
                relative
                    .to_str()
                    .context("manifest path must be UTF-8")?
                    .to_owned(),
            );
        }
    }
    let orphan = on_disk.difference(&registered).cloned().collect();
    let dangling = registered
        .into_iter()
        .filter(|p| !root.join(p).is_file())
        .collect();
    Ok((orphan, dangling))
}

pub(crate) fn untracked_imports(root: &Path) -> Result<Vec<Value>> {
    let source = crate::candidate_source::Source::open(root)?;
    let tracked = source.tracked_index_paths()?;
    let scan = conductor_native::dead_tests::bounded::scan(source.root(), &tracked)?;
    anyhow::ensure!(
        source.tracked_index_paths()? == tracked,
        "live index paths changed during hygiene import scan"
    );
    anyhow::ensure!(
        conductor_native::dead_tests::bounded::scan(source.root(), &tracked)? == scan,
        "live import sources or lookups changed during hygiene import scan"
    );
    // Deliberate native difference: bounded scanner refuses parse/read failures;
    // legacy Python discarded parse_error_paths and returned partial findings.
    scan.records
        .into_iter()
        .map(|record| serde_json::to_value(record).map_err(Into::into))
        .collect()
}

pub(crate) fn leases(_root: &Path, worktrees: &[PathBuf]) -> Result<Vec<Value>> {
    leases_at(worktrees, crate::instant::now())
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.0),
    }
}

fn leases_at(worktrees: &[PathBuf], now: f64) -> Result<Vec<Value>> {
    anyhow::ensure!(
        worktrees.len() <= ROW_LIMIT,
        "worktree inventory exceeds 100000 entries"
    );
    let mut total = 0;
    let mut rows = Vec::new();
    for root in worktrees {
        charge(&mut total, root.as_os_str().as_encoded_bytes().len())?;
        if !root.is_dir() || !root.join(".git").is_file() {
            continue;
        }
        let target = root.join(".worktree-lease.json");
        if !target.try_exists()? {
            rows.push(json!({"worktree":root,"status":"unleased"}));
            continue;
        }
        let record = read_json(&target, &mut total)
            .with_context(|| format!("cannot read the lease at {}", target.display()))?;
        if !record.is_object() || record["schema"] != "worktree-lease.v1" {
            bail!("{} is not a worktree-lease.v1 lease", target.display());
        }
        for field in ["owner", "purpose", "expires_at"] {
            if !truthy(&record[field]) {
                bail!("the lease at {} has no {field}", target.display());
            }
        }
        let raw = match &record["expires_at"] {
            Value::String(value) => value.clone(),
            other => other.to_string(),
        };
        let overdue = now - timestamp(&raw)?;
        rows.push(json!({"worktree":root,"status":if overdue > 0.0 {"expired"} else {"leased"},
            "owner":record["owner"],"purpose":record["purpose"],
            "branch":record.get("branch").cloned().unwrap_or(json!("")),
            "expires_at":record["expires_at"],"overdue_minutes":(overdue / 60.0).floor().max(0.0) as i64}));
    }
    Ok(rows)
}

#[cfg(test)]
#[path = "workspace_hygiene_inputs_tests.rs"]
mod tests;
