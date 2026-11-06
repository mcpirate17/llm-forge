//! Keep original operator authority separate from captured live Git objects.
use super::{finish_capture, GateArgs};
use crate::{
    candidate_execution_workspace::{Grants, Workspace},
    candidate_source::Source,
    mutation_snapshot::{LiveBinding, Snapshot},
};
use anyhow::{ensure, Context, Result};
use serde_json::Value;

struct LiveCapture {
    snapshot: Snapshot,
    binding: LiveBinding,
}

pub(super) struct SelectedSource {
    origin: Source,
    tree_source: Option<Source>,
    live: Option<LiveCapture>,
    tree: String,
}

impl SelectedSource {
    pub(super) fn capture(args: &GateArgs, origin: Source, grants: &Grants) -> Result<Self> {
        if let Some(commit) = &args.commit {
            let tree = origin
                .commit_tree(commit)
                .context("resolve explicit commit")?;
            return Ok(Self {
                origin,
                tree_source: None,
                live: None,
                tree,
            });
        }
        let mut snapshot =
            Snapshot::allocate(&grants.scratch_parent).map_err(anyhow::Error::msg)?;
        let captured = (|| -> Result<_> {
            let binding = snapshot
                .populate_live(origin.root(), &args.inputs)
                .map_err(anyhow::Error::msg)?;
            if let Some(artifacts) = &args.artifacts {
                binding
                    .reject_output(artifacts)
                    .map_err(anyhow::Error::msg)?;
            }
            snapshot
                .verify_live(origin.root(), &binding)
                .map_err(anyhow::Error::msg)?;
            let tree_source = Source::open(&snapshot.worktree)?;
            ensure!(
                snapshot.commit == binding.commit(),
                "live capture commit changed"
            );
            let tree = tree_source.commit_tree(binding.commit())?;
            ensure!(tree == binding.tree(), "live capture tree changed");
            Ok((binding, tree_source, tree))
        })();
        match captured {
            Ok((binding, tree_source, tree)) => Ok(Self {
                origin,
                tree_source: Some(tree_source),
                live: Some(LiveCapture { snapshot, binding }),
                tree,
            }),
            Err(error) => finish_capture(Err(error), snapshot.remove().map_err(anyhow::Error::msg)),
        }
    }

    pub(super) fn origin(&self) -> &Source {
        &self.origin
    }

    pub(super) fn tree_source(&self) -> &Source {
        self.tree_source.as_ref().unwrap_or(&self.origin)
    }

    pub(super) fn tree(&self) -> &str {
        &self.tree
    }

    pub(super) fn scope(&self) -> &'static str {
        if self.live.is_some() {
            "live-whole-module-declared-inputs"
        } else {
            "committed-whole-module"
        }
    }

    pub(super) fn live_evidence(&self) -> Option<Value> {
        self.live.as_ref().map(|live| live.binding.evidence())
    }

    pub(super) fn verify(&self) -> Result<()> {
        if let Some(live) = &self.live {
            live.snapshot
                .verify_live(self.origin.root(), &live.binding)
                .map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }

    pub(super) fn workspace(&self, grants: &Grants) -> Result<Workspace> {
        if self.live.is_some() {
            Workspace::new_live(self.tree_source(), &self.tree, grants)
        } else {
            Workspace::new(self.tree_source(), &self.tree, grants)
        }
    }

    pub(super) fn discard(self) -> Result<()> {
        // Stage owns its copied Git objects; it cannot depend on these inputs
        // after execution returns, including when its own cleanup is uncertain.
        drop(self.tree_source);
        if let Some(LiveCapture { snapshot, binding }) = self.live {
            drop(binding);
            snapshot.remove().map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }
}
