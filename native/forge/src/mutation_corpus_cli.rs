//! Corpus CLI. It audits exact candidate Git objects with operator-pinned tools.
use crate::{
    candidate_mutation_runner::{RunnerBundle, RunnerBundleSpec},
    candidate_policy,
    candidate_review_mutation::{inputs::Capture, registry},
    candidate_snapshot::{Limits, Snapshot},
    candidate_source::{Kind, Source},
    mutation_corpus_audit,
    mutation_corpus_patch::Checker,
};
use anyhow::{ensure, Context, Result};
use clap::Args;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct AuditArgs {
    #[arg(long, default_value = ".")]
    pub(crate) repo: PathBuf,
    /// Omit to audit the live working tree; select a kind for an immutable candidate.
    #[arg(long, value_enum)]
    pub(crate) kind: Option<Kind>,
    #[arg(long, default_value = "HEAD")]
    pub(crate) target_ref: String,
    #[arg(long)]
    pub(crate) base_ref: Option<String>,
    #[arg(long)]
    pub(crate) policy: Option<String>,
    #[arg(long)]
    pub(crate) scratch_parent: Option<PathBuf>,
    #[arg(long)]
    pub(crate) git: PathBuf,
    #[arg(long)]
    pub(crate) git_sha256: String,
    /// Owned, externally pinned operator declaration for this Git build.
    #[arg(long)]
    pub(crate) git_build_profile: PathBuf,
    #[arg(long)]
    pub(crate) git_build_profile_sha256: String,
    #[arg(long)]
    pub(crate) mutation_runner_bundle: PathBuf,
    #[arg(long)]
    pub(crate) mutation_runner_sha256: String,
    #[arg(long)]
    pub(crate) registry: Option<String>,
    #[arg(long)]
    pub(crate) baseline: Option<String>,
    #[arg(long)]
    pub(crate) changed_file: Vec<String>,
    #[arg(long)]
    pub(crate) changed_files_from: Option<PathBuf>,
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u32).range(1..=64))]
    pub(crate) max_workers: u32,
    #[arg(long)]
    pub(crate) summary: bool,
    #[arg(long)]
    pub(crate) write_baseline: bool,
}
pub(crate) fn baseline_path(registry: &str, explicit: Option<&str>) -> Result<String> {
    if let Some(path) = explicit {
        crate::candidate_source::safe_path(path.as_bytes())?;
        return Ok(path.to_owned());
    }
    let path = Path::new(registry)
        .parent()
        .unwrap_or(Path::new(""))
        .join("reproducibility_baseline.json");
    let path = path.to_str().context("baseline path must be UTF-8")?;
    crate::candidate_source::safe_path(path.as_bytes())?;
    Ok(path.to_owned())
}
fn changed_files(
    args: &AuditArgs,
    snapshot: &Snapshot,
) -> Result<(Vec<String>, Option<crate::candidate_inputs::BoundFile>)> {
    let mut changed = args.changed_file.clone();
    let held = if let Some(path) = &args.changed_files_from {
        let named = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir()?.join(path)
        };
        ensure!(
            named.canonicalize()? == named,
            "changed-file list path must be canonical and contain no symlinks"
        );
        let (held, bytes) =
            crate::candidate_inputs::observed(&named, 8 * 1024 * 1024, &snapshot.path())?;
        changed.extend(
            std::str::from_utf8(&bytes)?
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned),
        );
        Some(held)
    } else {
        None
    };
    Ok((changed, held))
}
fn publish(
    source: &Source,
    path: &str,
    recorded: &serde_json::Value,
    capture: &mut Capture<'_>,
    evidence: &mutation_corpus_audit::PublicationEvidence<'_, '_>,
) -> Result<()> {
    let expected = capture.optional_hash(path)?;
    let verify = || -> Result<()> {
        capture.verify()?;
        evidence.verify()
    };
    verify()?;
    crate::mutation_corpus_record::write(source.root(), path, expected.as_deref(), recorded, verify)
}
pub fn run(args: AuditArgs) -> Result<u8> {
    ensure!(
        (1..=64).contains(&args.max_workers),
        "max-workers must be 1..64"
    );
    let interrupts = crate::process_signals::Interrupts::install()?;
    if args.kind.is_none() {
        return crate::mutation_live_cli::run(args, &interrupts);
    }
    run_candidate(args, &interrupts)
}
fn candidate_scratch(args: &AuditArgs, source: &Source) -> Result<PathBuf> {
    let scratch = args
        .scratch_parent
        .as_ref()
        .context("candidate audit requires --scratch-parent")?;
    ensure!(scratch.is_absolute(), "scratch parent must be absolute");
    let scratch = scratch.canonicalize()?;
    ensure!(
        !scratch.starts_with(source.root()) && !source.root().starts_with(&scratch),
        "candidate scratch must be separate from the source root"
    );
    ensure!(
        git2::Repository::discover(&scratch).is_err(),
        "candidate scratch must not be inside any Git repository"
    );
    Ok(scratch)
}
pub(crate) fn baseline_lease(
    args: &AuditArgs,
    interrupts: &crate::process_signals::Interrupts,
) -> Result<Option<crate::candidate_review_lock::ReviewLock>> {
    if !args.write_baseline {
        return Ok(None);
    }
    Ok(Some(
        crate::candidate_review_lock::ReviewLock::acquire_exclusive_cancellable(
            &args.repo,
            std::time::Duration::from_secs(30),
            &mut || {
                ensure!(
                    !interrupts.interrupted()?,
                    "corpus audit interrupted while waiting for lease"
                );
                Ok(())
            },
        )?,
    ))
}
fn run_candidate(args: AuditArgs, interrupts: &crate::process_signals::Interrupts) -> Result<u8> {
    let lease = baseline_lease(&args, interrupts)?;
    let source = Source::open(&args.repo)?;
    let scratch = candidate_scratch(&args, &source)?;
    let candidate = source.resolve(
        args.kind.context("candidate kind missing")?,
        &args.target_ref,
        args.base_ref.as_deref(),
    )?;
    let output = args
        .scratch_parent
        .as_ref()
        .context("scratch parent missing")?
        .join(crate::native_id::new("forge-corpus-audit")?);
    let snapshot = Snapshot::create(&source, &candidate.tree_oid, &output, Limits::default())?;
    let result = observe_candidate(
        &args,
        &source,
        &snapshot,
        &candidate,
        scratch,
        lease.as_ref(),
        interrupts,
    );
    let cleanup = snapshot.discard();
    match (result, cleanup) {
        (Ok((code, report)), Ok(())) => {
            ensure!(
                !interrupts.interrupted()?,
                "corpus audit interrupted during snapshot cleanup"
            );
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(code)
        }
        (result, cleanup) => anyhow::bail!(
            "corpus disposition failed: audit={:?}; snapshot cleanup={:?}",
            result.err(),
            cleanup.err()
        ),
    }
}
fn observe_candidate(
    args: &AuditArgs,
    source: &Source,
    snapshot: &Snapshot,
    candidate: &crate::candidate_source::Candidate,
    scratch: PathBuf,
    lease: Option<&crate::candidate_review_lock::ReviewLock>,
    interrupts: &crate::process_signals::Interrupts,
) -> Result<(u8, serde_json::Value)> {
    ensure!(
        snapshot.report().skipped_gitlinks == 0,
        "corpus audit refuses omitted gitlinks"
    );
    let today = crate::candidate_plan::today()?;
    let policy = candidate_policy::load(&snapshot.path(), args.policy.as_deref(), &today)?;
    let mut capture = Capture::new(source, snapshot, candidate, &policy, &today)?;
    let config = capture.config()?;
    let configured =
        conductor_native::project_paths::mutation_registry_from_config(config.as_ref())
            .map_err(anyhow::Error::msg)?;
    let registry = if let Some(path) = args.registry.as_deref() {
        registry::registry_at(&mut capture, path)?
    } else {
        registry::registry(&mut capture, config.as_ref())?
    }
    .context("mutation registry absent")?;
    let catalog = registry::catalog(&mut capture, &registry, &[])?;
    let path = baseline_path(&configured, args.baseline.as_deref())?;
    let baseline = if capture.inputs.entries.contains_key(&path) {
        Some(capture.json(&path)?)
    } else {
        capture.inputs.absent(&path)?;
        None
    };
    let bundle = RunnerBundle::capture(
        RunnerBundleSpec {
            manifest: &args.mutation_runner_bundle,
            sha256: &args.mutation_runner_sha256,
        },
        source,
        snapshot,
        &[scratch],
    )?;
    let profile = crate::mutation_corpus_git_profile::Profile::open(
        &args.git_build_profile,
        &args.git_build_profile_sha256,
        &args.git_sha256,
        source.root(),
    )?;
    let checker =
        Checker::open(snapshot, &args.git, &args.git_sha256, profile)?.with_interrupts(interrupts);
    let (changed, held_list) = changed_files(args, snapshot)?;
    let (report, code, recorded, interpreters) = mutation_corpus_audit::audit(
        &catalog,
        &mut capture,
        &bundle,
        &checker,
        mutation_corpus_audit::AuditOptions {
            changed: &changed,
            baseline: baseline.as_ref(),
            root: source.root(),
            summary: args.summary,
            record: args.write_baseline,
        },
    )?;
    for interpreter in &interpreters {
        interpreter.verify()?;
    }
    if args.write_baseline {
        publish(
            source,
            &path,
            &recorded,
            &mut capture,
            &mutation_corpus_audit::PublicationEvidence {
                bundle: &bundle,
                checker: &checker,
                lease: lease.context("baseline writer lease missing")?,
                list: held_list.as_ref(),
                interpreters: &interpreters,
            },
        )?;
    }
    if let Some(held) = &held_list {
        held.verify()?;
    }
    checker.poll()?;
    Ok((code, report))
}

#[cfg(test)]
#[path = "mutation_corpus_cli_tests.rs"]
mod tests;
