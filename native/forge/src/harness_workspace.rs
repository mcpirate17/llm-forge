//! Operator-selected workspace context and bounded immutable memory readers.
use super::{policy::Policy, Blocking};
use anyhow::{ensure, Context, Result};
use conductor_native::{memory_index_sidecar::Reader, sha256_hex};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// Harness vectors use the closed tool grammar; the standalone memory API may
// support larger vectors independently.
pub(super) const MAX_MEMORY_DIMENSION: usize = 4096;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BriefCapability {
    pub identity: Option<String>,
    pub max_chars: usize,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryCapability {
    pub source: PathBuf,
    pub snapshot: PathBuf,
    pub fingerprint: String,
    pub dimension: usize,
    pub max_rows: usize,
    pub max_mapped_mib: u64,
}

pub(super) fn reservation(policy: &Policy) -> Result<u64> {
    ensure!(
        policy.memory_indexes.len() <= 8,
        "too many memory capabilities"
    );
    let mut total = 0;
    if let Some(brief) = &policy.workspace {
        ensure!(
            (1024..=16384).contains(&brief.max_chars),
            "invalid brief capability budget"
        );
        if let Some(identity) = &brief.identity {
            conductor_native::a2a_store::validate_identity(identity)?;
        }
        total += 32;
    }
    for (name, memory) in &policy.memory_indexes {
        crate::task_store::validate_id(name)?;
        sha256_hex::parse(
            memory
                .fingerprint
                .strip_prefix("sha256:")
                .context("invalid memory fingerprint")?,
        )
        .map_err(anyhow::Error::msg)?;
        ensure!(
            (1..=MAX_MEMORY_DIMENSION).contains(&memory.dimension),
            "workspace_recall memory dimension {} is outside supported range 1..={MAX_MEMORY_DIMENSION}",
            memory.dimension
        );
        ensure!(
            (1..=1_000_000).contains(&memory.max_rows)
                && (1..=2048).contains(&memory.max_mapped_mib),
            "invalid memory capability limits"
        );
        // One pinned reader, with serialized per-index queries and bounded row decoding.
        total += memory.max_mapped_mib + 16;
    }
    Ok(total)
}

pub(super) fn validate(policy: &Policy, state: &Path) -> Result<()> {
    for memory in policy.memory_indexes.values() {
        for path in [&memory.source, &memory.snapshot] {
            ensure!(
                path.is_absolute(),
                "memory capability paths must be absolute"
            );
            let canonical = path.canonicalize()?;
            ensure!(
                &canonical == path,
                "memory capability paths must be canonical without symlinks"
            );
            ensure!(
                !path.starts_with(state),
                "memory capability cannot expose harness state"
            );
            let metadata = std::fs::symlink_metadata(path)?;
            ensure!(
                metadata.is_file(),
                "memory capability must be a regular file"
            );
        }
        ensure!(
            memory.source != memory.snapshot,
            "source and snapshot must differ"
        );
        checked_reader(memory)?;
    }
    Ok(())
}

fn checked_reader(config: &MemoryCapability) -> Result<Reader> {
    let reader = Reader::open_limited(
        &config.source,
        &config.snapshot,
        config.max_rows,
        config.max_mapped_mib * 1024 * 1024,
    )?;
    let metadata = reader.metadata();
    ensure!(
        metadata.fingerprint == config.fingerprint && metadata.dimension == config.dimension,
        "memory snapshot differs from operator vector space"
    );
    Ok(reader)
}

#[derive(PartialEq, Eq)]
struct Identity([u64; 9]);
fn identity(path: &Path) -> Result<Identity> {
    let m = std::fs::symlink_metadata(path)?;
    ensure!(
        m.is_file(),
        "memory capability no longer names a regular file"
    );
    Ok(Identity([
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime() as u64,
        m.mtime_nsec() as u64,
        m.ctime() as u64,
        m.ctime_nsec() as u64,
        m.mode() as u64,
        m.nlink(),
    ]))
}
struct Cached {
    identity: (Identity, Identity),
    reader: Reader,
}
struct Index {
    config: MemoryCapability,
    cached: Mutex<Option<Cached>>,
    slot: Arc<tokio::sync::Semaphore>,
}
impl Index {
    fn query(&self, query: &crate::recall::Query, count: usize) -> Result<Value> {
        let mut cached = self
            .cached
            .lock()
            .map_err(|_| anyhow::anyhow!("memory reader cache poisoned"))?;
        let before = (
            identity(&self.config.source)?,
            identity(&self.config.snapshot)?,
        );
        if cached.as_ref().is_none_or(|cache| cache.identity != before) {
            // Drop the old mapping before admitting a replacement to retain one-map accounting.
            *cached = None;
            let reader = checked_reader(&self.config)?;
            let after = (
                identity(&self.config.source)?,
                identity(&self.config.snapshot)?,
            );
            ensure!(
                before == after,
                "memory files changed during reader admission; retry"
            );
            *cached = Some(Cached {
                identity: after,
                reader,
            });
        }
        let reader = &cached
            .as_ref()
            .context("missing admitted memory reader")?
            .reader;
        let hits = reader.query(query.vector(), count, query.fingerprint())?;
        let after = (
            identity(&self.config.source)?,
            identity(&self.config.snapshot)?,
        );
        ensure!(
            cached.as_ref().is_some_and(|c| c.identity == after),
            "memory paths changed during query; retry"
        );
        let hits: Vec<_> = hits.into_iter().map(|(score,source,path,title,text)|json!({"score":score,"source":source,"path":path,"title":title,"text":text})).collect();
        let result = json!({"schema_version":1,"authority":"non-authoritative-workspace-recall","mode":"mapped-f32","metadata":reader.metadata(),"hits":hits});
        ensure!(
            serde_json::to_vec(&result)?.len() <= 1024 * 1024,
            "recall response exceeds 1 MiB; request fewer hits"
        );
        Ok(result)
    }
}

pub(super) struct Workspace {
    root: PathBuf,
    brief: Option<BriefCapability>,
    indexes: BTreeMap<String, Arc<Index>>,
    workers: Blocking,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BriefRequest {
    task: String,
    #[serde(default)]
    paths: Vec<String>,
    max_chars: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecallRequest {
    index: String,
    task: String,
    query: Value,
    limit: Option<usize>,
}
impl Workspace {
    pub(super) fn new(policy: &Policy, workers: Blocking) -> Self {
        Self {
            root: policy.source_root.clone(),
            brief: policy.workspace.clone(),
            workers,
            indexes: policy
                .memory_indexes
                .iter()
                .map(|(name, config)| {
                    (
                        name.clone(),
                        Arc::new(Index {
                            config: config.clone(),
                            cached: Mutex::new(None),
                            slot: Arc::new(tokio::sync::Semaphore::new(1)),
                        }),
                    )
                })
                .collect(),
        }
    }
    pub(super) async fn call(&self, name: &str, input: &str) -> Result<Value> {
        match name {
            "session_brief" => {
                let config = self
                    .brief
                    .clone()
                    .context("workspace brief lacks an operator capability")?;
                let request: BriefRequest = serde_json::from_str(input)?;
                let max_chars = request.max_chars.unwrap_or(config.max_chars);
                ensure!(
                    max_chars <= config.max_chars,
                    "brief exceeds operator budget"
                );
                let root = self.root.clone();
                self.workers
                    .run(move || {
                        Ok(serde_json::to_value(crate::session_brief::capture(
                            &root,
                            &request.task,
                            &request.paths,
                            config.identity.as_deref(),
                            max_chars,
                        )?)?)
                    })
                    .await
            }
            "workspace_recall" => {
                let request: RecallRequest = serde_json::from_str(input)?;
                let index = self
                    .indexes
                    .get(&request.index)
                    .context("memory index lacks an operator capability")?
                    .clone();
                let query = crate::recall::Query::from_document(request.query, &request.task)?;
                let count = request.limit.unwrap_or(8);
                ensure!((1..=64).contains(&count), "recall limit must be 1..64");
                let slot = index.slot.clone().acquire_owned().await?;
                self.workers
                    .run(move || {
                        let _slot = slot;
                        index.query(&query, count)
                    })
                    .await
            }
            _ => anyhow::bail!("unknown workspace tool"),
        }
    }
}
