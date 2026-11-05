//! Retain independently selected native identity and the candidate's existing lock pin.
use super::{inputs::Capture, Context};
use crate::{candidate_inputs::BoundFile, candidate_native_runner_pin::NativeRunnerPin};
use anyhow::{ensure, Context as _, Result};
use serde_json::{json, Value};

pub(super) struct Identity {
    pin: NativeRunnerPin,
    lock: BoundFile,
    pub evidence: Value,
}

impl Identity {
    pub fn capture(context: &Context<'_>, capture: &mut Capture<'_>) -> Result<Option<Self>> {
        let Some((path, sha256)) = context.native_runner_pin else {
            return Ok(None);
        };
        ensure!(
            capture.native_inventory_absent()?,
            "partial native source inventory cannot use an installed runner pin"
        );
        let stamp = context
            .stamp
            .context("native runner pin requires trusted CLI build identity")?;
        let entries = context.source.list_tree(&context.candidate.tree_oid)?;
        let (commit, lock_sha256, lock) =
            crate::candidate_engine_identity::installed_pin(context.snapshot, &entries)?;
        ensure!(
            commit.as_deref() == Some(stamp.git_commit),
            "candidate uv.lock does not pin the running native CLI revision"
        );
        let lock = lock.context("installed candidate runner requires held uv.lock")?;
        let mut excluded = vec![
            context.source.root().to_path_buf(),
            context.source.common_dir().to_path_buf(),
            context.snapshot.path(),
        ];
        excluded.extend_from_slice(context.scratch_exclusions);
        let pin = NativeRunnerPin::capture(path, sha256, &excluded, stamp)?;
        let identity = pin.identity();
        let evidence = json!({"mode":"installed","pin":path,"pin_sha256":sha256,"candidate_lock_sha256":lock_sha256,
            "compiled_git_commit":identity.compiled_git_commit,"compiled_source_sha256":identity.compiled_source_sha256,
            "executable_sha256":identity.executable_sha256,"scope":"operator-selected cooperative freshness; not producer attestation"});
        Ok(Some(Self {
            pin,
            lock,
            evidence,
        }))
    }

    pub fn identity(&self) -> &conductor_native::mutation_native_runner::InstalledIdentity {
        self.pin.identity()
    }

    pub fn verify(&self) -> Result<()> {
        self.lock.verify()?;
        self.pin.verify()
    }
}
