//! Captured candidate configuration, recorded drivers and immutable target plan.
use crate::{
    candidate_execution_plan::Selection,
    candidate_execution_workspace::Workspace,
    candidate_inputs::{self, BoundFile, DirectoryBinding},
    candidate_snapshot::Snapshot,
    candidate_source::lines::RangeScope,
    candidate_test_selection::inputs::Inputs,
};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{OpenOptions, OpenOptionsExt};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProbeConfig {
    waivers: Option<String>,
    ledger: Option<String>,
    report: Option<String>,
    run_artifacts: Option<String>,
    per_module_timeout: Option<f64>,
    jobs: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordConfig {
    store: String,
    #[serde(default = "yes")]
    lines: bool,
    #[serde(default)]
    environment: Vec<String>,
}
fn yes() -> bool {
    true
}
pub(crate) struct Captured<'a> {
    inputs: Inputs<'a>,
    record: Option<BoundFile>,
    record_path: Option<PathBuf>,
    pub targets: Vec<slop_core::gate_types::Target>,
    pub waivers: Option<Vec<u8>>,
    pub timeout_seconds: f64,
    pub jobs: usize,
    pub evidence: serde_json::Value,
    pub artifacts: Option<Artifacts>,
}
impl Captured<'_> {
    pub(crate) fn verify(&self) -> Result<()> {
        self.inputs.verify()?;
        if let Some(artifacts) = &self.artifacts {
            artifacts.binding.verify()?;
        }
        if let Some(record) = &self.record {
            record.verify()?;
        }
        if let Some(path) = &self.record_path {
            no_wal(path)?;
        }
        Ok(())
    }
}
pub(crate) struct Artifacts {
    binding: DirectoryBinding,
    directory: PathBuf,
}
impl Artifacts {
    pub(crate) fn publish(&self, summary: &[u8], cleanup_complete: bool) -> Result<PathBuf> {
        ensure!(
            cleanup_complete,
            "cannot publish equivalence summary before cleanup"
        );
        self.binding.verify()?;
        let name = format!("gate-{}.json", crate::resource_id("equivalence")?);
        let path = self.directory.join(name);
        ensure!(!path.exists(), "equivalence artifact already exists");
        crate::candidate_report_store::publish(&path, summary)?;
        self.binding.verify()?;
        Ok(path)
    }
}
fn admit_artifacts(
    configured: Option<&str>,
    supplied: Option<&Path>,
    source: &Path,
    snapshot: &Path,
    scratch: &Path,
) -> Result<Option<Artifacts>> {
    match (configured, supplied) {
        (None, None) => Ok(None),
        (Some(configured), Some(supplied)) => {
            let configured = if let Some(rest) = configured.strip_prefix("~/") {
                PathBuf::from(std::env::var_os("HOME").context("HOME required for artifacts")?)
                    .join(rest)
            } else {
                source.join(configured)
            };
            let resolved = configured
                .canonicalize()
                .context("resolve configured artifacts directory")?;
            ensure!(
                configured == resolved,
                "configured artifact path is not canonical or contains a symlink"
            );
            ensure!(
                resolved == supplied,
                "artifact grant differs from captured configuration"
            );
            ensure!(
                !resolved.starts_with(snapshot) && !resolved.starts_with(scratch),
                "artifact directory is inside candidate or scratch"
            );
            let binding = DirectoryBinding::open(supplied, Some(snapshot))?;
            Ok(Some(Artifacts {
                binding,
                directory: supplied.to_owned(),
            }))
        }
        (Some(_), None) => {
            anyhow::bail!("configured run_artifacts requires an explicit output directory grant")
        }
        (None, Some(_)) => anyhow::bail!("artifact grant is not named by captured configuration"),
    }
}
fn no_wal(path: &Path) -> Result<()> {
    for suffix in ["-wal", "-journal"] {
        let mut candidate = path.as_os_str().to_os_string();
        candidate.push(suffix);
        match std::fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
            Ok(_) => anyhow::bail!("record store has an active SQLite sidecar; capture a consistent standalone snapshot"),
        }
    }
    Ok(())
}
fn config(inputs: &mut Inputs<'_>) -> Result<(ProbeConfig, Option<RecordConfig>)> {
    if !inputs.entries.contains_key("pyproject.toml") {
        inputs.absent("pyproject.toml")?;
        return Ok((ProbeConfig::default(), None));
    }
    let text = inputs.read("pyproject.toml")?;
    let doc: toml::Value = toml::from_str(text)?;
    let forge = doc.get("tool").and_then(|v| v.get("forge"));
    let probe: ProbeConfig = forge
        .and_then(|v| v.get("equivalence_probe"))
        .cloned()
        .map(toml::Value::try_into)
        .transpose()?
        .unwrap_or_default();
    let record: Option<RecordConfig> = forge
        .and_then(|v| v.get("test_record"))
        .cloned()
        .map(toml::Value::try_into)
        .transpose()?;
    Ok((probe, record))
}
fn record_path(config: &RecordConfig, root: &Path) -> Result<PathBuf> {
    let path = if let Some(rest) = config.store.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").context("HOME required for record store")?)
            .join(rest)
    } else {
        root.join(&config.store)
    };
    path.canonicalize()
        .context("resolve configured record store")
}
fn changes(
    selection: &Selection,
    source: &crate::candidate_source::Source,
) -> Result<Vec<(String, Option<Vec<usize>>)>> {
    let paths: Vec<_> = selection
        .candidate
        .changes
        .iter()
        .filter(|c| {
            !c.deleted()
                && c.classes.iter().any(|v| v == "python")
                && !c.classes.iter().any(|v| v == "test")
        })
        .map(|c| c.path.as_str())
        .collect();
    let ranges = source.changed_ranges(&selection.candidate, &paths)?;
    let mut result = Vec::new();
    let mut count = 0usize;
    for file in ranges.files {
        if file.scope != RangeScope::Changed {
            continue;
        }
        let mut lines = Vec::new();
        for range in file.ranges {
            count = count
                .checked_add((range.end - range.start + 1) as usize)
                .context("line count overflow")?;
            ensure!(count <= 1_000_000, "changed lines exceed budget");
            lines.extend((range.start..=range.end).map(|line| line as usize));
        }
        if !lines.is_empty() {
            result.push((file.path, Some(lines)));
        }
    }
    Ok(result)
}
fn bind_python_tree(inputs: &mut Inputs<'_>) -> Result<()> {
    let paths: Vec<_> = inputs
        .entries
        .keys()
        .filter(|p| p.ends_with(".py"))
        .cloned()
        .collect();
    ensure!(paths.len() <= 4096, "Python inventory exceeds 4096 paths");
    for path in paths {
        inputs.read(&path)?;
    }
    Ok(())
}
fn store_copy(
    execution: &Selection,
    snapshot: &Snapshot,
    record: Option<&[u8]>,
    changed: &[(String, Option<Vec<usize>>)],
    timeout: f64,
) -> Result<Vec<slop_core::gate_types::Target>> {
    let source = execution.source()?;
    let workspace = Workspace::new(&source, &execution.candidate.tree_oid, &execution.grants)?;
    let path = workspace.runtime().join("equivalence-record.sqlite");
    if let Some(record) = record {
        let dir = workspace.artifact_root()?;
        let mut file = dir.open_with(
            "equivalence-record.sqlite",
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW),
        )?;
        file.write_all(record)?;
        file.sync_all()?;
        workspace.verify_artifact_root(&dir)?;
    }
    let result = conductor_native::equivalence_targets::targets(
        &snapshot.path(),
        record.map(|_| path.as_path()),
        changed,
        timeout,
    );
    workspace.discard()?;
    result
}
type RecordCapture = (Option<BoundFile>, Option<Vec<u8>>, Option<PathBuf>);
fn capture_record(
    config: Option<RecordConfig>,
    supplied_store: Option<(&Path, &str)>,
    root: &Path,
    snapshot: &Snapshot,
) -> Result<RecordCapture> {
    let mut record = None;
    let mut record_bytes = None;
    let mut path = None;
    if let Some(config) = config.filter(|config| config.lines) {
        let _environment = &config.environment;
        let expected = record_path(&config, root)?;
        let (supplied, sha256) = supplied_store
            .context("configured recorded store needs an operator path and SHA-256 grant")?;
        ensure!(
            supplied == expected,
            "granted record store must be the exact canonical configured path"
        );
        no_wal(&expected)?;
        let (held, bytes) =
            candidate_inputs::external(&expected, sha256, 32 * 1024 * 1024, &snapshot.path())?;
        no_wal(&expected)?;
        held.verify()?;
        record = Some(held);
        record_bytes = Some(bytes);
        path = Some(expected);
    } else {
        ensure!(
            supplied_store.is_none(),
            "record store grant supplied without enabled captured configuration"
        );
    }
    Ok((record, record_bytes, path))
}
/// Captures configured drivers and all candidate Python source the native selector/index may read.
pub(crate) fn capture<'a>(
    execution: &'a Selection,
    snapshot: &'a Snapshot,
    supplied_store: Option<(&Path, &str)>,
    supplied_artifacts: Option<&Path>,
    granted_jobs: usize,
) -> Result<Captured<'a>> {
    let source = execution.source()?;
    let mut inputs = Inputs::new(
        &source,
        snapshot,
        &execution.candidate,
        &execution.policy,
        execution.date(),
    )?;
    let changed = changes(execution, &source)?;
    if changed.is_empty() {
        return Ok(Captured {
            inputs,
            record: None,
            record_path: None,
            targets: Vec::new(),
            waivers: None,
            timeout_seconds: 180.0,
            jobs: 1,
            artifacts: None,
            evidence: serde_json::json!({"candidate_tree":execution.candidate.tree_oid,
                "changed":[],"target_count":0,"execution_verified":false}),
        });
    }
    let (probe, record_config) = config(&mut inputs)?;
    let timeout = probe.per_module_timeout.unwrap_or(180.0);
    ensure!(
        timeout.is_finite() && (0.001..=3600.0).contains(&timeout),
        "invalid equivalence module timeout"
    );
    let jobs = probe.jobs.unwrap_or(granted_jobs).min(granted_jobs);
    ensure!((1..=32).contains(&jobs), "invalid equivalence job count");
    let waivers = if let Some(path) = &probe.waivers {
        crate::candidate_source::safe_path(path.as_bytes())?;
        Some(inputs.read_bytes_with_limit(path, 1024 * 1024)?.to_vec())
    } else {
        None
    };
    let artifacts = admit_artifacts(
        probe.run_artifacts.as_deref(),
        supplied_artifacts,
        source.root(),
        &snapshot.path(),
        &execution.grants.scratch_parent,
    )?;
    let _other_config = (&probe.ledger, &probe.report);
    let (record, record_bytes, path) =
        capture_record(record_config, supplied_store, source.root(), snapshot)?;
    bind_python_tree(&mut inputs)?;
    let targets = store_copy(
        execution,
        snapshot,
        record_bytes.as_deref(),
        &changed,
        timeout,
    )?;
    let evidence = serde_json::json!({"candidate_tree":execution.candidate.tree_oid,
        "changed":changed, "target_count":targets.len(),
        "record_sha256":record_bytes.as_ref().map(|bytes| format!("{:x}",Sha256::digest(bytes))),
        "waivers_sha256":waivers.as_ref().map(|bytes| format!("{:x}",Sha256::digest(bytes))),
        "execution_verified":false});
    let result = Captured {
        inputs,
        record,
        record_path: path,
        targets,
        waivers,
        timeout_seconds: timeout,
        jobs,
        evidence,
        artifacts,
    };
    result.verify()?;
    Ok(result)
}

