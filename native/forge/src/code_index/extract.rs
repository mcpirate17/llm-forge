//! Bounded source planning and ordered, batched embedding refresh.
use super::{
    build::{build_checked, BuildHeader, EmbeddingRow},
    Manifest, Store, MAX_CHUNK_BYTES,
};
use crate::{embedding, source_file::SourceRoot};
use anyhow::{ensure, Context, Result};
use conductor_native::{
    syntax::{self, Language},
    vector_index::{Config, MAX_ROWS, MAX_VECTOR_BYTES},
};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub paths: Vec<String>,
    pub chunk_bytes: usize,
    pub max_source_bytes: usize,
    #[serde(default)]
    pub allow_syntax_errors: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refresh {
    pub sources: Options,
    pub embedding: embedding::Config,
    #[serde(default)]
    pub graph: Config,
    pub deadline_seconds: u64,
}
#[derive(Debug, Serialize)]
pub struct Plan {
    pub source_root: PathBuf,
    pub source_bytes: usize,
    pub rows: usize,
    pub files: Vec<PlannedFile>,
}
#[derive(Debug, Serialize)]
pub struct PlannedFile {
    pub path: String,
    pub source_sha256: String,
    pub spans: Vec<[usize; 2]>,
    pub syntax_error: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct RefreshReport {
    pub manifest: Manifest,
    pub source_bytes: usize,
    pub diagnostics: Vec<(String, String)>,
    pub batches: u64,
    pub input_tokens_total: u64,
}

/// Read-only planning: no embedding requests or index mutations.
pub fn plan(source: &Path, options: &Options) -> Result<Plan> {
    plan_checked(source, options, || Ok(()))
}
fn plan_checked(
    source: &Path,
    options: &Options,
    mut checkpoint: impl FnMut() -> Result<()>,
) -> Result<Plan> {
    ensure!(
        (128..=MAX_CHUNK_BYTES).contains(&options.chunk_bytes)
            && options.paths.len() <= 100000
            && options.paths.iter().all(|p| p.len() <= 4096)
            && (1..=1024 * 1024 * 1024).contains(&options.max_source_bytes),
        "invalid source extraction limits"
    );
    let mut paths = options.paths.clone();
    paths.sort();
    ensure!(
        !paths.windows(2).any(|p| p[0] == p[1]),
        "duplicate source path in plan"
    );
    let source_root = source.canonicalize()?;
    let root = SourceRoot::open(&source_root)?;
    let mut plan = Plan {
        source_root,
        source_bytes: 0,
        rows: 0,
        files: Vec::with_capacity(paths.len()),
    };
    for path in paths {
        checkpoint()?;
        crate::harness::policy::source_path(Path::new(&path))?;
        ensure!(
            path.split('/')
                .all(|p| !p.is_empty() && p != "." && p != ".."),
            "source paths must be normalized"
        );
        let source = root.read(Path::new(&path))?;
        plan.source_bytes += source.content().len();
        ensure!(
            plan.source_bytes <= options.max_source_bytes,
            "source plan exceeds aggregate byte limit"
        );
        let (ends, error) = boundaries(
            source.content(),
            Path::new(&path),
            options.allow_syntax_errors,
        )?;
        let spans = split(source.content(), options.chunk_bytes, &ends);
        plan.rows += spans.len();
        ensure!(
            plan.rows <= MAX_ROWS,
            "source plan exceeds chunk count limit"
        );
        plan.files.push(PlannedFile {
            path,
            source_sha256: source.sha256().to_owned(),
            spans,
            syntax_error: error,
        });
    }
    Ok(plan)
}
fn boundaries(
    source: &str,
    path: &Path,
    allow_errors: bool,
) -> Result<(BTreeSet<usize>, Option<String>)> {
    let Some(language) = Language::from_path(path) else {
        return Ok((BTreeSet::new(), None));
    };
    match syntax::inspect(source, language) {
        Ok(ast) => Ok((
            ast.symbols
                .iter()
                .flat_map(|symbol| [symbol.byte_start, symbol.byte_end])
                .collect(),
            None,
        )),
        Err(error) if allow_errors => Ok((BTreeSet::new(), Some(format!("{error:#}")))),
        Err(error) => Err(error).with_context(|| {
            format!(
                "cannot plan {} without explicit allow_syntax_errors",
                path.display()
            )
        }),
    }
}
fn split(source: &str, maximum: usize, ends: &BTreeSet<usize>) -> Vec<[usize; 2]> {
    let mut spans = Vec::new();
    let mut start = 0;
    while start < source.len() {
        let mut end = source.len().min(start + maximum);
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        if end < source.len() {
            let minimum = start + maximum / 2;
            if let Some(&boundary) = ends.range(minimum..=end).next_back() {
                end = boundary;
            } else if let Some(newline) = source[start..end].rfind('\n') {
                if start + newline + 1 >= minimum {
                    end = start + newline + 1;
                }
            }
        }
        spans.push([start, end]);
        start = end;
    }
    spans
}

pub fn refresh(store: &Store, source: &Path, settings: Refresh) -> Result<RefreshReport> {
    ensure!(
        (1..=3600).contains(&settings.deadline_seconds),
        "refresh deadline must be 1..3600 seconds"
    );
    let deadline = Instant::now() + Duration::from_secs(settings.deadline_seconds);
    let mut checkpoint = || {
        ensure!(Instant::now() < deadline, "source refresh deadline expired");
        Ok(())
    };
    settings.graph.validate()?;
    let client = embedding::Client::new(settings.embedding)?;
    let plan = plan_checked(source, &settings.sources, &mut checkpoint)?;
    let source_bytes = plan.source_bytes;
    let diagnostics: Vec<_> = plan
        .files
        .iter()
        .filter_map(|file| {
            file.syntax_error
                .as_ref()
                .map(|error| (file.path.clone(), error.clone()))
        })
        .collect();
    ensure!(
        serde_json::to_vec(&diagnostics)?.len() <= 65536,
        "syntax diagnostics exceed report budget; fix sources or narrow scope"
    );
    ensure!(
        (plan.rows as u64) * client.space().dimension as u64 * 4 <= MAX_VECTOR_BYTES,
        "planned embeddings exceed 4 GiB"
    );
    let rows = plan.rows;
    let metrics = Cell::new((0u64, 0u64));
    let stream = EmbeddedRows {
        client,
        plan,
        root: SourceRoot::open(source)?,
        file: 0,
        span: 0,
        pending: VecDeque::new(),
        failed: false,
        deadline,
        metrics: &metrics,
    };
    let header = BuildHeader {
        version: 1,
        rows,
        space: stream.client.space().clone(),
        config: settings.graph,
    };
    let manifest = build_checked(store, source, header, stream, checkpoint)?;
    let (batches, input_tokens_total) = metrics.get();
    Ok(RefreshReport {
        manifest,
        source_bytes,
        diagnostics,
        batches,
        input_tokens_total,
    })
}

struct EmbeddedRows<'a> {
    client: embedding::Client,
    plan: Plan,
    root: SourceRoot,
    file: usize,
    span: usize,
    pending: VecDeque<EmbeddingRow>,
    failed: bool,
    deadline: Instant,
    metrics: &'a Cell<(u64, u64)>,
}
impl EmbeddedRows<'_> {
    fn fill(&mut self) -> Result<()> {
        let mut items = Vec::new();
        let mut texts = Vec::new();
        let mut bytes = 0;
        while self.file < self.plan.files.len() && items.len() < 32 {
            let planned = &self.plan.files[self.file];
            if self.span == planned.spans.len() {
                self.file += 1;
                self.span = 0;
                continue;
            }
            let source = self.root.read(Path::new(&planned.path))?;
            ensure!(
                source.sha256() == planned.source_sha256,
                "source changed after planning: {}",
                planned.path
            );
            while self.span < planned.spans.len() && items.len() < 32 {
                let [start, end] = planned.spans[self.span];
                if bytes + end - start > 65536 {
                    break;
                }
                texts.push(source.content()[start..end].to_owned());
                bytes += end - start;
                items.push(EmbeddingRow {
                    path: planned.path.clone(),
                    source_sha256: planned.source_sha256.clone(),
                    start_byte: start,
                    end_byte: end,
                    vector: Vec::new(),
                });
                self.span += 1;
            }
            if self.span < planned.spans.len() {
                break;
            }
        }
        if items.is_empty() {
            return Ok(());
        }
        let inputs: Vec<_> = texts.iter().map(String::as_str).collect();
        let batch = self.client.embed_batch(&inputs, self.deadline)?;
        let (batches, tokens) = self.metrics.get();
        self.metrics
            .set((batches + 1, tokens + batch.input_tokens_total));
        ensure!(
            batch.vectors.len() == items.len(),
            "embedding batch lost row identity"
        );
        for (mut row, vector) in items.into_iter().zip(batch.vectors) {
            row.vector = vector;
            self.pending.push_back(row);
        }
        Ok(())
    }
}
impl Iterator for EmbeddedRows<'_> {
    type Item = Result<EmbeddingRow>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        if self.pending.is_empty() {
            if let Err(error) = self.fill() {
                self.failed = true;
                return Some(Err(error));
            }
        }
        self.pending.pop_front().map(Ok)
    }
}
