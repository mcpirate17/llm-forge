//! Admission for the explicitly generated confined Fest test scope.
use crate::mutation_campaign::Campaign;
use anyhow::{ensure, Result};
use std::collections::BTreeSet;

/// Validate the verified generator contract independently of any receipt.
/// Snapshot selection and guest admission remain separate required steps.
pub(crate) fn validate_campaign(campaign: &Campaign) -> Result<()> {
    ensure!(
        campaign.generated_value.is_none()
            && campaign.language == "python"
            && campaign.mutation_engine == "fest"
            && campaign.option_text("execution_profile").as_deref() == Some("fest-confined-v1"),
        "confined Fest requires its verified generated profile"
    );
    ensure!(
        campaign.options.len() == 1 && campaign.options.contains_key("execution_profile"),
        "confined Fest refuses unsupported generator options"
    );
    ensure!(
        campaign.environment.is_empty()
            && campaign.jobs == 1
            && campaign.seed >= 0
            && campaign.operators.is_empty(),
        "confined Fest requires one worker and no environment or operator overrides"
    );
    ensure!(
        campaign.exclude == ["**/test_*.py", "**/conftest.py"],
        "confined Fest requires the generated test exclusions"
    );
    ensure!(
        campaign.source.len() == 1
            && campaign.source_sha256.len() == 1
            && campaign.source_sha256.contains_key(&campaign.source[0]),
        "confined Fest requires exactly one pinned generated source"
    );
    let source = &campaign.source[0];
    conductor_native::mutation_evidence::validate_fest_path(source).map_err(anyhow::Error::msg)?;
    let prefix = ["python", "-m", "pytest", "-q", "--rootdir=."];
    ensure!(
        campaign.test_argv.len() > prefix.len()
            && campaign
                .test_argv
                .iter()
                .take(prefix.len())
                .map(String::as_str)
                .eq(prefix),
        "confined Fest requires the generated pytest command"
    );
    let tests = &campaign.test_argv[prefix.len()..];
    let declared: BTreeSet<_> = tests.iter().map(String::as_str).collect();
    let pinned: BTreeSet<_> = campaign.test_sha256.keys().map(String::as_str).collect();
    ensure!(
        declared.len() == tests.len() && declared == pinned && !declared.is_empty(),
        "confined Fest test paths must match the pinned test set exactly"
    );
    for test in tests {
        conductor_native::mutation_evidence::validate_fest_path(test)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            test != source,
            "Fest source and test paths must be distinct"
        );
    }
    Ok(())
}

/// Full-source snapshot inputs admitted independently of a mutation receipt.
pub(crate) struct Inputs {
    root: PathBuf,
    selection: crate::mutation_snapshot::SnapshotSelection,
    identity: conductor_native::mutation_cargo_supporting_binding::SupportingInputs,
    config: Option<conductor_native::project_config::ProjectConfig>,
    held: Vec<crate::candidate_inputs::BoundFile>,
    absent: Vec<PathBuf>,
    pins: BTreeMap<String, String>,
    tests: Vec<String>,
    replacement: RefCell<Option<crate::mutation_snapshot::SnapshotReplacement>>,
}

use anyhow::Context;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

fn project_config(
    root: &Path,
    held: &mut Vec<crate::candidate_inputs::BoundFile>,
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

fn absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => anyhow::bail!("unsupported Fest/Pytest control: {}", path.display()),
    }
}

fn pytest_controls(root: &Path, tests: &[String]) -> Result<()> {
    for name in [
        "fest.toml",
        ".venv",
        "sitecustomize.py",
        "usercustomize.py",
        "src/sitecustomize.py",
        "src/usercustomize.py",
    ] {
        absent(&root.join(name))?;
    }
    let mut directories = BTreeSet::new();
    directories.insert(root.to_owned());
    for test in tests {
        let mut current = root
            .join(test)
            .parent()
            .context("test has no parent")?
            .to_owned();
        loop {
            ensure!(
                current.starts_with(root),
                "pytest test ancestor escaped snapshot"
            );
            directories.insert(current.clone());
            if current == root {
                break;
            }
            current = current
                .parent()
                .context("pytest test ancestor missing")?
                .to_owned();
        }
    }
    for directory in directories {
        for name in [
            "conftest.py",
            "pytest.ini",
            ".pytest.ini",
            "tox.ini",
            "setup.cfg",
        ] {
            absent(&directory.join(name))?;
        }
        let path = directory.join("pyproject.toml");
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
            Ok(_) => {
                let (_, bytes) = crate::candidate_inputs::repository_live(&path, 1024 * 1024)?;
                let value: toml::Value = std::str::from_utf8(&bytes)?.parse()?;
                for key in ["pytest", "fest", "coverage"] {
                    ensure!(
                        value.get("tool").and_then(|v| v.get(key)).is_none(),
                        "confined Fest refuses {key} configuration in {}",
                        path.display()
                    );
                }
            }
        }
    }
    Ok(())
}

