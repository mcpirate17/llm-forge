//! Full-source binding for an explicitly authorized offline Cargo workspace.
use crate::{
    candidate_inputs::{self, BoundFile},
    mutation_campaign::Campaign,
    mutation_cargo_seed::{HeldSeed, StagedSeed},
    mutation_execution_grants::Grants,
    mutation_snapshot::{self, Snapshot, SnapshotSelection},
    mutation_workspace_metadata, mutation_workspace_policy as policy,
};
use anyhow::{ensure, Context, Result};
use conductor_native::{mutation_cargo_supporting_binding::SupportingInputs, project_config};
use std::{
    cell::OnceCell,
    collections::BTreeSet,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub(crate) struct Inputs {
    root: PathBuf,
    manifest: String,
    lockfile: String,
    package: String,
    held: Vec<BoundFile>,
    absent: Vec<PathBuf>,
    config: Option<project_config::ProjectConfig>,
    selection: SnapshotSelection,
    identity: SupportingInputs,
    seed: Option<HeldSeed>,
    tests: OnceCell<mutation_workspace_metadata::Selection>,
}

fn config(
    root: &Path,
    held: &mut Vec<BoundFile>,
    absent: &mut Vec<PathBuf>,
) -> Result<Option<project_config::ProjectConfig>> {
    for name in ["forge.toml", "pyproject.toml"] {
        let path = root.join(name);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => absent.push(path),
            Err(error) => return Err(error.into()),
            Ok(_) => {
                let (file, bytes) = candidate_inputs::repository_live(&path, 65536)?;
                let text = std::str::from_utf8(&bytes)?;
                let parsed = if name == "forge.toml" {
                    project_config::parse_native(&path, text)
                } else {
                    project_config::parse_legacy(&path, text)
                };
                held.push(file);
                return parsed.map(Some).map_err(anyhow::Error::msg);
            }
        }
    }
    Ok(None)
}

fn campaign(campaign: &Campaign) -> Result<(String, String)> {
    ensure!(
        campaign.generated_value.is_none()
            && campaign.language == "rust"
            && campaign.mutation_engine == "cargo-mutants",
        "unsupported workspace engine"
    );
    let selector = conductor_native::mutation_workspace_selector::parse(&campaign.options)
        .map_err(anyhow::Error::msg)?;
    ensure!(
        matches!(
            campaign.option_text("execution_profile").as_deref(),
            Some("cargo-workspace-v2" | "cargo-workspace-v3")
        ),
        "workspace profile marker required"
    );
    ensure!(
        campaign.environment.is_empty()
            && campaign.jobs == 1
            && campaign.exclude.is_empty()
            && campaign.operators.is_empty(),
        "workspace execution requires one worker, no environment overrides or generator exclusions"
    );
    ensure!(
        campaign.options.keys().all(|key| [
            "manifest_path",
            "package",
            "package_root",
            "execution_profile",
            "workspace_selector"
        ]
        .contains(&key.as_str())),
        "unsupported workspace generator option"
    );
    let manifest = campaign
        .option_text("manifest_path")
        .context("workspace manifest missing")?;
    let package = campaign
        .option_text("package")
        .context("workspace package missing")?;
    conductor_native::mutation_snapshot_inputs::validate_path(&manifest)
        .map_err(anyhow::Error::msg)?;
    ensure!(
        Path::new(&manifest)
            .file_name()
            .is_some_and(|v| v == "Cargo.toml"),
        "workspace manifest must name Cargo.toml"
    );
    let parent = Path::new(&manifest)
        .parent()
        .context("manifest parent missing")?
        .to_str()
        .context("manifest is not UTF-8")?;
    ensure!(
        campaign
            .option_text("package_root")
            .as_deref()
            .unwrap_or("")
            == parent,
        "workspace package root differs from manifest"
    );
    let mut expected = vec![
        "cargo".into(),
        "test".into(),
        "--manifest-path".into(),
        manifest.clone(),
        "--package".into(),
        package.clone(),
    ];
    if let Some(selector) = &selector {
        expected.extend(selector.cargo_args());
        expected.extend(selector.libtest_args());
    }
    ensure!(
        campaign.test_argv == expected,
        "unsupported workspace test command"
    );
    verify_source_scope(campaign, parent)?;
    Ok((manifest, package))
}

