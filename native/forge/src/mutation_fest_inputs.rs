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
    shared: crate::mutation_profile_inputs::Inputs,
    pins: BTreeMap<String, String>,
    tests: Vec<String>,
    replacement: RefCell<Option<crate::mutation_snapshot::SnapshotReplacement>>,
}

use anyhow::Context;
use std::{cell::RefCell, collections::BTreeMap, fs, path::Path};

fn absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => anyhow::bail!("unsupported Fest/Pytest control: {}", path.display()),
    }
}

fn pytest_controls(root: &Path, tests: &[String]) -> Result<()> {
    for name in ["fest.toml", ".venv"] {
        absent(&root.join(name))?;
    }
    for directory in [root.to_owned(), root.join("src")] {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let name = entry?.file_name();
            let name = name.to_str().context("non-UTF8 Fest import root entry")?;
            ensure!(
                !crate::mutation_fest_guest::startup_name(name),
                "unsupported Fest startup hook: {}",
                directory.join(name).display()
            );
        }
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
        let shared = crate::mutation_profile_inputs::Inputs::capture(campaign, root)?;
        let result = Self {
            shared,
            pins: campaign.source_sha256.clone(),
            tests: campaign.test_sha256.keys().cloned().collect(),
            replacement: RefCell::new(None),
        };
        result.verify(None)?;
        Ok(result)
    }

    pub(crate) fn populate(&self, snapshot: &mut crate::mutation_snapshot::Snapshot) -> Result<()> {
        self.shared.populate(snapshot)
    }

    pub(crate) fn verify(&self, snapshot: Option<&Path>) -> Result<()> {
        if let Some(snapshot) = snapshot {
            pytest_controls(snapshot, &self.tests)?;
        }
        self.shared
            .verify(snapshot, self.replacement.borrow().as_ref())
    }

    pub(crate) fn identity(
        &self,
    ) -> &conductor_native::mutation_cargo_supporting_binding::SupportingInputs {
        self.shared.identity()
    }
    pub(crate) fn receipt(&self, relative: &str) -> Result<()> {
        self.shared.receipt(relative)
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

    #[test]
    fn import_roots_reject_all_startup_module_forms() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        for name in [
            "sitecustomize",
            "sitecustomize.py",
            "usercustomize.cpython-312-x86_64-linux-gnu.so",
        ] {
            let path = root.path().join("src").join(name);
            fs::write(&path, b"fixture").unwrap();
            assert!(pytest_controls(root.path(), &[]).is_err());
            fs::remove_file(path).unwrap();
        }
        assert!(pytest_controls(root.path(), &[]).is_ok());
    }
}
