//! Operator-pinned compatibility runner sources, never candidate-supplied identity.
use crate::{
    candidate_inputs::{self, BoundFile, DirectoryBinding},
    candidate_snapshot::Snapshot,
    candidate_source::Source,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
pub(crate) const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_PROBE_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_TOTAL_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
pub(crate) const COMPONENTS: [&str; 11] = [
    "conductor/mutation_campaign_model.py",
    "conductor/mutation_patch_apply.py",
    "conductor/mutation_receipt_build.py",
    "conductor/mutation_scope.py",
    "conductor/mutation_testing.py",
    "conductor/mutation_testing_support.py",
    "conductor/mutation_value.py",
    "conductor/snapshot_worktree.py",
    "tooling/native/conductor-native/src/mutation_evidence.rs",
    "tooling/native/conductor-native/src/mutation_manifest.rs",
    "tooling/native/conductor-native/src/mutation_receipt.rs",
];
pub(crate) const BINDINGS: [&str; 5] = [
    "conductor/mutation_engine_generated.py",
    "conductor/mutation_run_scope.py",
    "conductor/mutation_engine_cargo.py",
    "conductor/mutation_engine_fest.py",
    "conductor/mutation_engine_mull.py",
];
pub(crate) struct RunnerBundleSpec<'a> {
    pub manifest: &'a Path,
    pub sha256: &'a str,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FilePin {
    pub(crate) path: PathBuf,
    pub(crate) sha256: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub(crate) schema_version: u32,
    pub(crate) symbol_hash_protocol: String,
    pub(crate) package_root: PathBuf,
    pub(crate) components: BTreeMap<String, FilePin>,
    pub(crate) bindings: BTreeMap<String, FilePin>,
    pub(crate) lineage: Option<FilePin>,
    pub(crate) external_invariant_probe: Option<FilePin>,
}
impl Manifest {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.symbol_hash_protocol == conductor_native::candidate_python_symbol_hash::PROTOCOL,
            "mutation runner bundle symbol protocol must be CPython3.12"
        );
        ensure!(
            self.schema_version == 1,
            "mutation runner bundle schema must be1"
        );
        ensure!(
            self.components.len() == COMPONENTS.len()
                && COMPONENTS.iter().all(|key| self.components.contains_key(*key)),
            "mutation runner bundle component inventory differs from shipped RUNNER_COMPONENT_PATHS"
        );
        ensure!(
            self.bindings.len() == BINDINGS.len()
                && BINDINGS.iter().all(|key| self.bindings.contains_key(*key)),
            "mutation runner bundle requires exact core/scope/adapter inventory"
        );
        Ok(())
    }
}
#[derive(serde::Serialize)]
pub(crate) struct Probe {
    pub(crate) path: PathBuf,
    pub(crate) sha256: String,
}
pub(crate) struct RunnerBundle {
    root: DirectoryBinding,
    held: Vec<BoundFile>,
    pub runner: Value,
    pub bindings: Value,
    pub lineage: Option<Value>,
    pub probe: Option<Probe>,
    pub evidence: Value,
    digest: String,
}
impl RunnerBundle {
    pub fn capture(
        spec: RunnerBundleSpec<'_>,
        source: &Source,
        snapshot: &Snapshot,
        scratch_exclusions: &[PathBuf],
    ) -> Result<Self> {
        excluded(spec.manifest, source, snapshot, scratch_exclusions)?;
        let (manifest, bytes) = candidate_inputs::external(
            spec.manifest,
            spec.sha256,
            MAX_MANIFEST_BYTES,
            &snapshot.path(),
        )?;
        let parsed = crate::tool_schema::parse_document(std::str::from_utf8(&bytes)?)?;
        let payload: Manifest = serde_json::from_value(parsed)?;
        payload.validate()?;
        excluded(&payload.package_root, source, snapshot, scratch_exclusions)?;
        let root = DirectoryBinding::open(&payload.package_root, Some(&snapshot.path()))?;
        let mut bundle = Self {
            root,
            held: vec![manifest],
            runner: Value::Null,
            bindings: json!({}),
            lineage: None,
            probe: None,
            evidence: json!({}),
            digest: String::new(),
        };
        let mut facts = BTreeMap::new();
        let mut rows = Vec::new();
        let mut total = bytes.len();
        for (key, pin) in &payload.components {
            let bytes = bundle.file(
                pin,
                &payload.package_root,
                snapshot,
                &mut total,
                MAX_SOURCE_BYTES,
            )?;
            let hash = hex(&bytes);
            facts.insert(key.clone(), hash.clone());
            rows.push(
                json!({"logical_path":key,"path":pin.path,"sha256":hash,"bytes":bytes.len()}),
            );
        }
        for (key, pin) in &payload.bindings {
            let bytes = bundle.file(
                pin,
                &payload.package_root,
                snapshot,
                &mut total,
                MAX_SOURCE_BYTES,
            )?;
            bundle.bindings[key] = json!(hex(&bytes));
            rows.push(json!({"logical_path":key,"path":pin.path,"sha256":hex(&bytes),"bytes":bytes.len()}));
        }
        if let Some(pin) = &payload.lineage {
            let bytes = bundle.file(
                pin,
                &payload.package_root,
                snapshot,
                &mut total,
                MAX_SOURCE_BYTES,
            )?;
            bundle.lineage = Some(crate::tool_schema::parse_document(std::str::from_utf8(
                &bytes,
            )?)?);
        }
        if let Some(pin) = &payload.external_invariant_probe {
            let bytes = bundle.file(
                pin,
                &payload.package_root,
                snapshot,
                &mut total,
                MAX_PROBE_BYTES,
            )?;
            bundle.probe = Some(Probe {
                path: pin.path.clone(),
                sha256: hex(&bytes),
            });
        }
        bundle.runner = json!({"mutation_testing_sha256":facts["conductor/mutation_testing.py"],"components":facts,"error":null});
        bundle.evidence = json!({"manifest":spec.manifest,"manifest_sha256":hex(&bytes),"package_root":payload.package_root,
            "symbol_hash_protocol":payload.symbol_hash_protocol,"files":rows,"bytes":total,"scope":"operator-pinned compatibility source bundle; not execution authentication",
            "execution_verified":false,"approval":false});
        bundle.digest = crate::candidate_test_selection::digest_json(&(
            &bundle.runner,
            &bundle.bindings,
            &bundle.lineage,
            &bundle.evidence,
            &bundle.probe,
        ))?;
        bundle.verify()?;
        Ok(bundle)
    }
    fn file(
        &mut self,
        pin: &FilePin,
        root: &Path,
        snapshot: &Snapshot,
        total: &mut usize,
        maximum: usize,
    ) -> Result<Vec<u8>> {
        ensure!(
            pin.path.is_absolute() && pin.path.starts_with(root),
            "runner component must be inside canonical installed package root"
        );
        let (held, bytes) =
            candidate_inputs::external(&pin.path, &pin.sha256, maximum, &snapshot.path())?;
        *total = total
            .checked_add(bytes.len())
            .context("runner byte overflow")?;
        ensure!(
            *total <= MAX_TOTAL_BYTES,
            "mutation runner bundle exceeds32MiB"
        );
        self.held.push(held);
        Ok(bytes)
    }
    pub fn verify(&self) -> Result<()> {
        self.root.verify()?;
        for held in &self.held {
            held.verify()?;
        }
        ensure!(
            crate::candidate_test_selection::digest_json(&(
                &self.runner,
                &self.bindings,
                &self.lineage,
                &self.evidence,
                &self.probe
            ))? == self.digest,
            "mutation runner observation output changed"
        );
        Ok(())
    }
}
fn excluded(path: &Path, source: &Source, snapshot: &Snapshot, scratch: &[PathBuf]) -> Result<()> {
    ensure!(
        path.is_absolute() && path.canonicalize()? == path,
        "runner bundle paths must be canonical absolute paths"
    );
    ensure!(
        !path.starts_with(source.root())
            && !path.starts_with(source.common_dir())
            && !path.starts_with(snapshot.path())
            && !source.root().starts_with(path)
            && !source.common_dir().starts_with(path)
            && !snapshot.path().starts_with(path),
        "runner bundle must be outside candidate/source/Git metadata"
    );
    for root in scratch {
        ensure!(
            !path.starts_with(root.canonicalize()?),
            "runner bundle must be outside scratch roots"
        );
    }
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
