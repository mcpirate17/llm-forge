//! Held candidate blobs and explicitly pinned import-provider observations.
use crate::{
    candidate_inputs::{self, BoundFile},
    candidate_policy::Policy,
    candidate_source::{Candidate, Source, TreeEntry},
};
use anyhow::{ensure, Context, Result};
use cap_std::fs::Dir;
use conductor_native::candidate_cargo_lint::read_optional;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub const MAX_FILE: usize = 1024 * 1024;
pub const MAX_TOTAL: usize = 32 * 1024 * 1024;

pub struct Capture {
    root: PathBuf,
    directory: Dir,
    pub entries: BTreeMap<String, TreeEntry>,
    texts: BTreeMap<String, String>,
    held: Vec<BoundFile>,
    absent: Vec<String>,
    pub bytes: usize,
}
impl Capture {
    pub fn new(
        source: &Source,
        candidate: &Candidate,
        root: &Path,
        policy: &Policy,
    ) -> Result<Self> {
        let entries = source
            .list_tree(&candidate.tree_oid)?
            .into_iter()
            .map(|entry| (entry.path.clone(), entry))
            .collect();
        let mut capture = Self {
            root: root.into(),
            directory: Dir::open_ambient_dir(root, cap_std::ambient_authority())?,
            entries,
            texts: BTreeMap::new(),
            held: Vec::new(),
            absent: Vec::new(),
            bytes: 0,
        };
        let policy_text = capture.read(&policy.path)?;
        ensure!(
            format!("{:x}", Sha256::digest(policy_text.as_bytes())) == policy.digest,
            "captured analysis policy differs from classified policy"
        );
        for path in ["forge.toml", "pyproject.toml"] {
            if capture.entries.contains_key(path) {
                capture.read(path)?;
            } else {
                capture.absence(path)?;
            }
        }
        Ok(capture)
    }
    pub fn read(&mut self, path: &str) -> Result<&str> {
        if !self.texts.contains_key(path) {
            ensure!(
                self.texts.len() < 4096,
                "analysis inventory exceeds 4096 files"
            );
            let entry = self
                .entries
                .get(path)
                .context("analysis path has no captured Git entry")?;
            ensure!(
                entry.object_type == "blob"
                    && matches!(entry.mode.as_str(), "100644" | "100755")
                    && entry.size.is_some_and(|size| size <= MAX_FILE as u64),
                "analysis input must be a regular Git blob of at most 1 MiB: {path}"
            );
            let (held, bytes) = candidate_inputs::snapshot(&self.root, path, &entry.oid, MAX_FILE)?;
            self.bytes = self
                .bytes
                .checked_add(bytes.len())
                .context("analysis byte count overflow")?;
            ensure!(
                self.bytes <= MAX_TOTAL,
                "analysis captured sources exceed 32 MiB"
            );
            let text = String::from_utf8(bytes).context("analysis source must be UTF-8")?;
            self.held.push(held);
            self.texts.insert(path.into(), text);
        }
        Ok(self
            .texts
            .get(path)
            .context("analysis capture disappeared")?)
    }
    pub fn absence(&mut self, path: &str) -> Result<()> {
        ensure!(
            read_optional(&self.directory, path, MAX_FILE)
                .map_err(anyhow::Error::msg)?
                .is_none(),
            "captured absent analysis path is present: {path}"
        );
        self.absent.push(path.into());
        Ok(())
    }
    pub fn verify(&self) -> Result<()> {
        for held in &self.held {
            held.verify()?;
        }
        for path in &self.absent {
            ensure!(
                read_optional(&self.directory, path, MAX_FILE)
                    .map_err(anyhow::Error::msg)?
                    .is_none(),
                "absent analysis path appeared: {path}"
            );
        }
        Ok(())
    }
    pub fn strict_trees(&mut self) -> Result<Vec<String>> {
        let config = if self.entries.contains_key("forge.toml") {
            Some(
                conductor_native::project_config::parse_native(
                    Path::new("forge.toml"),
                    self.read("forge.toml")?,
                )
                .map_err(anyhow::Error::msg)?,
            )
        } else if self.entries.contains_key("pyproject.toml") {
            Some(
                conductor_native::project_config::parse_legacy(
                    Path::new("pyproject.toml"),
                    self.read("pyproject.toml")?,
                )
                .map_err(anyhow::Error::msg)?,
            )
        } else {
            None
        };
        let package = match config
            .as_ref()
            .and_then(|config| config.conductor.get("package_root"))
        {
            Some(value) => value.as_str().context("package_root must be a string")?,
            None => "conductor",
        };
        let package = conductor_native::project_paths::relative(package, "captured package_root")
            .map_err(anyhow::Error::msg)?;
        let tooling = Path::new(&package)
            .parent()
            .unwrap_or(Path::new(""))
            .join("tooling");
        Ok(vec![
            package,
            tooling
                .to_str()
                .context("tooling path must be UTF-8")?
                .into(),
        ])
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderSpec {
    path: PathBuf,
    sha256: String,
}
fn provider_spec(raw: &str) -> Result<ProviderSpec> {
    ensure!(
        raw.len() <= 128 * 1024,
        "provider specification exceeds 128 KiB"
    );
    Ok(serde_json::from_value(crate::tool_schema::parse_document(
        raw,
    )?)?)
}

pub(crate) fn validate_operator_provider(raw: &str, excluded: &[&Path]) -> Result<()> {
    let spec = provider_spec(raw)?;
    ensure!(
        spec.path.is_absolute() && spec.path.canonicalize()? == spec.path,
        "provider path must be absolute and canonical"
    );
    ensure!(
        excluded.iter().all(|root| !spec.path.starts_with(root)),
        "operator provider observation must be outside source and scratch roots"
    );
    Ok(())
}

pub struct Provider {
    held: BoundFile,
    pub evidence: Value,
    pub payload: Value,
}
impl Provider {
    pub fn load(raw: &str, snapshot: &Path) -> Result<Self> {
        let spec = provider_spec(raw)?;
        let (held, bytes) =
            candidate_inputs::external(&spec.path, &spec.sha256, MAX_TOTAL, snapshot)
                .context("bind operator import-provider observation")?;
        let value =
            crate::tool_schema::parse_document_with_limit(std::str::from_utf8(&bytes)?, MAX_TOTAL)?;
        let object = value
            .as_object()
            .context("provider observation must be an object")?;
        let keys = [
            "schema_version",
            "implementation",
            "version",
            "platform",
            "stdlib",
            "modules",
        ];
        ensure!(
            object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key)),
            "provider observation has missing or unknown fields"
        );
        ensure!(
            value["schema_version"] == 1,
            "unsupported provider observation schema"
        );
        let payload = json!({"stdlib":value["stdlib"],"modules":value["modules"],"provenance":{
            "schema_version":1,"implementation":value["implementation"],"version":value["version"],
            "platform":value["platform"],"sha256":spec.sha256}});
        Ok(Self {
            held,
            evidence: json!({"path":spec.path,"sha256":spec.sha256}),
            payload,
        })
    }
    pub fn verify(&self) -> Result<()> {
        self.held.verify()
    }
}
