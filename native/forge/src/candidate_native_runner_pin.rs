//! Independently held operator selection of one clean installed native runner.
//! These measurements do not attest that source bytes produced the executable.
use crate::{
    candidate_engine_identity::{self as engine, BuildStamp},
    candidate_inputs::{self, BoundFile},
};
use anyhow::{ensure, Context, Result};
use conductor_native::mutation_native_runner::InstalledIdentity;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_PIN_BYTES: usize = 16 * 1024;
const SCHEMA: &str = "llm.mutation-testing.native-runner-pin.v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PinDescription {
    pub schema_version: String,
    pub compiled_git_commit: String,
    pub compiled_source_sha256: String,
    pub executable_sha256: String,
}

/// Describe this process from trusted CLI build literals and the running inode.
/// Output is informational until the operator independently chooses and pins it.
pub fn describe(stamp: &BuildStamp) -> Result<PinDescription> {
    ensure!(
        stamp.source_matches_git == "1",
        "native CLI build is dirty or has unknown cleanliness"
    );
    ensure!(
        engine::valid_sha256(stamp.source_sha256),
        "native CLI source stamp is malformed"
    );
    ensure!(
        engine::valid_commit(stamp.git_commit),
        "native CLI Git revision is missing or malformed"
    );
    let executable = engine::running_executable()?;
    Ok(PinDescription {
        schema_version: SCHEMA.into(),
        compiled_git_commit: stamp.git_commit.into(),
        compiled_source_sha256: stamp.source_sha256.into(),
        executable_sha256: executable.sha256,
    })
}

/// Retain this object through final disposition and call `verify` then.
/// The caller must prove and retain complete native source inventory absence;
/// an unavailable or partial inventory alone is never such proof.
pub struct NativeRunnerPin {
    held: BoundFile,
    expected: PinDescription,
    identity: InstalledIdentity,
    executable: engine::BinaryIdentity,
}
impl NativeRunnerPin {
    pub fn capture(
        path: &Path,
        sha256: &str,
        excluded_roots: &[PathBuf],
        stamp: &BuildStamp,
    ) -> Result<Self> {
        let first = excluded_roots
            .first()
            .context("at least the host root must be excluded")?;
        for root in excluded_roots {
            candidate_inputs::exclude(path, Some(root))?;
        }
        let (held, bytes) = candidate_inputs::external(path, sha256, MAX_PIN_BYTES, first)?;
        let expected: PinDescription =
            serde_json::from_slice(&bytes).context("invalid native runner pin JSON")?;
        ensure!(
            expected.schema_version == SCHEMA,
            "native runner pin schema is unsupported"
        );
        ensure!(
            stamp.source_matches_git == "1",
            "native CLI build is dirty or has unknown cleanliness"
        );
        ensure!(
            engine::valid_sha256(stamp.source_sha256),
            "native CLI source stamp is malformed"
        );
        ensure!(
            engine::valid_commit(stamp.git_commit),
            "native CLI Git revision is missing or malformed"
        );
        let executable = engine::running_executable()?;
        ensure!(
            expected.compiled_source_sha256 == stamp.source_sha256,
            "native runner source stamp differs from pin"
        );
        ensure!(
            expected.compiled_git_commit == stamp.git_commit,
            "native runner Git revision differs from pin"
        );
        ensure!(
            expected.executable_sha256 == executable.sha256,
            "native runner executable differs from pin"
        );
        held.verify()?;
        let identity = InstalledIdentity {
            compiled_source_sha256: expected.compiled_source_sha256.clone(),
            compiled_git_commit: expected.compiled_git_commit.clone(),
            executable_sha256: expected.executable_sha256.clone(),
        };
        Ok(Self {
            held,
            expected,
            identity,
            executable,
        })
    }
    pub fn identity(&self) -> &InstalledIdentity {
        &self.identity
    }
    pub fn verify(&self) -> Result<()> {
        self.held.verify().context("native runner pin changed")?;
        let current = engine::running_executable()?;
        ensure!(
            current == self.executable && current.sha256 == self.expected.executable_sha256,
            "running native executable changed after pin capture"
        );
        Ok(())
    }
}

#[cfg(test)]
#[path = "candidate_native_runner_pin_tests.rs"]
mod tests;
