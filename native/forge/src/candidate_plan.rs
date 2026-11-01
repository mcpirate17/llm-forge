//! Inspect the exact candidate's policy and inputs without executing checks.
use crate::{
    candidate_policy,
    candidate_snapshot::{Limits, Snapshot},
    candidate_source::{Kind, Source},
};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, OpenOptions, OpenOptionsExt};
use clap::{Args, ValueEnum};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, ValueEnum)]
pub enum Profile {
    Fast,
    Full,
    Nightly,
}
impl Profile {
    fn name(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Full => "full",
            Self::Nightly => "nightly",
        }
    }
}

#[derive(Args)]
pub struct PlanArgs {
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    #[arg(long, value_enum)]
    kind: Kind,
    #[arg(long, default_value = "HEAD")]
    target_ref: String,
    #[arg(long)]
    base_ref: Option<String>,
    #[arg(long)]
    policy: Option<String>,
    #[arg(long, value_enum, default_value = "fast")]
    profile: Profile,
    /// UTC calendar date used only for this planning operation; defaults to today.
    #[arg(long)]
    today: Option<String>,
    /// Existing parent for the private, automatically removed snapshot.
    #[arg(long)]
    scratch_parent: Option<PathBuf>,
}

fn today() -> Result<String> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let days = i64::try_from(seconds / 86_400)?;
    let (year, month, day) = crate::civil::civil_from_days(days);
    ensure!(
        (1..=9999).contains(&year),
        "current UTC date is outside supported range"
    );
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

pub fn run(args: PlanArgs) -> Result<Value> {
    let date = args.today.map(Ok).unwrap_or_else(today)?;
    candidate_policy::validate_today(&date)?;
    let source = Source::open(&args.repo)?;
    let mut candidate = source.resolve(args.kind, &args.target_ref, args.base_ref.as_deref())?;
    let scratch = args.scratch_parent.unwrap_or_else(std::env::temp_dir);
    let output = scratch.join(crate::native_id::new("forge-candidate-plan")?);
    let snapshot = Snapshot::create(&source, &candidate.tree_oid, &output, Limits::default())?;
    let policy = candidate_policy::load(&snapshot.path(), args.policy.as_deref(), &date)?;
    policy.classify_candidate(&mut candidate)?;
    let checks = policy.active_checks(args.profile.name())?;
    let changes = serde_json::to_value(&candidate.changes)?;
    let changes = changes
        .as_array()
        .context("candidate changes must be an array")?;
    let planned = plan_checks(changes, &checks)?;
    let classes: BTreeSet<_> = candidate
        .changes
        .iter()
        .flat_map(|change| change.classes.iter().map(String::as_str))
        .collect();
    let baselines = baseline_receipts(
        &snapshot.path(),
        &policy.document,
        args.profile.name(),
        &classes,
    )?;
    let report = json!({"schema_version":1,"plan_only":true,"profile":args.profile.name(),"today":date,
        "candidate":candidate,"policy":{"path":policy.path,"sha256":policy.digest,"configuration":policy.document},
        "checks":planned,"baselines":baselines});
    snapshot.discard()?;
    Ok(report)
}

fn plan_checks(changes: &[Value], checks: &[Value]) -> Result<Vec<Value>> {
    ensure!(
        changes
            .len()
            .checked_mul(checks.len())
            .is_some_and(|work| work <= 10_000_000),
        "candidate check selection exceeds 10 million change/check pairs"
    );
    let mut planned = Vec::with_capacity(checks.len());
    let mut output_bytes = 0usize;
    for check in checks {
        let files = conductor_native::candidate_checks::select_files(changes, check)
            .map_err(anyhow::Error::msg)?;
        for file in &files {
            output_bytes = output_bytes
                .checked_add(file.len() + 8)
                .context("candidate file-list size overflow")?;
            ensure!(
                output_bytes <= 32 * 1024 * 1024,
                "candidate selected files exceed 32 MiB"
            );
        }
        let scheduled = check["always"]
            .as_bool()
            .context("check always flag missing")?
            || !files.is_empty();
        planned.push(json!({"check":check,"files":files,"scheduled":scheduled}));
    }
    Ok(planned)
}

fn baseline_receipts(
    snapshot: &Path,
    policy: &Value,
    profile: &str,
    classes: &BTreeSet<&str>,
) -> Result<Vec<Value>> {
    let root = Dir::open_ambient_dir(snapshot, cap_std::ambient_authority())?;
    let mut receipts = Vec::new();
    let mut remaining = 2 * 1024 * 1024 * 1024u64;
    for baseline in policy["baselines"]
        .as_array()
        .context("policy baselines missing")?
    {
        let required = baseline["required_profiles"]
            .as_array()
            .context("baseline profiles missing")?
            .iter()
            .any(|item| item.as_str() == Some(profile));
        let relevant = baseline["classes"]
            .as_array()
            .context("baseline classes missing")?
            .iter()
            .filter_map(Value::as_str)
            .any(|class| classes.contains(class));
        if !required || !relevant {
            continue;
        }
        let path = baseline["path"].as_str().context("baseline path missing")?;
        let digest = baseline_digest(&root, path, &mut remaining)
            .with_context(|| format!("candidate baseline {path}"))?;
        receipts.push(json!({"id":baseline["baseline_id"],"path":path,"sha256":digest,"expires":policy["baseline_expires"]}));
    }
    Ok(receipts)
}

fn baseline_digest(root: &Dir, path: &str, remaining: &mut u64) -> Result<String> {
    ensure!(
        !path.is_empty()
            && path.len() <= 4096
            && !path.contains('\0')
            && !Path::new(path).is_absolute()
            && Path::new(path).components().count() <= 256
            && Path::new(path).components().all(|part| matches!(
                part,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )),
        "baseline path must be a bounded candidate-relative path without traversal"
    );
    ensure!(
        !path
            .split('/')
            .any(|part| part.eq_ignore_ascii_case(".git")),
        "baseline cannot name Git metadata"
    );
    // The capability permits internal relative symlinks but refuses escape.
    let mut file = root.open_with(
        path,
        OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK),
    )?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.len() <= 64 * 1024 * 1024,
        "baseline must be a regular file at most 64 MiB"
    );
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        ensure!(
            size <= 64 * 1024 * 1024 && *remaining >= read as u64,
            "baseline byte budget exceeded"
        );
        *remaining -= read as u64;
        hash.update(&buffer[..read]);
    }
    ensure!(size == meta.len(), "baseline size changed during read");
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_honors_classes_exclusions_deletions_and_always() {
        let changes = vec![
            json!({"path":"deleted.rs","classes":["rust"],"new_mode":"000000"}),
            json!({"path":"live.rs","classes":["rust"],"new_mode":"100644"}),
            json!({"path":"generated.rs","classes":["rust","generated"],"new_mode":"100644"}),
        ];
        let regular = json!({"classes":["rust"],"exclude_classes":["generated"],"run_on_deletions":false,"always":false});
        let mut deletions = regular.clone();
        deletions["run_on_deletions"] = json!(true);
        let mut absent = regular.clone();
        absent["classes"] = json!(["docs"]);
        let mut always = absent.clone();
        always["always"] = json!(true);
        let result = plan_checks(&changes, &[regular, deletions, absent, always]).unwrap();
        assert_eq!(result[0]["files"], json!(["live.rs"]));
        assert_eq!(result[1]["files"], json!(["deleted.rs", "live.rs"]));
        assert_eq!(result[2]["scheduled"], false);
        assert_eq!(result[3]["scheduled"], true);
    }
}
