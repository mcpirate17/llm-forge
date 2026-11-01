//! Catalog ingestion through pinned native embeddings into immutable generations.
#[path = "memory_ingest_rows.rs"]
mod rows;
use crate::{
    embedding,
    memory_catalog::{Catalog, InputFile},
    memory_store::Store,
    source_file::SourceRoot,
};
use anyhow::{ensure, Context, Result};
use rows::{Previous, Writer};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const MAX_INDEX_BYTES: usize = 512 * 1024 * 1024;
const QUERY_PREFIX: &str =
    "Instruct: Retrieve the developer note that answers the search query.\nQuery: ";

pub struct Settings {
    pub host: PathBuf,
    pub store: PathBuf,
    pub catalog: Option<PathBuf>,
    pub sources: Vec<String>,
    pub full: bool,
    pub allow_missing_roots: bool,
    pub timeout_seconds: u64,
    pub embedding: embedding::Config,
}
#[derive(Serialize)]
pub struct Report {
    pub changed: bool,
    pub generation: String,
    pub rows: usize,
    pub reused: usize,
    pub embedded: usize,
    pub preserved: usize,
    pub removed: usize,
    pub missing_roots: Vec<(String, PathBuf)>,
    pub embedding_input_tokens: u64,
}

pub fn fingerprint(config: &embedding::Config) -> Result<String> {
    // Schema-3 memory metadata preserves its explicit CPU/GPU guest envelope.
    ensure!(
        config.context_tokens == 2048 && matches!(config.gpu_layers, 0 | 99),
        "memory schema 3 requires context_tokens=2048 and gpu_layers=0 or 99"
    );
    let recipe =
        json!({"protocol":"forge-memory-v1","space":config.space()?,"query_prefix":QUERY_PREFIX});
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&recipe)?)
    ))
}

pub(crate) fn validate_missing_roots(
    pin: &crate::memory_store::Pinned,
    missing: &[(String, PathBuf)],
) -> Result<()> {
    rows::validate_missing_roots(pin, missing)
}

fn previous(store: &Store) -> Result<Option<crate::memory_store::Pinned>> {
    match store.pin() {
        Ok(pin) => Ok(Some(pin)),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                && !store.root().join("current").try_exists()? =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

struct EmbeddingRows<'a> {
    client: embedding::Client,
    config: &'a embedding::Config,
    fingerprint: String,
    deadline: Instant,
    embedded: usize,
    tokens: u64,
}
impl EmbeddingRows<'_> {
    fn append(
        &mut self,
        input: &InputFile,
        hash: &str,
        text: &str,
        output: &mut Writer,
    ) -> Result<()> {
        let path = input.path.to_str().context("memory path must be UTF-8")?;
        let title = input
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .context("memory file lacks UTF-8 name")?;
        let chunks = conductor_native::memory_chunking::chunk_text(
            text,
            &input.source,
            path,
            title,
            &input.mode,
        );
        let mut start = 0;
        while start < chunks.len() {
            ensure!(
                Instant::now() < self.deadline,
                "memory ingestion deadline expired"
            );
            let mut end = start;
            let mut bytes = 0;
            while end < chunks.len() && end - start < 32 && bytes + chunks[end].3.len() <= 65536 {
                bytes += chunks[end].3.len();
                end += 1;
            }
            ensure!(end > start, "memory chunk exceeds embedding batch bound");
            let texts: Vec<_> = chunks[start..end]
                .iter()
                .map(|row| row.3.as_str())
                .collect();
            let batch = self.client.embed_batch(&texts, self.deadline)?;
            self.tokens = self
                .tokens
                .checked_add(batch.input_tokens_total)
                .context("embedding token counter overflow")?;
            for ((source, path, title, text), vector) in
                chunks[start..end].iter().zip(batch.vectors)
            {
                output.value(&json!({"schema_version":3,"source":source,"path":path,"title":title,"text":text,
                    "source_sha256":hash,"recipe_sha256":input.recipe_sha256,"vector":vector,
                    "embedding":{"fingerprint":self.fingerprint,"dimension":self.config.dimension,
                        "paid":false,"num_gpu":self.config.gpu_layers,"num_ctx":self.config.context_tokens,
                        "native_recipe":"forge-memory-v1","model":self.config.model}}))?;
                self.embedded += 1;
            }
            start = end;
        }
        Ok(())
    }
}

