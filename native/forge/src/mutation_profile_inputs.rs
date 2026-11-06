//! Complete captured-input binding shared by confined external engine profiles.
use crate::{candidate_inputs::BoundFile, mutation_campaign::Campaign, mutation_snapshot};
use anyhow::{ensure, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) struct Inputs {
    root: PathBuf,
    selection: mutation_snapshot::SnapshotSelection,
    identity: conductor_native::mutation_cargo_supporting_binding::SupportingInputs,
    config: Option<conductor_native::project_config::ProjectConfig>,
    held: Vec<BoundFile>,
    absent: Vec<PathBuf>,
}

fn project_config(
    root: &Path,
    held: &mut Vec<BoundFile>,
    absent: &mut Vec<PathBuf>,
) -> Result<Option<conductor_native::project_config::ProjectConfig>> {
    for name in ["forge.toml", "pyproject.toml"] {
        let path = root.join(name);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => absent.push(path),
            Err(error) => return Err(error.into()),
            Ok(_) => {
                let (binding, bytes) = crate::candidate_inputs::repository_live(&path, 65536)?;
                let text = std::str::from_utf8(&bytes)?;
                let value = if name == "forge.toml" {
                    conductor_native::project_config::parse_native(&path, text)
                } else {
                    conductor_native::project_config::parse_legacy(&path, text)
                }
                .map_err(anyhow::Error::msg)?;
                held.push(binding);
                return Ok(Some(value));
            }
        }
    }
    Ok(None)
}

impl Inputs {
    /// The caller validates the profile before selecting this hash domain.
    pub(crate) fn capture(campaign: &Campaign, root: &Path) -> Result<Self> {
        crate::mutation_campaign::require_no_drift(campaign, root).map_err(anyhow::Error::msg)?;
        let control_path = std::path::absolute(&campaign.manifest_path)?;
        let (control, bytes) =
            crate::candidate_inputs::repository_live(&control_path, 2 * 1024 * 1024)?;
        ensure!(
            crate::mutation_campaign::sha256_hex(&bytes) == campaign.manifest_sha256,
            "mutation campaign manifest changed before admission"
        );
        let mut held = vec![control];
        let mut absent = Vec::new();
        let config = project_config(root, &mut held, &mut absent)?;
        let relative = control_path
            .strip_prefix(root)?
            .to_str()
            .context("campaign path is not UTF-8")?;
        crate::mutation_workspace_policy::manifest_in_namespace(config.as_ref(), relative)?;
        let protected: Vec<String> = campaign
            .source_sha256
            .keys()
            .chain(campaign.test_sha256.keys())
            .cloned()
            .collect();
        let selection = crate::mutation_workspace_policy::selection(config.as_ref(), &protected)?;
        let identity =
            mutation_snapshot::live_input_identity(root, &selection).map_err(anyhow::Error::msg)?;
        let result = Self {
            root: root.to_owned(),
            selection,
            identity,
            config,
            held,
            absent,
        };
        result.verify(None, None)?;
        Ok(result)
    }

    pub(crate) fn populate(&self, snapshot: &mut mutation_snapshot::Snapshot) -> Result<()> {
        let actual = snapshot
            .populate_bound(&self.root, &self.selection)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            actual == self.identity,
            "mutation snapshot differs from admitted inputs"
        );
        snapshot
            .verify_bound(&self.root, &self.selection, &self.identity)
            .map_err(anyhow::Error::msg)
    }

    pub(crate) fn verify(
        &self,
        snapshot: Option<&Path>,
        replacement: Option<&mutation_snapshot::SnapshotReplacement>,
    ) -> Result<()> {
        for file in &self.held {
            file.verify()?;
        }
        for path in &self.absent {
            ensure!(
                matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound),
                "mutation project configuration appeared: {}",
                path.display()
            );
        }
        ensure!(
            mutation_snapshot::live_input_identity(&self.root, &self.selection)
                .map_err(anyhow::Error::msg)?
                == self.identity,
            "mutation source inputs changed"
        );
        if let Some(snapshot) = snapshot {
            let actual = if let Some(replacement) = replacement {
                mutation_snapshot::materialized_input_identity_with_replacement(
                    snapshot,
                    &self.selection,
                    replacement,
                )
            } else {
                mutation_snapshot::materialized_input_identity(snapshot, &self.selection)
            }
            .map_err(anyhow::Error::msg)?;
            ensure!(actual == self.identity, "mutation snapshot inputs changed");
        }
        Ok(())
    }

    pub(crate) fn identity(
        &self,
    ) -> &conductor_native::mutation_cargo_supporting_binding::SupportingInputs {
        &self.identity
    }
    pub(crate) fn receipt(&self, relative: &str) -> Result<()> {
        crate::mutation_workspace_policy::receipt_in_namespace(self.config.as_ref(), relative)
    }
}
