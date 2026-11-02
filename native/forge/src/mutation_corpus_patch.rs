//! One read-only corpus patch applicability observation over a held snapshot.
//! This is not a full corpus audit, receipt, or gate verdict.
use crate::{
    candidate_inputs::{self, BoundFile},
    candidate_snapshot::Snapshot,
    candidate_source, process_probe,
    source_file::SourceRoot,
};
use anyhow::{ensure, Context, Result};
use serde::Serialize;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const MAX_GIT_BYTES: usize = 32 * 1024 * 1024;
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PatchObservation {
    Missing,
    HashDrift { expected: String, actual: String },
    GitApplies,
    AnchoredApplies,
    DoesNotApply { detail: String },
}
/// A pinned native Git executable and a held immutable candidate snapshot.
pub struct Checker<'a> {
    snapshot: Option<&'a Snapshot>,
    interrupts: Option<&'a crate::process_signals::Interrupts>,
    root_path: PathBuf,
    identity: (u64, u64),
    git: PathBuf,
    git_binding: BoundFile,
    profile: crate::mutation_corpus_git_profile::Profile,
    root: SourceRoot,
    held: RefCell<BTreeMap<String, Option<String>>>,
    configuration: RefCell<Option<String>>,
    attributes: RefCell<Option<crate::mutation_corpus_git_attrs::Attributes>>,
}
impl<'a> Checker<'a> {
    pub(crate) fn open(
        snapshot: &'a Snapshot,
        git: &Path,
        git_sha256: &str,
        profile: crate::mutation_corpus_git_profile::Profile,
    ) -> Result<Self> {
        let root = snapshot.path();
        profile.verify_for(git_sha256)?;
        snapshot.directory()?;
        let (git_binding, _) = candidate_inputs::external(git, git_sha256, MAX_GIT_BYTES, &root)?;
        let metadata = std::fs::metadata(git)?;
        ensure!(
            metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
            "pinned Git is not executable"
        );
        let opened = SourceRoot::open(&root)?;
        let identity = opened.identity()?;
        Ok(Self {
            snapshot: Some(snapshot),
            interrupts: None,
            root_path: root,
            identity,
            git: git.to_owned(),
            git_binding,
            profile,
            root: opened,
            held: RefCell::new(BTreeMap::new()),
            configuration: RefCell::new(None),
            attributes: RefCell::new(None),
        })
    }
    pub(crate) fn open_live(
        root: &Path,
        git: &Path,
        git_sha256: &str,
        profile: crate::mutation_corpus_git_profile::Profile,
    ) -> Result<Self> {
        profile.verify_for(git_sha256)?;
        let root_path = root.canonicalize()?;
        ensure!(root_path == root, "live corpus root must be canonical");
        let opened = SourceRoot::open(root)?;
        let identity = opened.identity()?;
        let (git_binding, _) = candidate_inputs::external(git, git_sha256, MAX_GIT_BYTES, root)?;
        let metadata = std::fs::metadata(git)?;
        ensure!(
            metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
            "pinned Git is not executable"
        );
        Ok(Self {
            snapshot: None,
            interrupts: None,
            root_path,
            identity,
            git: git.to_owned(),
            git_binding,
            profile,
            root: opened,
            held: RefCell::new(BTreeMap::new()),
            configuration: RefCell::new(None),
            attributes: RefCell::new(None),
        })
    }
    pub(crate) fn with_interrupts(
        mut self,
        interrupts: &'a crate::process_signals::Interrupts,
    ) -> Self {
        self.interrupts = Some(interrupts);
        self
    }
    pub(crate) fn poll(&self) -> Result<()> {
        if let Some(owner) = self.interrupts {
            ensure!(!owner.interrupted()?, "corpus audit interrupted");
        }
        Ok(())
    }
    fn probe(
        &self,
        command: &mut Command,
        input: Option<&[u8]>,
        limit: usize,
    ) -> Result<std::process::Output> {
        let timeout = Duration::from_secs(30);
        match (self.interrupts, input) {
            (Some(owner), Some(bytes)) => {
                process_probe::run_with_input_interruptible(command, bytes, timeout, limit, owner)
            }
            (Some(owner), None) => process_probe::run_interruptible(command, timeout, limit, owner),
            (None, Some(bytes)) => process_probe::run_with_input(command, bytes, timeout, limit),
            (None, None) => process_probe::run(command, timeout, limit),
        }
    }
    fn directory(&self) -> Result<cap_std::fs::Dir> {
        if let Some(snapshot) = self.snapshot {
            return snapshot.directory();
        }
        let reopened = SourceRoot::open(&self.root_path)?;
        ensure!(
            self.root_path.canonicalize()? == self.root_path
                && reopened.identity()? == self.identity
                && self.root.identity()? == self.identity,
            "live corpus root changed"
        );
        Ok(cap_std::fs::Dir::open_ambient_dir(
            &self.root_path,
            cap_std::ambient_authority(),
        )?)
    }
    fn admit_path(&self, relative: &str) -> Result<()> {
        candidate_source::safe_path(relative.as_bytes())?;
        let path = self.root_path.join(relative);
        let mut parent = path.parent().context("patch input parent missing")?;
        loop {
            match std::fs::symlink_metadata(parent) {
                Ok(metadata) => {
                    ensure!(
                        metadata.is_dir() && !metadata.file_type().is_symlink(),
                        "input parent is not a safe directory"
                    );
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    parent = parent.parent().context("input ancestor missing")?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        ensure!(
            parent.starts_with(&self.root_path) && parent.canonicalize()? == parent,
            "patch input parent has a symlink or escapes root"
        );
        Ok(())
    }
    fn capture(&self, paths: &[String]) -> Result<BTreeMap<String, Option<String>>> {
        let directory = self.directory()?;
        let mut result = BTreeMap::new();
        let mut bytes = 0usize;
        for path in paths {
            self.admit_path(path)?;
            let value = match directory.symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(error).with_context(|| format!("inspect patch target {path}"))
                }
                Ok(_) => Some(self.root.read(Path::new(path))?.into_content()),
            };
            if let Some(text) = &value {
                bytes = bytes
                    .checked_add(text.len())
                    .context("patch target byte overflow")?;
                ensure!(
                    bytes <= 64 * 1024 * 1024,
                    "patch target aggregate exceeds 64 MiB"
                );
            }
            result.insert(path.clone(), value);
        }
        self.directory()?;
        Ok(result)
    }
    fn configuration(&self) -> Result<String> {
        use sha2::{Digest, Sha256};
        let mut command = Command::new(&self.git);
        command
            .current_dir(&self.root_path)
            .args(["config", "--null", "--show-origin", "--list"]);
        let output = self.probe(&mut command, None, 1024 * 1024)?;
        ensure!(
            output.status.success()
                || (output.status.code() == Some(1) && output.stdout.is_empty()),
            "Git configuration observation failed"
        );
        Ok(format!("{:x}", Sha256::digest(&output.stdout)))
    }
    fn hold_configuration(&self) -> Result<()> {
        self.git_binding.verify()?;
        let actual = self.configuration()?;
        self.git_binding.verify()?;
        let mut held = self.configuration.borrow_mut();
        if let Some(previous) = held.as_ref() {
            ensure!(
                *previous == actual,
                "Git configuration changed during audit"
            );
        } else {
            *held = Some(actual);
        }
        Ok(())
    }
    fn hold_attributes(&self) -> Result<()> {
        self.git_binding.verify()?;
        self.profile.verify()?;
        let mut held = self.attributes.borrow_mut();
        if held.is_none() {
            *held = Some(crate::mutation_corpus_git_attrs::Attributes::capture(
                &self.git,
                &self.root_path,
                &self.profile,
                self.interrupts,
            )?);
        }
        held.as_ref()
            .context("Git attributes not captured")?
            .verify()?;
        self.git_binding.verify()
    }
    pub(crate) fn provenance(&self) -> serde_json::Value {
        self.profile.provenance()
    }
    /// Revalidate every previously observed target and the pinned executable.
    pub fn verify(&self) -> Result<()> {
        self.poll()?;
        self.git_binding.verify()?;
        self.hold_configuration()?;
        self.hold_attributes()?;
        let held = self.held.borrow();
        let names: Vec<_> = held.keys().cloned().collect();
        ensure!(
            self.capture(&names)? == *held,
            "previously observed patch targets changed"
        );
        Ok(())
    }
    fn hold(&self, values: &BTreeMap<String, Option<String>>) -> Result<()> {
        let mut held = self.held.borrow_mut();
        for (path, value) in values {
            if let Some(previous) = held.get(path) {
                ensure!(
                    previous == value,
                    "patch target changed between observations"
                );
            } else {
                held.insert(path.clone(), value.clone());
            }
        }
        ensure!(held.len() <= 4096, "held patch target count exceeds 4096");
        Ok(())
    }
    // Git identifies paths for every format it accepts; do not require the
    // narrower anchored parser before asking Git for its applicability verdict.
    fn git_paths(&self, patch: &str) -> Result<Option<Vec<String>>> {
        let root = self.root_path.clone();
        let mut command = Command::new(&self.git);
        command
            .current_dir(&root)
            .args(["apply", "--numstat", "-z", "-"]);
        self.git_binding.verify()?;
        let output = self.probe(&mut command, Some(patch.as_bytes()), 1024 * 1024)?;
        self.git_binding.verify()?;
        ensure!(
            output.status.code().is_some(),
            "Git patch inventory terminated by signal"
        );
        if !output.status.success() {
            return Ok(None);
        }
        let mut fields = output.stdout.split(|byte| *byte == 0);
        let mut paths = std::collections::BTreeSet::new();
        while let Some(row) = fields.next() {
            if row.is_empty() {
                continue;
            }
            let mut cols = row.splitn(3, |byte| *byte == b'\t');
            let _added = cols.next().context("invalid Git numstat")?;
            let _deleted = cols.next().context("invalid Git numstat")?;
            let path = cols.next().context("invalid Git numstat path")?;
            if path.is_empty() {
                for _ in 0..2 {
                    let name = fields.next().context("incomplete Git rename path")?;
                    paths.insert(candidate_source::safe_path(name)?.to_owned());
                }
            } else {
                paths.insert(candidate_source::safe_path(path)?.to_owned());
            }
            ensure!(paths.len() <= 4096, "patch target count exceeds 4096");
        }
        Ok(Some(paths.into_iter().collect()))
    }
    /// Check native Git first, then the anchored in-memory fallback on Git refusal.
    /// Unsupported/bounded inputs refuse; they are never reported as a stale mutant.
    pub fn check(&self, patch_path: &str, expected_sha256: &str) -> Result<PatchObservation> {
        self.poll()?;
        self.admit_path(patch_path)?;
        conductor_native::sha256_hex::parse(expected_sha256).map_err(anyhow::Error::msg)?;
        let directory = self.directory()?;
        match directory.symlink_metadata(patch_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PatchObservation::Missing)
            }
            Err(error) => return Err(error).context("inspect mutation patch"),
            Ok(_) => (),
        }
        let patch = self.root.read(Path::new(patch_path))?;
        if patch.sha256() != expected_sha256 {
            return Ok(PatchObservation::HashDrift {
                expected: expected_sha256.into(),
                actual: patch.sha256().into(),
            });
        }
        self.hold_configuration()?;
        self.hold_attributes()?;
        let paths = match self.git_paths(patch.content())? {
            Some(paths) => Ok(paths),
            None => conductor_native::mutation_patch_anchor::required_paths(patch.content()),
        };
        let paths = paths.and_then(with_worktree_attributes);
        let before = match &paths {
            Ok(paths) => Some(self.capture(paths)?),
            Err(_) => None,
        };
        self.git_binding.verify()?;
        let root = self.root_path.clone();
        let mut command = Command::new(&self.git);
        command.current_dir(&root).args(["apply", "--check", "-"]);
        let output = self.probe(&mut command, Some(patch.content().as_bytes()), 64 * 1024)?;
        self.git_binding.verify()?;
        self.hold_configuration()?;
        self.hold_attributes()?;
        ensure!(
            self.root.read(Path::new(patch_path))?.sha256() == expected_sha256,
            "mutation patch changed during Git check"
        );
        if let (Ok(paths), Some(before)) = (&paths, &before) {
            ensure!(
                self.capture(paths)? == *before,
                "mutation patch target changed during Git check"
            );
            self.hold(before)?;
        }
        if output.status.success() {
            ensure!(
                before.is_some(),
                "Git accepted patch but target inventory cannot be bound"
            );
            return Ok(PatchObservation::GitApplies);
        }
        fallback(patch.content(), paths, before, &output)
    }
}
fn with_worktree_attributes(paths: Vec<String>) -> Result<Vec<String>> {
    let mut result: std::collections::BTreeSet<String> = paths.iter().cloned().collect();
    result.insert(".gitattributes".into());
    for path in paths {
        let mut parent = Path::new(&path).parent();
        while let Some(directory) = parent.filter(|path| !path.as_os_str().is_empty()) {
            let attribute = directory.join(".gitattributes");
            result.insert(
                attribute
                    .to_str()
                    .context("attribute path must be UTF-8")?
                    .to_owned(),
            );
            if result.len() > 4096 {
                return Err(conductor_native::mutation_patch_anchor::ResourceLimit(
                    "patch target and attribute paths exceed 4096",
                )
                .into());
            }
            parent = directory.parent();
        }
    }
    Ok(result.into_iter().collect())
}
fn fallback(
    patch: &str,
    paths: Result<Vec<String>>,
    before: Option<BTreeMap<String, Option<String>>>,
    output: &std::process::Output,
) -> Result<PatchObservation> {
    ensure!(
        output.status.code().is_some(),
        "Git patch check terminated by signal"
    );
    if let Err(error) = &paths {
        if error
            .downcast_ref::<conductor_native::mutation_patch_anchor::ResourceLimit>()
            .is_some()
        {
            anyhow::bail!("anchored applicability did not run within limits: {error:#}");
        }
    }
    let diagnostic = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    let diagnostic = String::from_utf8_lossy(diagnostic)
        .trim()
        .chars()
        .take(200)
        .collect::<String>();
    let Some(before) = before else {
        anyhow::bail!("Git refused the patch and its target/attribute inventory cannot be bound");
    };
    let files: BTreeMap<_, _> = before
        .into_iter()
        .filter_map(|(path, value)| value.map(|text| (path, text)))
        .collect();
    match conductor_native::mutation_patch_anchor::check(patch, &files) {
        Ok(()) => Ok(PatchObservation::AnchoredApplies),
        Err(error)
            if error
                .downcast_ref::<conductor_native::mutation_patch_anchor::ResourceLimit>()
                .is_some() =>
        {
            Err(error).context("anchored applicability did not run within limits")
        }
        Err(error) => Ok(PatchObservation::DoesNotApply {
            detail: format!(
                "{diagnostic}; anchored retry: {}",
                format!("{error:#}").chars().take(200).collect::<String>()
            ),
        }),
    }
}
#[cfg(test)]
#[path = "mutation_corpus_patch_tests.rs"]
mod tests;