fn selected(
    catalog: &Catalog,
    requested: &[String],
) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let available: BTreeSet<_> = catalog
        .entries
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    let selected = if requested.is_empty() {
        available.clone()
    } else {
        requested.iter().cloned().collect()
    };
    ensure!(
        !selected.is_empty() && selected.is_subset(&available),
        "select at least one known index source"
    );
    Ok((available, selected))
}

pub fn ingest(settings: Settings) -> Result<Report> {
    ensure!(
        (1..=3600).contains(&settings.timeout_seconds),
        "memory ingestion deadline must be 1..3600 seconds"
    );
    let deadline = Instant::now() + Duration::from_secs(settings.timeout_seconds);
    let fingerprint = fingerprint(&settings.embedding)?;
    let catalog = Catalog::load(&settings.host, settings.catalog.as_deref())?;
    let (available, selected) = selected(&catalog, &settings.sources)?;
    let store = Store::create(&settings.store)?;
    let building = store.begin()?;
    let old = previous(&store)?;
    if selected != available {
        let prior = old
            .as_ref()
            .context("partial ingestion requires an existing complete native store")?;
        ensure!(
            prior.manifest.fingerprint == fingerprint,
            "embedding recipe changed; full ingestion required"
        );
        let known: BTreeSet<_> = prior.manifest.sources.iter().cloned().collect();
        ensure!(
            available.difference(&known).all(|id| selected.contains(id)),
            "new catalog sources must be selected on partial ingestion"
        );
    }
    let inventory = catalog.scan(&selected)?;
    let mut output = Writer::new(&building.path.join("index.jsonl"))?;
    let previous = old
        .as_ref()
        .map(|pin| Previous::load(pin, &available, &selected, &mut output))
        .transpose()?;
    for (source, root) in &inventory.missing_roots {
        ensure!(
            settings.allow_missing_roots
                || previous
                    .as_ref()
                    .is_none_or(|old| !old.has_root(source, root)),
            "source {source} root {} is missing; refusing implicit removal of prior rows",
            root.display()
        );
    }
    let same_space = old
        .as_ref()
        .is_some_and(|pin| pin.manifest.fingerprint == fingerprint);
    let embeddings = EmbeddingRows {
        client: embedding::Client::new(settings.embedding.clone())?,
        config: &settings.embedding,
        fingerprint,
        deadline,
        embedded: 0,
        tokens: 0,
    };
    let progress = process(
        &inventory.files,
        previous,
        output,
        embeddings,
        !settings.full && same_space,
    )?;
    finish(
        settings.full,
        available,
        old,
        building,
        progress,
        inventory.missing_roots,
    )
}