impl Inputs {
    pub(crate) fn capture(campaign: &Campaign, root: &Path) -> Result<Self> {
        validate_campaign(campaign)?;
        crate::mutation_campaign::require_no_drift(campaign, root).map_err(anyhow::Error::msg)?;
        let control_path = std::path::absolute(&campaign.manifest_path)?;
        let (control, bytes) =
            crate::candidate_inputs::repository_live(&control_path, 2 * 1024 * 1024)?;
        ensure!(
            crate::mutation_campaign::sha256_hex(&bytes) == campaign.manifest_sha256,
            "Fest campaign manifest changed before admission"
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
        let identity = crate::mutation_snapshot::live_input_identity(root, &selection)
            .map_err(anyhow::Error::msg)?;
        let result = Self {
            root: root.to_owned(),
            selection,
            identity,
            config,
            held,
            absent,
            pins: campaign.source_sha256.clone(),
            tests: campaign.test_sha256.keys().cloned().collect(),
            replacement: RefCell::new(None),
        };
        result.verify(None)?;
        Ok(result)
    }

    pub(crate) fn populate(&self, snapshot: &mut crate::mutation_snapshot::Snapshot) -> Result<()> {
        let actual = snapshot
            .populate_bound(&self.root, &self.selection)
            .map_err(anyhow::Error::msg)?;
        ensure!(
            actual == self.identity,
            "Fest snapshot differs from admitted inputs"
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
            ensure!(
                matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound),
                "Fest project configuration appeared: {}",
                path.display()
            );
        }
        ensure!(
            crate::mutation_snapshot::live_input_identity(&self.root, &self.selection)
                .map_err(anyhow::Error::msg)?
                == self.identity,
            "Fest source inputs changed"
        );
        if let Some(snapshot) = snapshot {
            pytest_controls(snapshot, &self.tests)?;
            let replacement = self.replacement.borrow();
            let actual = if let Some(replacement) = replacement.as_ref() {
                crate::mutation_snapshot::materialized_input_identity_with_replacement(
                    snapshot,
                    &self.selection,
                    replacement,
                )
            } else {
                crate::mutation_snapshot::materialized_input_identity(snapshot, &self.selection)
            }
            .map_err(anyhow::Error::msg)?;
            ensure!(actual == self.identity, "Fest snapshot inputs changed");
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

    pub(crate) fn begin_mutant<'a>(
        &'a self,
        row: &serde_json::Value,
        worktree: &Path,
    ) -> Result<FestMutantGuard<'a>> {
        ensure!(
            self.replacement.borrow().is_none(),
            "nested Fest mutant guard unsupported"
        );
        self.verify(Some(worktree))?;
        let replacement =
            crate::mutation_snapshot::attribution_replacement(worktree, row, &self.pins)
                .map_err(anyhow::Error::msg)?;
        *self.replacement.borrow_mut() = Some(replacement);
        Ok(FestMutantGuard { inputs: self })
    }
}

pub(crate) struct FestMutantGuard<'a> {
    inputs: &'a Inputs,
}
impl Drop for FestMutantGuard<'_> {
    fn drop(&mut self) {
        self.inputs.replacement.borrow_mut().take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{collections::BTreeMap, path::PathBuf};

    fn campaign() -> Campaign {
        Campaign {
            manifest_path: PathBuf::from("campaign.json"),
            manifest_sha256: "a".repeat(64),
            campaign_id: "example".into(),
            title: "example".into(),
            language: "python".into(),
            mutation_engine: "fest".into(),
            source: vec!["pkg/subject.py".into()],
            exclude: vec!["**/test_*.py".into(), "**/conftest.py".into()],
            operators: vec![],
            options: json!({"execution_profile":"fest-confined-v1"})
                .as_object()
                .unwrap()
                .clone(),
            seed: 0,
            jobs: 1,
            mutant_timeout_seconds: None,
            run_timeout_seconds: 30,
            test_argv: [
                "python",
                "-m",
                "pytest",
                "-q",
                "--rootdir=.",
                "tests/test_subject.py",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            source_sha256: BTreeMap::from([("pkg/subject.py".into(), "a".repeat(64))]),
            test_sha256: BTreeMap::from([("tests/test_subject.py".into(), "b".repeat(64))]),
            survivor_baseline: vec![],
            survivor_baseline_recorded: false,
            environment: BTreeMap::new(),
            generated_value: None,
        }
    }

    #[test]
    fn accepts_only_the_exact_generated_test_scope() {
        let good = campaign();
        assert!(validate_campaign(&good).is_ok());
        let mut bad = good.clone();
        bad.test_argv.push("tests/other.py".into());
        assert!(validate_campaign(&bad).is_err());
        let mut bad = good.clone();
        bad.test_argv[0] = "python -I".into();
        assert!(validate_campaign(&bad).is_err());
        let mut bad = good.clone();
        bad.test_argv[5] = "tests/*.py".into();
        bad.test_sha256 = BTreeMap::from([("tests/*.py".into(), "b".repeat(64))]);
        assert!(validate_campaign(&bad).is_err());
        let mut bad = good;
        bad.environment.insert("PYTHONPATH".into(), "/tmp".into());
        assert!(validate_campaign(&bad).is_err());
    }
}
