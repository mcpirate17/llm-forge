//! Live worktree corpus audit; all inputs are held and rechecked before output.
use crate::{
    candidate_inputs::BoundFile,
    candidate_mutation_runner::{RunnerBundle, RunnerBundleSpec},
    candidate_source::Source,
    mutation_corpus_audit::{self, CorpusInputs},
    mutation_corpus_cli::{self, AuditArgs},
    mutation_corpus_patch::Checker,
    mutation_live_catalog,
    mutation_live_inputs::LiveInputs,
};
use anyhow::{ensure, Context, Result};
use std::path::Path;
fn relative(root: &Path, raw: &str) -> Result<String> {
    let path = Path::new(raw);
    let path = if path.is_absolute() {
        path.strip_prefix(root)
            .context("absolute corpus path must be inside repository")?
    } else {
        path
    };
    let path = path.to_str().context("corpus path must be UTF-8")?;
    conductor_native::project_paths::relative(path, "corpus path").map_err(anyhow::Error::msg)
}
fn config(
    capture: &mut LiveInputs,
) -> Result<Option<conductor_native::project_config::ProjectConfig>> {
    for (path, native) in [("forge.toml", true), ("pyproject.toml", false)] {
        if let Some(bytes) = capture.read(path)? {
            let text = std::str::from_utf8(bytes)?;
            let result = if native {
                conductor_native::project_config::parse_native(Path::new(path), text)
            } else {
                conductor_native::project_config::parse_legacy(Path::new(path), text)
            };
            return result.map(Some).map_err(anyhow::Error::msg);
        }
    }
    Ok(None)
}
fn registry_path(capture: &mut LiveInputs, root: &Path, explicit: Option<&str>) -> Result<String> {
    if let Some(path) = explicit {
        return relative(root, path);
    }
    if let Some(path) = std::env::var_os("CONDUCTOR_MUTATION_REGISTRY") {
        let path = path
            .into_string()
            .map_err(|_| anyhow::anyhow!("registry override must be UTF-8"))?;
        if !path.trim().is_empty() {
            return relative(root, &path);
        }
    }
    let config = config(capture)?;
    conductor_native::project_paths::mutation_registry_from_config(config.as_ref())
        .map_err(anyhow::Error::msg)
}
fn changed(
    args: &AuditArgs,
    root: &Path,
    capture: &mut LiveInputs,
) -> Result<(Vec<String>, Option<BoundFile>)> {
    let mut values = args.changed_file.clone();
    let held = if let Some(path) = &args.changed_files_from {
        let named = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir()?.join(path)
        };
        let resolved = named.canonicalize()?;
        ensure!(
            resolved == named,
            "changed-file list path must be canonical and contain no symlinks"
        );
        let bytes = if let Ok(relative) = resolved.strip_prefix(root) {
            let relative = relative
                .to_str()
                .context("changed list path must be UTF-8")?;
            (
                None,
                capture
                    .read(relative)?
                    .context("changed list disappeared")?
                    .to_vec(),
            )
        } else {
            let (held, bytes) =
                crate::candidate_inputs::observed(&resolved, 8 * 1024 * 1024, root)?;
            (Some(held), bytes)
        };
        values.extend(
            std::str::from_utf8(&bytes.1)?
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned),
        );
        bytes.0
    } else {
        None
    };
    Ok((values, held))
}
fn publish(
    root: &Path,
    path: &str,
    recorded: &serde_json::Value,
    capture: &mut LiveInputs,
    evidence: &mutation_corpus_audit::PublicationEvidence<'_, '_>,
) -> Result<()> {
    let expected = capture.hash(path)?;
    let verify = || -> Result<()> {
        capture.verify()?;
        evidence.verify()
    };
    verify()?;
    crate::mutation_corpus_record::write(root, path, expected.as_deref(), recorded, verify)
}
fn reject_pin_overlap(baseline: &Path, pin: Option<&Path>) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let Some(pin) = pin else {
        return Ok(());
    };
    ensure!(
        baseline != pin,
        "external baseline overlaps native runner pin"
    );
    let pin_meta = std::fs::metadata(pin)?;
    match std::fs::metadata(baseline) {
        Ok(meta) => ensure!(
            (meta.dev(), meta.ino()) != (pin_meta.dev(), pin_meta.ino()),
            "external baseline aliases native runner pin"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
fn publish_external(
    external: &crate::mutation_live_baseline::External,
    recorded: &serde_json::Value,
    capture: &LiveInputs,
    evidence: &mutation_corpus_audit::PublicationEvidence<'_, '_>,
    pin: Option<&Path>,
) -> Result<()> {
    let expected = external.hash();
    let verify = || -> Result<()> {
        external.verify()?;
        reject_pin_overlap(&external.root().join(external.name()), pin)?;
        capture.verify()?;
        evidence.verify()
    };
    verify()?;
    crate::mutation_corpus_record::write(
        external.root(),
        external.name(),
        expected.as_deref(),
        recorded,
        verify,
    )
}
fn load_baseline(
    args: &AuditArgs,
    root: &Path,
    configured: &str,
    capture: &mut LiveInputs,
) -> Result<(
    Option<crate::mutation_live_baseline::External>,
    String,
    Option<serde_json::Value>,
)> {
    let external = args
        .baseline
        .as_deref()
        .map(Path::new)
        .filter(|path| path.is_absolute() && !path.starts_with(root))
        .map(|path| crate::mutation_live_baseline::External::open(path, root))
        .transpose()?;
    let selected = if external.is_some() {
        None
    } else {
        args.baseline
            .as_deref()
            .map(|path| relative(root, path))
            .transpose()?
    };
    let path = mutation_corpus_cli::baseline_path(configured, selected.as_deref())?;
    let value = if let Some(external) = &external {
        external.value()?
    } else {
        match capture.read(&path)? {
            Some(bytes) => Some(crate::tool_schema::parse_document_with_limit(
                std::str::from_utf8(bytes)?,
                2 * 1024 * 1024,
            )?),
            None => None,
        }
    };
    Ok((external, path, value))
}
fn verify_observation(
    capture: &LiveInputs,
    bundle: Option<&RunnerBundle>,
    pin: Option<&crate::candidate_native_runner_pin::NativeRunnerPin>,
    checker: &Checker<'_>,
    list: Option<&BoundFile>,
    external: Option<&crate::mutation_live_baseline::External>,
) -> Result<()> {
    capture.verify()?;
    if let Some(bundle) = bundle {
        bundle.verify()?;
    }
    if let Some(pin) = pin {
        pin.verify()?;
    }
    checker.verify()?;
    if let Some(list) = list {
        list.verify()?;
    }
    if let Some(external) = external {
        external.verify()?;
    }
    Ok(())
}
fn capture_bundle(args: &AuditArgs, source: &Source) -> Result<Option<RunnerBundle>> {
    match (&args.mutation_runner_bundle, &args.mutation_runner_sha256) {
        (Some(manifest), Some(sha256)) => Ok(Some(RunnerBundle::capture_live(
            RunnerBundleSpec { manifest, sha256 },
            source,
        )?)),
        (None, None) => Ok(None),
        _ => anyhow::bail!("runner bundle and sha256 must be supplied together"),
    }
}
pub(crate) fn run(
    args: AuditArgs,
    interrupts: &crate::process_signals::Interrupts,
    stamp: Option<&crate::candidate_engine_identity::BuildStamp>,
) -> Result<u8> {
    ensure!(
        args.policy.is_none() && args.base_ref.is_none(),
        "live audit does not use candidate policy or base ref"
    );
    let lease = mutation_corpus_cli::baseline_lease(&args, interrupts)?;
    let source = Source::open(&args.repo)?;
    let mut capture = LiveInputs::open(source.root())?;
    let configured = registry_path(&mut capture, source.root(), None)?;
    let path = match args.registry.as_deref() {
        Some(path) => relative(source.root(), path)?,
        None => configured.clone(),
    };
    let registry = mutation_live_catalog::registry(&mut capture, &path)?;
    let catalog = mutation_live_catalog::catalog(&mut capture, &registry)?;
    let (external, baseline_path, baseline) =
        load_baseline(&args, source.root(), &configured, &mut capture)?;
    let bundle = capture_bundle(&args, &source)?;
    let native_pin = mutation_corpus_cli::capture_native_pin(
        &args,
        stamp,
        &[source.root().to_owned(), source.common_dir().to_owned()],
    )?;
    let profile = crate::mutation_corpus_git_profile::Profile::open(
        &args.git_build_profile,
        &args.git_build_profile_sha256,
        &args.git_sha256,
        source.root(),
    )?;
    let checker = Checker::open_live(source.root(), &args.git, &args.git_sha256, profile)?
        .with_interrupts(interrupts);
    let (changed, list) = changed(&args, source.root(), &mut capture)?;
    let (report, code, recorded, interpreters) = mutation_corpus_audit::audit_installed(
        &catalog,
        &mut capture,
        bundle.as_ref(),
        native_pin.as_ref(),
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
        let lease = lease.as_ref().context("baseline writer lease missing")?;
        let evidence = mutation_corpus_audit::PublicationEvidence {
            bundle: bundle.as_ref(),
            native_pin: native_pin.as_ref(),
            checker: &checker,
            list: list.as_ref(),
            lease,
            interpreters: &interpreters,
        };
        if let Some(external) = &external {
            publish_external(
                external,
                &recorded,
                &capture,
                &evidence,
                args.native_runner_pin.as_deref(),
            )?;
        } else {
            publish(
                source.root(),
                &baseline_path,
                &recorded,
                &mut capture,
                &evidence,
            )?;
        }
    } else {
        verify_observation(
            &capture,
            bundle.as_ref(),
            native_pin.as_ref(),
            &checker,
            list.as_ref(),
            external.as_ref(),
        )?;
    }
    for interpreter in &interpreters {
        interpreter.verify()?;
    }
    checker.poll()?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_baseline_rejects_pin_and_hardlink_alias() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::create_dir(&source).unwrap();
        let pin = dir.path().join("pin.json");
        std::fs::write(&pin, b"{}").unwrap();
        assert!(reject_pin_overlap(&pin, Some(&pin)).is_err());
        let alias = dir.path().join("alias.json");
        std::fs::hard_link(&pin, &alias).unwrap();
        assert!(reject_pin_overlap(&alias, Some(&pin)).is_err());
    }
}