struct Progress<'a> {
    output: Writer,
    previous: Option<Previous>,
    embeddings: EmbeddingRows<'a>,
    captured: Vec<(&'a InputFile, String)>,
    reused: usize,
    emitted: BTreeSet<(String, String)>,
}
fn process<'a>(
    files: &'a [InputFile],
    previous: Option<Previous>,
    mut output: Writer,
    mut embeddings: EmbeddingRows<'a>,
    reuse: bool,
) -> Result<Progress<'a>> {
    let mut reused = 0;
    let mut source_bytes = 0;
    let mut emitted = BTreeSet::new();
    let mut captured = Vec::new();
    for input in files {
        ensure!(
            Instant::now() < embeddings.deadline,
            "memory ingestion deadline expired"
        );
        let snapshot = SourceRoot::open(&input.root)?.read(&input.relative)?;
        source_bytes += snapshot.content().len();
        ensure!(
            source_bytes <= MAX_INDEX_BYTES,
            "memory source content exceeds 512 MiB per ingestion"
        );
        let path = input.path.to_str().context("memory path must be UTF-8")?;
        let before = output.count;
        let keep = if reuse {
            previous
                .as_ref()
                .map(|old| {
                    old.reuse(
                        &input.source,
                        path,
                        snapshot.sha256(),
                        &input.recipe_sha256,
                        &mut output,
                    )
                })
                .transpose()?
                .flatten()
        } else {
            None
        };
        if let Some(count) = keep {
            reused += count;
        } else {
            embeddings.append(input, snapshot.sha256(), snapshot.content(), &mut output)?;
        }
        if output.count > before {
            emitted.insert((input.source.clone(), path.to_owned()));
        }
        captured.push((input, snapshot.sha256().to_owned()));
    }
    Ok(Progress {
        output,
        previous,
        embeddings,
        captured,
        reused,
        emitted,
    })
}

fn finish(
    full: bool,
    available: BTreeSet<String>,
    old: Option<crate::memory_store::Pinned>,
    building: crate::memory_store::Building,
    progress: Progress<'_>,
    missing_roots: Vec<(String, PathBuf)>,
) -> Result<Report> {
    let Progress {
        output,
        previous,
        embeddings,
        captured,
        reused,
        emitted,
    } = progress;
    ensure!(output.count > 0, "memory ingestion produced no chunks");
    for (input, hash) in captured {
        ensure!(
            Instant::now() < embeddings.deadline,
            "memory ingestion deadline expired before publication"
        );
        ensure!(
            SourceRoot::open(&input.root)?
                .read(&input.relative)?
                .sha256()
                == hash,
            "memory source changed before publication: {}",
            input.path.display()
        );
    }
    if let Some(pin) = &old {
        pin.reader.verify_source_hash()?;
    }
    let preserved = previous.as_ref().map_or(0, |old| old.preserved);
    let removed = previous.as_ref().map_or(0, |old| old.removed(&emitted));
    let sources: Vec<_> = available.into_iter().collect();
    let changed = full
        || embeddings.embedded > 0
        || removed > 0
        || old
            .as_ref()
            .is_none_or(|pin| pin.manifest.sources != sources);
    let rows = output.finish()?;
    let manifest = if changed {
        building.publish(sources)?
    } else {
        old.context("missing unchanged memory store")?.manifest
    };
    Ok(Report {
        changed,
        generation: manifest.generation,
        rows,
        reused,
        embedded: embeddings.embedded,
        preserved,
        removed,
        missing_roots,
        embedding_input_tokens: embeddings.tokens,
    })
}

pub fn search(
    store: &Store,
    config: embedding::Config,
    task: &str,
    top_k: usize,
) -> Result<serde_json::Value> {
    ensure!(
        !task.trim().is_empty() && task.len() <= 4096 && (1..=64).contains(&top_k),
        "invalid memory search task or hit count"
    );
    let pin = store.pin()?;
    ensure!(
        pin.manifest.fingerprint == fingerprint(&config)?,
        "query embedding recipe differs from memory generation"
    );
    let vector = embedding::Client::new(config)?
        .embed(&format!("{QUERY_PREFIX}{}", task.trim()))?
        .vector;
    let vector: Vec<_> = vector.into_iter().map(f64::from).collect();
    let hits = pin
        .reader
        .query(&vector, top_k, &pin.manifest.fingerprint)?;
    let hits:Vec<_>=hits.into_iter().map(|(score,source,path,title,text)|json!({"score":score,"source":source,"path":path,"title":title,"text":text})).collect();
    Ok(
        json!({"schema_version":1,"generation":pin.manifest.generation,"embedding_fingerprint":pin.manifest.fingerprint,"hits":hits}),
    )
}
