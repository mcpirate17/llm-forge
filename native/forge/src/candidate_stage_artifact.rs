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
        let workspace = self
            .workspace
            .as_ref()
            .context("capture workspace missing")?;
        workspace.verify_artifact_root(&self.root)?;
        let before = self.root.symlink_metadata(&self.name)?;
        ensure!(
            before.is_file() && before.nlink() == 1 && before.uid() == unsafe { libc::geteuid() },
            "artifact must be an owned singly linked regular file"
        );
        let bytes = conductor_native::candidate_cargo_lint::read_optional(
            &self.root,
            &self.name,
            self.maximum,
        )
        .map_err(anyhow::Error::msg)?
        .context("required stage artifact missing")?;
        workspace.verify_artifact_root(&self.root)?;
        let after = self.root.symlink_metadata(&self.name)?;
        ensure!(
            after.is_file()
                && after.nlink() == 1
                && before.dev() == after.dev()
                && before.ino() == after.ino(),
            "artifact identity changed"
        );
        self.budget.admit(bytes.len())?;
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