fn verify_source_scope(campaign: &Campaign, parent: &str) -> Result<()> {
    let selected: BTreeSet<String> = campaign
        .source
        .iter()
        .map(|path| {
            if parent.is_empty() {
                path.clone()
            } else {
                format!("{parent}/{path}")
            }
        })
        .collect();
    ensure!(
        selected.len() == campaign.source.len()
            && !selected.is_empty()
            && selected == campaign.source_sha256.keys().cloned().collect(),
        "workspace source scope differs from pins"
    );
    for source in &campaign.source {
        conductor_native::mutation_snapshot_inputs::validate_path(source)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            !source.contains(['*', '?', '[', ']', '{', '}', '\\']),
            "workspace source must be a literal path without glob metacharacters"
        );
    }
    Ok(())
}

fn no_cargo_redirection(root: &Path, manifest: &str) -> Result<()> {
    let path = root.join(manifest);
    for directory in path
        .parent()
        .context("manifest parent missing")?
        .ancestors()
    {
        ensure!(
            directory.starts_with(root),
            "Cargo configuration traversal escaped snapshot"
        );
        for name in [
            ".cargo/config",
            ".cargo/config.toml",
            "rust-toolchain",
            "rust-toolchain.toml",
        ] {
            let path = directory.join(name);
            match fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
                Ok(_) => anyhow::bail!(
                    "workspace profile refuses active Cargo/toolchain configuration: {}",
                    path.display()
                ),
            }
        }
        if directory == root {
            break;
        }
    }
    Ok(())
}

/// Check the actual command cwd and every Cargo search ancestor, including the
/// private scratch parent and filesystem root. This is point-in-time evidence.
pub(crate) fn verify_command_ancestors(cwd: &Path) -> Result<()> {
    ensure!(
        cwd.is_absolute() && cwd.canonicalize()? == cwd,
        "Cargo command cwd must be canonical"
    );
    for directory in cwd.ancestors() {
        let meta = fs::symlink_metadata(directory)?;
        ensure!(
            meta.is_dir() && !meta.file_type().is_symlink(),
            "Cargo command ancestor must be a real directory"
        );
        let cargo = directory.join(".cargo");
        match fs::symlink_metadata(&cargo) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
            Ok(m) => ensure!(
                m.is_dir() && !m.file_type().is_symlink(),
                "Cargo configuration directory must not be aliased"
            ),
        }
        for name in [
            ".cargo/config",
            ".cargo/config.toml",
            "rust-toolchain",
            "rust-toolchain.toml",
        ] {
            let path = directory.join(name);
            match fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
                Ok(_) => anyhow::bail!(
                    "Cargo command ancestor contains configuration: {}",
                    path.display()
                ),
            }
        }
    }
    Ok(())
}

/// An unseeded workspace has no external dependencies; no executable/config inputs
/// may appear in its Cargo home. These bounded lock/database files are outputs.
pub(crate) fn verify_unseeded_home(root: &Path) -> Result<()> {
    ensure!(
        root.canonicalize()? == root,
        "unseeded Cargo home became aliased"
    );
    let mode = fs::symlink_metadata(root)?;
    ensure!(
        mode.is_dir() && mode.mode() & 0o077 == 0,
        "Cargo home must remain private"
    );
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().context("Cargo output name is not UTF-8")?;
        ensure!(
            [
                ".package-cache",
                ".package-cache-mutate",
                ".global-cache",
                ".global-cache-journal",
                ".global-cache-wal",
                ".global-cache-shm"
            ]
            .contains(&name),
            "unexpected unseeded Cargo home input: {name}"
        );
        let metadata = fs::symlink_metadata(entry.path())?;
        ensure!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.nlink() == 1
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.len() <= 256 * 1024 * 1024,
            "unsafe Cargo output file"
        );
    }
    Ok(())
}

