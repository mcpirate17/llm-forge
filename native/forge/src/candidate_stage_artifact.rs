//! One optional owner-captured runtime file. Capture is attempted only after
//! successful process and cgroup cleanup; delivery waits for workspace cleanup.
use crate::{
    candidate_execution_workspace::Workspace,
    confined_execution::{FinishState, Resources},
};
use anyhow::{ensure, Context, Result};
use cap_std::fs::{Dir, MetadataExt};
use sha2::{Digest, Sha256};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::oneshot;

pub(crate) struct Artifact {
    pub bytes: Vec<u8>,
    pub sha256: String,
}
pub(crate) struct Request<'a> {
    pub name: &'a str,
    pub maximum: usize,
    pub budget: Arc<ArtifactBudget>,
}
pub(crate) struct ArtifactBudget {
    used: AtomicUsize,
    maximum: usize,
}
impl ArtifactBudget {
    pub(crate) fn new(maximum: usize) -> Self {
        Self {
            used: AtomicUsize::new(0),
            maximum,
        }
    }
    fn admit(&self, bytes: usize) -> Result<()> {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                n.checked_add(bytes).filter(|sum| *sum <= self.maximum)
            })
            .map_err(|_| anyhow::anyhow!("stage artifact aggregate budget exceeded"))?;
        Ok(())
    }
}
pub(crate) struct Capture {
    workspace: Option<Workspace>,
    root: Dir,
    name: String,
    maximum: usize,
    budget: Arc<ArtifactBudget>,
    sender: Option<oneshot::Sender<std::result::Result<Artifact, String>>>,
}
impl Capture {
    pub(crate) fn new(
        workspace: Workspace,
        name: &str,
        maximum: usize,
        budget: Arc<ArtifactBudget>,
    ) -> Result<(
        Self,
        oneshot::Receiver<std::result::Result<Artifact, String>>,
    )> {
        ensure!(
            !name.is_empty()
                && name.len() <= 255
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                && name != "."
                && name != "..",
            "unsafe artifact name"
        );
        ensure!(
            (1..=4 * 1024 * 1024).contains(&maximum),
            "unsafe artifact bound"
        );
        let root = workspace.artifact_root()?;
        workspace.verify_artifact_root(&root)?;
        let (sender, receiver) = oneshot::channel();
        Ok((
            Self {
                workspace: Some(workspace),
                root,
                name: name.into(),
                maximum,
                budget,
                sender: Some(sender),
            },
            receiver,
        ))
    }
    fn read(&self) -> Result<Artifact> {
        self.read_named(&self.name, self.maximum, &self.budget)
    }
    fn read_named(&self, name: &str, maximum: usize, budget: &ArtifactBudget) -> Result<Artifact> {
        let workspace = self
            .workspace
            .as_ref()
            .context("capture workspace missing")?;
        workspace.verify_artifact_root(&self.root)?;
        let before = self.root.symlink_metadata(name)?;
        ensure!(
            before.is_file() && before.nlink() == 1 && before.uid() == unsafe { libc::geteuid() },
            "artifact must be an owned singly linked regular file"
        );
        let bytes =
            conductor_native::candidate_cargo_lint::read_optional(&self.root, name, maximum)
                .map_err(anyhow::Error::msg)?
                .context("required stage artifact missing")?;
        workspace.verify_artifact_root(&self.root)?;
        let after = self.root.symlink_metadata(name)?;
        ensure!(
            after.is_file()
                && after.nlink() == 1
                && before.dev() == after.dev()
                && before.ino() == after.ino(),
            "artifact identity changed"
        );
        budget.admit(bytes.len())?;
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        Ok(Artifact { bytes, sha256 })
    }
    fn complete(mut self: Box<Self>, state: FinishState) -> Result<()> {
        let required = state.succeeded();
        let captured = if state.succeeded() {
            self.read()
        } else {
            Err(anyhow::anyhow!("stage unsuccessful; artifact unavailable"))
        };
        let cleanup = Box::new(self.workspace.take().context("capture workspace missing")?)
            .finish(state.cleanup_safe);
        let cleanup_failed = cleanup.is_err();
        let result = match (captured, cleanup) {
            (Ok(value), Ok(())) => Ok(value),
            (capture, cleanup) => Err(format!(
                "artifact capture: {}; cleanup: {}",
                capture
                    .err()
                    .map(|e| format!("{e:#}"))
                    .unwrap_or_else(|| "ok".into()),
                cleanup
                    .err()
                    .map(|e| format!("{e:#}"))
                    .unwrap_or_else(|| "ok".into())
            )),
        };
        let failure = result.as_ref().err().cloned();
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(result);
        }
        if let Some(failure) = failure.filter(|_| required || cleanup_failed) {
            anyhow::bail!("{failure}");
        }
        Ok(())
    }
}
fn no_sqlite_sidecars(root: &Dir, name: &str) -> Result<()> {
    for suffix in ["-wal", "-journal", "-shm"] {
        match root.symlink_metadata(format!("{name}{suffix}")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
            Ok(_) => anyhow::bail!("record SQLite sidecar remains"),
        }
    }
    Ok(())
}
/// Capture coverage and an owned per-shard SQLite record after group cleanup.
/// The first capture also owns workspace destruction; the second never outlives it.
pub(crate) struct CapturePair {
    first: Option<Box<Capture>>,
    first_receiver: Option<oneshot::Receiver<std::result::Result<Artifact, String>>>,
    second_name: String,
    second_maximum: usize,
    second_budget: Arc<ArtifactBudget>,
    sender: Option<oneshot::Sender<std::result::Result<Vec<Artifact>, String>>>,
}
type PairReceiver = oneshot::Receiver<std::result::Result<Vec<Artifact>, String>>;
impl CapturePair {
    pub fn new(
        workspace: Workspace,
        first: Request<'_>,
        second: Request<'_>,
    ) -> Result<(Self, PairReceiver)> {
        ensure!(
            second.name != first.name
                && !second.name.is_empty()
                && second.name.len() <= 255
                && second
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
            "invalid second artifact name"
        );
        ensure!(
            (1..=4 * 1024 * 1024).contains(&second.maximum),
            "invalid second artifact limit"
        );
        let (first, receiver) = Capture::new(workspace, first.name, first.maximum, first.budget)?;
        let (sender, output) = oneshot::channel();
        Ok((
            Self {
                first: Some(Box::new(first)),
                first_receiver: Some(receiver),
                second_name: second.name.into(),
                second_maximum: second.maximum,
                second_budget: second.budget,
                sender: Some(sender),
            },
            output,
        ))
    }
    fn complete(mut self, state: FinishState) -> Result<()> {
        let first = self.first.take().context("first capture missing")?;
        let second = if state.succeeded() {
            (|| -> Result<Artifact> {
                no_sqlite_sidecars(&first.root, &self.second_name)?;
                first.read_named(&self.second_name, self.second_maximum, &self.second_budget)
            })()
        } else {
            Err(anyhow::anyhow!("stage unsuccessful; record unavailable"))
        };
        let result = first.finish_with_state(state);
        // An unsuccessful child suppresses expected missing-artifact errors, not
        // a failure to dispose the owned workspace.
        let cleanup_failed = result.is_err();
        let first_result = self
            .first_receiver
            .take()
            .context("first receiver missing")?
            .try_recv()
            .context("first capture did not report")?;
        let pair = match (result, first_result, second) {
            (Ok(()), Ok(first), Ok(second)) => Ok(vec![first, second]),
            (cleanup, first, second) => Err(format!(
                "first capture/cleanup={:?}; first artifact={:?}; record artifact={:?}",
                cleanup.err(),
                first.err(),
                second.err()
            )),
        };
        let failure = pair.as_ref().err().cloned();
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(pair);
        }
        if let Some(error) =
            failure.filter(|_| state.succeeded() || !state.cleanup_safe || cleanup_failed)
        {
            anyhow::bail!(error);
        }
        Ok(())
    }
}
impl Resources for CapturePair {
    fn arm(&mut self) {
        if let Some(first) = &mut self.first {
            first.arm();
        }
    }
    fn finish(self: Box<Self>, cleanup_safe: bool) -> Result<()> {
        (*self).complete(FinishState {
            cleanup_safe,
            outcome: crate::confined_execution::Outcome::Failed,
            exit_code: None,
        })
    }
    fn finish_with_state(self: Box<Self>, state: FinishState) -> Result<()> {
        (*self).complete(state)
    }
}

impl Resources for Capture {
    fn arm(&mut self) {
        if let Some(workspace) = &mut self.workspace {
            workspace.arm();
        }
    }
    fn finish(self: Box<Self>, cleanup_safe: bool) -> Result<()> {
        self.complete(FinishState {
            cleanup_safe,
            outcome: crate::confined_execution::Outcome::Failed,
            exit_code: None,
        })
    }
    fn finish_with_state(self: Box<Self>, state: FinishState) -> Result<()> {
        self.complete(state)
    }
}

#[cfg(test)]
mod pair_tests {
    use super::*;
    #[test]
    fn sqlite_sidecars_prevent_record_capture() {
        let temp = tempfile::tempdir().unwrap();
        let root = Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority()).unwrap();
        assert!(no_sqlite_sidecars(&root, "record.sqlite").is_ok());
        for suffix in ["-wal", "-journal", "-shm"] {
            let path = temp.path().join(format!("record.sqlite{suffix}"));
            std::fs::write(&path, "x").unwrap();
            assert!(no_sqlite_sidecars(&root, "record.sqlite").is_err());
            std::fs::remove_file(path).unwrap();
        }
    }
}