#[cfg(test)]
mod artifact_tests {
    use super::*;
    struct Tree {
        root: PathBuf,
        source: PathBuf,
        snapshot: PathBuf,
        scratch: PathBuf,
        output: PathBuf,
    }
    impl Tree {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(crate::resource_id("equivalence-artifact-test").unwrap());
            let source = root.join("source");
            let snapshot = root.join("snapshot");
            let scratch = root.join("scratch");
            let output = source.join("output");
            std::fs::create_dir_all(&output).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::create_dir_all(&snapshot).unwrap();
            std::fs::create_dir_all(&scratch).unwrap();
            Self {
                root,
                source,
                snapshot,
                scratch,
                output,
            }
        }
        fn admit(
            &self,
            configured: Option<&str>,
            output: Option<&Path>,
        ) -> Result<Option<Artifacts>> {
            admit_artifacts(
                configured,
                output,
                &self.source,
                &self.snapshot,
                &self.scratch,
            )
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }
    #[test]
    fn configured_output_requires_exact_grant_and_publishes_after_cleanup() {
        let tree = Tree::new();
        assert!(tree.admit(Some("output"), None).is_err());
        assert!(tree.admit(Some("output"), Some(&tree.source)).is_err());
        assert!(tree.admit(None, Some(&tree.output)).is_err());
        let output = tree
            .admit(Some("output"), Some(&tree.output))
            .unwrap()
            .unwrap();
        assert!(output.publish(b"{}", false).is_err());
        assert_eq!(std::fs::read_dir(&tree.output).unwrap().count(), 0);
        let path = output.publish(b"{}", true).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"{}");
    }
    #[test]
    fn symlink_or_replaced_output_is_rejected() {
        use std::os::unix::fs::symlink;
        let tree = Tree::new();
        let alias = tree.source.join("alias");
        symlink(&tree.output, &alias).unwrap();
        assert!(tree.admit(Some("alias"), Some(&alias)).is_err());
        assert!(tree.admit(Some("alias"), Some(&tree.output)).is_err());
        let output = tree
            .admit(Some("output"), Some(&tree.output))
            .unwrap()
            .unwrap();
        std::fs::rename(&tree.output, tree.source.join("moved")).unwrap();
        std::fs::create_dir(&tree.output).unwrap();
        assert!(output.publish(b"{}", true).is_err());
        assert_eq!(std::fs::read_dir(&tree.output).unwrap().count(), 0);
    }
}

#[cfg(test)]
#[path = "candidate_equivalence_capture_tests.rs"]
mod capture_tests;