impl Inputs {
    pub(crate) fn capture(c: &Campaign, root: &Path, grants: &Grants) -> Result<Self> {
        let (manifest, package) = campaign(c)?;
        no_cargo_redirection(root, &manifest)?;
        let grant = grants
            .cargo_workspace
            .as_ref()
            .context("workspace grant required")?;
        let (lock, lock_bytes) =
            candidate_inputs::repository_live(&root.join(&grant.lockfile), 1024 * 1024)?;
        let (manifest_file, _) =
            candidate_inputs::repository_live(&root.join(&manifest), 1024 * 1024)?;
        let control_path = std::path::absolute(&c.manifest_path)?;
        let (control, bytes) = candidate_inputs::repository_live(&control_path, 2 * 1024 * 1024)?;
        ensure!(
            crate::mutation_campaign::sha256_hex(&bytes) == c.manifest_sha256,
            "campaign manifest changed before admission"
        );
        let mut held = vec![lock, manifest_file, control];
        let mut absent = Vec::new();
        let config = config(root, &mut held, &mut absent)?;
        let manifest_relative = control_path
            .strip_prefix(root)?
            .to_str()
            .context("campaign path UTF-8")?;
        policy::manifest_in_namespace(config.as_ref(), manifest_relative)?;
        let mut protected: Vec<String> = c
            .source_sha256
            .keys()
            .chain(c.test_sha256.keys())
            .cloned()
            .collect();
        protected.extend([manifest.clone(), grant.lockfile.clone()]);
        let selection = policy::selection(config.as_ref(), &protected)?;
        let seed = match &grant.seed {
            Some(seed) => Some(HeldSeed::capture(
                seed,
                &lock_bytes,
                grants.program_sha256("cargo")?,
            )?),
            None => {
                let value: toml::Value = std::str::from_utf8(&lock_bytes)?.parse()?;
                let packages = value
                    .get("package")
                    .and_then(toml::Value::as_array)
                    .context("workspace lock packages missing")?;
                ensure!(
                    !packages.is_empty()
                        && packages.len() <= 4096
                        && packages.iter().all(|p| p.get("source").is_none()),
                    "workspace external packages require an explicit verified seed"
                );
                None
            }
        };
        let identity =
            mutation_snapshot::live_input_identity(root, &selection).map_err(anyhow::Error::msg)?;
        let inputs = Self {
            root: root.into(),
            manifest,
            lockfile: grant.lockfile.clone(),
            package,
            held,
            absent,
            config,
            selection,
            identity,
            seed,
            tests: OnceCell::new(),
        };
        inputs.verify(None)?;
        Ok(inputs)
    }
    pub(crate) fn receipt(&self, relative: &str) -> Result<()> {
        policy::receipt_in_namespace(self.config.as_ref(), relative)
    }
    pub(crate) fn populate(&self, snapshot: &mut Snapshot) -> Result<()> {
        let observed = snapshot
            .populate_bound(&self.root, &self.selection)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            observed == self.identity,
            "workspace snapshot differs from admitted inputs"
        );
        snapshot
            .verify_bound(&self.root, &self.selection, &self.identity)
            .map_err(anyhow::Error::msg)
    }
    pub(crate) fn verify(&self, snapshot: Option<&Path>) -> Result<()> {
        for file in &self.held {
            file.verify()?;
        }
        for path in &self.absent {
            match fs::symlink_metadata(path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
                Ok(_) => anyhow::bail!("workspace configuration appeared: {}", path.display()),
            }
        }
        ensure!(
            mutation_snapshot::live_input_identity(&self.root, &self.selection)
                .map_err(anyhow::Error::msg)?
                == self.identity,
            "workspace source inputs changed"
        );
        if let Some(snapshot) = snapshot {
            no_cargo_redirection(snapshot, &self.manifest)?;
            ensure!(
                mutation_snapshot::materialized_input_identity(snapshot, &self.selection)
                    .map_err(anyhow::Error::msg)?
                    == self.identity,
                "workspace snapshot inputs changed"
            );
        }
        if let Some(seed) = &self.seed {
            seed.verify()?;
        }
        Ok(())
    }
    pub(crate) fn identity(&self) -> &SupportingInputs {
        &self.identity
    }
    pub(crate) fn stage(&self, path: &Path) -> Result<Option<StagedSeed>> {
        self.seed.as_ref().map(|seed| seed.stage(path)).transpose()
    }
    pub(crate) fn metadata_argv(&self) -> Vec<String> {
        vec![
            "cargo".into(),
            "metadata".into(),
            "--manifest-path".into(),
            self.manifest.clone(),
            "--format-version=1".into(),
            "--locked".into(),
            "--offline".into(),
        ]
    }
    pub(crate) fn admit_metadata(
        &self,
        bytes: &[u8],
        root: &Path,
        home: &Path,
        c: &Campaign,
    ) -> Result<()> {
        let selector = conductor_native::mutation_workspace_selector::parse(&c.options)
            .map_err(anyhow::Error::msg)?;
        let selection = mutation_workspace_metadata::validate(
            bytes,
            root,
            home,
            &self.lockfile,
            &self.manifest,
            &self.package,
            &c.source_sha256,
            &c.test_sha256,
            selector.as_ref(),
        )?;
        self.tests
            .set(selection)
            .map_err(|_| anyhow::anyhow!("workspace metadata already admitted"))
    }
    pub(crate) fn tests(&self) -> Result<&mutation_workspace_metadata::Selection> {
        self.tests
            .get()
            .context("workspace metadata admission required")
    }
}

#[cfg(test)]
#[path = "mutation_workspace_inputs_tests.rs"]
mod tests;
