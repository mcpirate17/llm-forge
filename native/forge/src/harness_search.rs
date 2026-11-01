//! Bounded per-capability reader pools. CPU, SQLite and HTTP work never occupy
//! the Tokio reactor. Reader leases pin immutable generations while calls run.
use super::{
    policy::{CodeIndex, Policy},
    Blocking,
};
use crate::{
    code_index::{QueryRequest, Reader, SearchMode, Store},
    embedding,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

pub struct Searches {
    indexes: BTreeMap<String, Arc<Index>>,
    workers: Blocking,
}
struct Index {
    source_root: PathBuf,
    policy: CodeIndex,
    store: Store,
    embedding: Option<embedding::Client>,
    slots: Arc<Semaphore>,
    readers: Mutex<Vec<Reader>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    index: String,
    text: String,
    kind: Option<Kind>,
    mode: Option<SearchMode>,
    limit: Option<usize>,
    candidates: Option<usize>,
    ef: Option<usize>,
}
#[derive(Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Lexical,
    Hybrid,
    Semantic,
}
impl Searches {
    pub fn new(policy: &Policy, workers: Blocking) -> Result<Self> {
        let mut indexes = BTreeMap::new();
        for (name, config) in &policy.code_indexes {
            let store = Store::open(&config.directory)?;
            let embedding = config
                .embedding
                .clone()
                .map(embedding::Client::new)
                .transpose()?;
            indexes.insert(
                name.clone(),
                Arc::new(Index {
                    source_root: policy.source_root.clone(),
                    policy: config.clone(),
                    store,
                    embedding,
                    slots: Arc::new(Semaphore::new(config.readers)),
                    readers: Mutex::new(Vec::with_capacity(config.readers)),
                }),
            );
        }
        Ok(Self { indexes, workers })
    }
    pub async fn call(&self, input: &str) -> Result<Value> {
        let request: Request = serde_json::from_str(input)?;
        let index = self
            .indexes
            .get(&request.index)
            .context("index lacks an operator capability")?
            .clone();
        let candidates = request.candidates.unwrap_or(100);
        let query = QueryRequest {
            text: request.text,
            vector: None,
            space: None,
            mode: request.mode.unwrap_or_default(),
            limit: request.limit.unwrap_or(10),
            candidates,
            ef: request.ef.unwrap_or(160.max(candidates)),
        };
        query.validate()?;
        let slot = index.slots.clone().acquire_owned().await?;
        self.workers
            .run(move || {
                let _slot = slot;
                index.search(request.kind, query)
            })
            .await
    }
}
impl Index {
    fn checkout(self: &Arc<Self>) -> Result<Borrowed> {
        let mut reader = self
            .readers
            .lock()
            .map_err(|_| anyhow::anyhow!("index reader cache poisoned"))?
            .pop();
        let current = self.store.current()?;
        if reader
            .as_ref()
            .is_some_and(|reader| reader.manifest().generation != current)
        {
            reader = None;
        }
        if reader.is_none() {
            reader = Some(Reader::open_limited(&self.store, self.policy.limits()?)?);
        }
        let reader = reader.context("missing admitted index reader")?;
        ensure!(
            reader.manifest().source_root == self.source_root,
            "index belongs to a different source root"
        );
        Ok(Borrowed {
            index: self.clone(),
            reader: Some(reader),
        })
    }
    fn search(self: &Arc<Self>, kind: Option<Kind>, mut query: QueryRequest) -> Result<Value> {
        let mut borrowed = self.checkout()?;
        let reader = borrowed
            .reader
            .as_mut()
            .context("missing checked-out reader")?;
        let kind = kind.unwrap_or(if self.embedding.is_some() {
            Kind::Hybrid
        } else {
            Kind::Lexical
        });
        let mut input_tokens = 0;
        if kind != Kind::Lexical {
            let client = self
                .embedding
                .as_ref()
                .context("index has no operator embedding capability")?;
            ensure!(
                client.space() == &reader.manifest().space,
                "index embedding recipe differs from operator provider"
            );
            if reader.manifest().rows > 0 {
                let embedding = client.embed(&query.text)?;
                query.vector = Some(embedding.vector);
                query.space = Some(embedding.space);
                input_tokens = embedding.input_tokens;
            }
        }
        let performed = query.vector.is_some();
        if kind == Kind::Semantic && performed {
            query.text.clear();
        }
        let mut result = serde_json::to_value(reader.search(&self.source_root, query)?)?;
        result["kind"] = serde_json::to_value(kind)?;
        result["embedding_performed"] = Value::Bool(performed);
        result["embedding_input_tokens"] = Value::from(input_tokens);
        result["space"] = serde_json::to_value(&reader.manifest().space)?;
        Ok(result)
    }
}
struct Borrowed {
    index: Arc<Index>,
    reader: Option<Reader>,
}
impl Drop for Borrowed {
    fn drop(&mut self) {
        let Some(reader) = self.reader.take() else {
            return;
        };
        match self.index.readers.lock() {
            Ok(mut readers) if readers.len() < self.index.policy.readers => readers.push(reader),
            Ok(_) => eprintln!("forge: index cache exceeded admitted slots; discarding reader"),
            Err(_) => eprintln!("forge: index cache poisoned; discarding reader"),
        }
    }
}
