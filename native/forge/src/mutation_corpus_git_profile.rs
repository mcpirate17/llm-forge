//! Operator-asserted system attributes for one exactly pinned Git build.
use crate::candidate_inputs::{self, BoundFile};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};
#[derive(Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum SystemAttributes {
    Path { path: PathBuf },
    None {},
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u32,
    git_sha256: String,
    system_attributes: SystemAttributes,
}
pub(crate) struct Profile {
    held: BoundFile,
    path: PathBuf,
    sha256: String,
    document: Document,
}
impl Profile {
    pub(crate) fn open(path: &Path, sha256: &str, git_sha256: &str, source: &Path) -> Result<Self> {
        let (held, bytes) = candidate_inputs::external(path, sha256, 64 * 1024, source)?;
        let value =
            crate::tool_schema::parse_document_with_limit(std::str::from_utf8(&bytes)?, 64 * 1024)?;
        let document: Document = serde_json::from_value(value)?;
        ensure!(
            document.schema_version == 1,
            "unsupported Git build profile schema"
        );
        conductor_native::sha256_hex::parse(&document.git_sha256).map_err(anyhow::Error::msg)?;
        ensure!(
            document.git_sha256 == git_sha256,
            "Git build profile does not bind the selected Git SHA-256"
        );
        if let SystemAttributes::Path { path } = &document.system_attributes {
            ensure!(
                path.is_absolute()
                    && path
                        .components()
                        .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
                    && path.as_os_str().as_encoded_bytes().len() <= 4096,
                "profile system attributes path must be bounded, absolute and normalized"
            );
        }
        let profile = Self {
            held,
            path: path.into(),
            sha256: sha256.into(),
            document,
        };
        profile.verify()?;
        Ok(profile)
    }
    pub(crate) fn verify_for(&self, git_sha256: &str) -> Result<()> {
        self.verify()?;
        ensure!(
            self.document.git_sha256 == git_sha256,
            "Git build profile SHA-256 binding differs from executable pin"
        );
        Ok(())
    }
    pub(crate) fn system_path(&self) -> Option<&Path> {
        match &self.document.system_attributes {
            SystemAttributes::Path { path } => Some(path),
            SystemAttributes::None {} => None,
        }
    }
    pub(crate) fn verify(&self) -> Result<()> {
        self.held.verify().context("Git build profile changed")
    }
    pub(crate) fn provenance(&self) -> serde_json::Value {
        serde_json::json!({"profile_path":self.path,"profile_sha256":self.sha256,
            "git_sha256":self.document.git_sha256,"system_attributes":self.document.system_attributes,
            "authority":"operator-asserted build profile; not independently runtime-verified"})
    }
}
#[cfg(test)]
#[path = "mutation_corpus_git_profile_tests.rs"]
mod tests;
