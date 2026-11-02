//! Opt-in, bounded real-source benchmark; never contacts a model without --run.
#[path = "real_code_index/metrics.rs"]
mod metrics;
#[path = "real_code_index/embedding.rs"]
mod query_embedding;
use anyhow::{ensure, Context, Result};
use conductor_native::vector_index::{Query, QueryWorkspace, Snapshot};
use forge::{
    code_index::{extract, QueryRequest, Reader, SearchMode, Store},
    embedding,
};
use metrics::{Intent, Workload};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const MODEL: &str = "qwen3-embed-cpu:latest";
const ARTIFACT: &str = "fdf7067d896175e871d925acacde4df653a1e6900a946710342ce972372dcf81";
const K: usize = 10;
static COMPLETED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static FAILED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static CENSORED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
use std::io::{Read, Write};
use std::sync::atomic::Ordering::Relaxed;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipe {
    source_root: PathBuf,
    index: PathBuf,
    embedding: embedding::Config,
    #[serde(default = "default_deadline")]
    deadline_seconds: u64,
    chunk_bytes: usize,
    #[serde(default)]
    batch: extract::BatchPolicy,
}
fn default_deadline() -> u64 {
    600
}
struct Bench {
    root: PathBuf,
    store: Store,
    workload: Workload,
    sources: BTreeMap<String, String>,
    deadline: Instant,
    frozen_sources: BTreeMap<String, String>,
}
fn source_revision(root: &Path) -> Result<String> {
    let repository = git2::Repository::discover(root)?;
    let head = repository.head()?;
    Ok(head
        .target()
        .context("source HEAD lacks commit ID")?
        .to_string())
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn executable_hash() -> Result<String> {
    let mut file = fs::File::open("/proc/self/exe")?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Ok(format!("{:x}", digest.finalize()));
        }
        digest.update(&buffer[..count]);
    }
}
fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    forge::bounded_file::read(path, usize::try_from(limit)?)
}
fn check_deadline(deadline: Instant) -> Result<()> {
    ensure!(Instant::now() < deadline, "benchmark deadline expired");
    Ok(())
}
fn rss() -> Result<Value> {
    let status = fs::read_to_string("/proc/self/status")?;
    let find = |name: &str| -> Result<u64> {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .context("missing Linux RSS counter")?
            .split_whitespace()
            .next()
            .context("empty RSS counter")?
            .parse()
            .map_err(Into::into)
    };
    Ok(json!({"rss_kib":find("VmRSS:")?,"peak_rss_kib":find("VmHWM:")?}))
}
fn validate_recipe(recipe: &Recipe) -> Result<()> {
    let e = &recipe.embedding;
    e.validate()?;
    ensure!(
        e.model == MODEL && hex_artifact(e) == ARTIFACT && e.dimension == 1024,
        "benchmark requires the frozen 1024-dimensional model artifact"
    );
    ensure!(
        e.gpu_layers == 0 && e.threads == 2 && e.context_tokens == 8192,
        "benchmark requires num_gpu=0, num_thread=2, num_ctx=8192"
    );
    ensure!(
        (1..=3600).contains(&recipe.deadline_seconds),
        "deadline must be 1..3600s"
    );
    ensure!(
        (128..=16384).contains(&recipe.chunk_bytes),
        "invalid chunk byte limit"
    );
    recipe.batch.validate(recipe.chunk_bytes)?;
    metrics::validated_index_path(&recipe.source_root, &recipe.index)?;
    Ok(())
}
fn hex_artifact(e: &embedding::Config) -> String {
    e.artifact_sha256
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn source_options(work: &Workload, chunk_bytes: usize) -> extract::Options {
    extract::Options {
        paths: work.files.keys().cloned().collect(),
        chunk_bytes,
        max_source_bytes: 8 * 1024 * 1024,
        allow_syntax_errors: false,
    }
}
fn rust_paths(
    root: &Path,
    directory: &Path,
    output: &mut Vec<String>,
    deadline: Instant,
) -> Result<()> {
    check_deadline(deadline)?;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            rust_paths(root, &entry.path(), output, deadline)?;
        } else if kind.is_file() && entry.path().extension().is_some_and(|e| e == "rs") {
            ensure!(
                output.len() < 4096,
                "source discovery exceeds 4096 candidate files"
            );
            output.push(
                entry
                    .path()
                    .strip_prefix(root)?
                    .to_str()
                    .context("non-UTF8 path")?
                    .to_owned(),
            );
        }
    }
    Ok(())
}
fn sample_plan(
    root: &Path,
    work: &Workload,
    chunk_bytes: usize,
    deadline: Instant,
) -> Result<(extract::Plan, Value)> {
    let mut options = source_options(work, chunk_bytes);
    let mut current = extract::plan(root, &options)?;
    ensure!(
        current.rows <= 1024,
        "labeled files exceed 1024 rows; keep every label"
    );
    let mut candidates = Vec::new();
    rust_paths(
        root,
        &root.join("native/forge/src"),
        &mut candidates,
        deadline,
    )?;
    candidates.sort();
    let mut skipped = Vec::new();
    let mut added = 0;
    for path in candidates
        .into_iter()
        .filter(|p| !work.files.contains_key(p))
    {
        check_deadline(deadline)?;
        if added == 100 {
            break;
        }
        let one = extract::plan(
            root,
            &extract::Options {
                paths: vec![path.clone()],
                chunk_bytes,
                max_source_bytes: 8 * 1024 * 1024,
                allow_syntax_errors: false,
            },
        )?;
        if current.rows + one.rows > 1024
            || current.source_bytes + one.source_bytes > 8 * 1024 * 1024
        {
            skipped.push(json!({"path":path,"reason":"1024-row or 8-MiB source ceiling"}));
            continue;
        }
        options.paths.push(path);
        current.rows += one.rows;
        current.source_bytes += one.source_bytes;
        current.files.extend(one.files);
        added += 1;
    }
    // Replan once so row ordering is exactly the production planner's ordering.
    let plan = extract::plan(root, &options)?;
    Ok((
        plan,
        json!({"labeled_files":work.files.len(),"distractor_files":added,
        "requested_distractor_files":100,"skipped_due_to_ceiling":skipped,
        "selection":"sorted native/forge/src regular Rust files, first 100 that fit; selected before inference"}),
    ))
}
impl Bench {
    fn verify_sources(&self) -> Result<()> {
        for (path, expected) in &self.frozen_sources {
            ensure!(
                hash(&bounded(&self.root.join(path), 1024 * 1024)?) == *expected,
                "frozen source hash drift: {path}; do not drop or relabel a hard query"
            );
        }
        Ok(())
    }
    fn label_ranges(&self, intent: &Intent) -> Result<Vec<(String, usize, usize)>> {
        intent
            .relevant
            .iter()
            .map(|l| {
                let text = self
                    .sources
                    .get(&l.path)
                    .context("missing labeled source")?;
                let (a, b) = metrics::line_bytes(text, l.line_start, l.line_end)?;
                Ok((l.path.clone(), a, b))
            })
            .collect()
    }
    fn query(
        &self,
        reader: &mut Reader,
        intent: &Intent,
        vector: &[f32],
        mode: SearchMode,
        lexical: bool,
    ) -> Result<Value> {
        let start = Instant::now();
        let result = reader.search(
            &self.root,
            QueryRequest {
                text: intent.query.clone(),
                vector: (!lexical).then(|| vector.to_vec()),
                space: (!lexical).then(|| reader.manifest().space.clone()),
                mode,
                limit: K,
                candidates: 64,
                ef: 256,
            },
        )?;
        ensure!(
            result.stale_candidates == 0
                && result.unreadable_candidates == 0
                && !result.freshness_budget_exhausted
                && !result.output_truncated,
            "incomplete or stale query {}",
            intent.id
        );
        let ranges = self.label_ranges(intent)?;
        let mut path_hit = false;
        let mut span_hit = false;
        let labels_recovered: Vec<_> = ranges
            .iter()
            .map(|l| {
                result.hits.iter().any(|h| {
                    metrics::overlaps((&h.path, h.start_byte, h.end_byte), (&l.0, l.1, l.2))
                })
            })
            .collect();
        for hit in &result.hits {
            path_hit |= ranges.iter().any(|l| l.0 == hit.path);
            span_hit |= ranges.iter().any(|l| {
                metrics::overlaps((&hit.path, hit.start_byte, hit.end_byte), (&l.0, l.1, l.2))
            });
        }
        let bytes = serde_json::to_vec(&result)?;
        Ok(
            json!({"id":intent.id,"latency_us":start.elapsed().as_secs_f64()*1e6,
            "path_hit_at_10":path_hit,"span_hit_at_10":span_hit,
            "labels_recovered":labels_recovered,"all_label_spans_hit_at_10":labels_recovered.iter().all(|x| *x),
            "source_bytes_read":result.source_bytes_read,"distance_evaluations":result.distance_evaluations,
            "json_bytes":bytes.len(),"result":result}),
        )
    }
}
fn coverage(bench: &Bench, plan: &extract::Plan) -> Result<Value> {
    ensure!(
        (1..=1024).contains(&plan.rows),
        "planned row budget must be 1..1024"
    );
    for intent in &bench.workload.queries {
        for (path, a, b) in bench.label_ranges(intent)? {
            let file = plan
                .files
                .iter()
                .find(|f| f.path == path)
                .context("label outside source plan")?;
            ensure!(
                file.spans.iter().any(|s| s[0] < b && a < s[1]),
                "label absent from indexed chunks: {} {path}",
                intent.id
            );
        }
    }
    Ok(
        json!({"files":plan.files,"rows":plan.rows,"source_bytes":plan.source_bytes,
        "scope":"frozen labeled files plus deterministic distractors; not full repository coverage"}),
    )
}
fn vector_recall(bench: &Bench, vectors: &[Vec<f32>], reader: &Reader) -> Result<Vec<Value>> {
    let manifest = reader.manifest();
    let snapshot = Snapshot::open(
        &bench
            .store
            .root()
            .join(&manifest.generation)
            .join("vectors.bin"),
    )?;
    ensure!(
        snapshot.space() == &manifest.space
            && snapshot.source_sha256() == manifest.source_set_sha256,
        "vector reference identity drift"
    );
    let mut workspace = QueryWorkspace::new(snapshot.len(), 256)?;
    let mut results = Vec::new();
    for (intent, vector) in bench.workload.queries.iter().zip(vectors) {
        check_deadline(bench.deadline)?;
        let query = Query::new(manifest.space.clone(), vector.clone())?;
        let exact: Vec<_> = snapshot
            .exact(&query, K.min(snapshot.len()), &mut workspace)?
            .hits
            .iter()
            .map(|h| h.row)
            .collect();
        let approximate: Vec<_> = snapshot
            .search(&query, K.min(snapshot.len()), 256, &mut workspace)?
            .hits
            .iter()
            .map(|h| h.row)
            .collect();
        let within_budget = Instant::now() < bench.deadline;
        let record = json!({"id":intent.id,"recall_at_10_against_exact":metrics::recall(&exact,&approximate)?,
            "budget_status":if within_budget {"within_deadline"} else {"censored_overrun"},
            "exact_rows":exact,"hnsw_rows":approximate});
        eprintln!("{}", json!({"event":"raw_vector_comparison","data":record}));
        results.push(record);
        ensure!(
            within_budget,
            "raw vector comparison censored: deadline overrun"
        );
    }
    Ok(results)
}
fn observe(
    bench: &Bench,
    reader: Option<&mut Reader>,
    intent: &Intent,
    vector: &[f32],
    mode: SearchMode,
    lexical: bool,
) -> Result<Value> {
    let admitted_at = Instant::now();
    if admitted_at >= bench.deadline {
        return Ok(
            json!({"id":intent.id,"status":"unstarted","observation_success":null,
            "budget_status":"not_admitted","reason":"deadline expired before retrieval admission"}),
        );
    }
    let reader = reader.context("reader absent for admitted retrieval")?;
    let observation = bench
        .query(reader, intent, vector, mode, lexical)
        .and_then(|value| {
            ensure!(
                value["latency_us"]
                    .as_f64()
                    .is_some_and(|t| t.is_finite() && t >= 0.0),
                "successful retrieval omitted valid latency"
            );
            Ok(value)
        });
    let within_budget = Instant::now() < bench.deadline;
    let success = observation.is_ok();
    let outcome = metrics::Outcome::classify(true, Some(success), within_budget)?;
    match outcome {
        metrics::Outcome::Completed => {
            COMPLETED.fetch_add(1, Relaxed);
        }
        metrics::Outcome::Failed => {
            FAILED.fetch_add(1, Relaxed);
        }
        metrics::Outcome::Censored => {
            CENSORED.fetch_add(1, Relaxed);
        }
        metrics::Outcome::Unstarted => unreachable!("admitted observation"),
    }
    let mut row = match observation {
        Ok(value) => value,
        Err(error) => json!({"id":intent.id,"error":format!("{error:#}"),
            "observed_elapsed_us":admitted_at.elapsed().as_secs_f64()*1e6}),
    };
    row["status"] = json!(outcome.as_str());
    row["observation_success"] = json!(success);
    row["budget_status"] = json!(if within_budget {
        "within_deadline"
    } else {
        "overrun"
    });
    Ok(row)
}
fn totals() -> Value {
    let completed = COMPLETED.load(Relaxed);
    let failed = FAILED.load(Relaxed);
    let censored = CENSORED.load(Relaxed);
    json!({"requested":144,"completed":completed,"failed":failed,"censored":censored,
        "unstarted":144-completed-failed-censored})
}
fn measure(bench: &Bench, vectors: &[Vec<f32>], mode: SearchMode, lexical: bool) -> Result<Value> {
    let open_start = Instant::now();
    let mut reader = if Instant::now() < bench.deadline {
        Some(Reader::open(&bench.store)?)
    } else {
        None
    };
    let open_us = open_start.elapsed().as_secs_f64() * 1e6;
    let mut rows = Vec::new();
    // Pass zero: first use of a newly opened Reader; pass one: same Reader warm.
    for pass in 0..2 {
        for (intent, vector) in bench.workload.queries.iter().zip(vectors) {
            let row = observe(bench, reader.as_mut(), intent, vector, mode, lexical)?;
            let event = json!({"pass":pass,"measurement":row});
            eprintln!("{}", json!({"event":"retrieval_query","data":event}));
            rows.push(event);
        }
    }
    let summaries: Vec<_> = (0..2)
        .map(|pass| {
            let selected: Vec<_> = rows
                .iter()
                .filter(|r| r["pass"] == pass)
                .map(|r| &r["measurement"])
                .collect();
            let cross: Vec<_> = selected
                .iter()
                .copied()
                .filter(|r| r["id"].as_str().is_some_and(|id| id >= "q17"))
                .collect();
            json!({"pass":pass,"all":metrics::summarize(&selected,24),
            "cross_module_q17_q24":metrics::summarize(&cross,8)})
        })
        .collect();
    Ok(json!({"mode":if lexical {"bm25"} else {match mode {
        SearchMode::Exact => "hybrid_exact", SearchMode::Hnsw => "hybrid_hnsw"}},
        "reader_open_us":open_us,"counts":metrics::summarize(&rows.iter().map(|r| &r["measurement"]).collect::<Vec<_>>(),48),
        "requested":48,"summaries":summaries,"measurements":rows,"process_memory":rss()?}))
}
fn prepare(
    recipe: &Recipe,
    deadline: Instant,
) -> Result<(Bench, Value, Value, Vec<u8>, extract::Options)> {
    let work_text = include_str!("real_code_index/workload.json");
    let workload: Workload = serde_json::from_str(work_text)?;
    workload.validate()?;
    let sources = workload
        .files
        .keys()
        .map(|p| {
            Ok((
                p.clone(),
                String::from_utf8(bounded(&recipe.source_root.join(p), 1024 * 1024)?)?,
            ))
        })
        .collect::<Result<_>>()?;
    let checked_index = metrics::validated_index_path(&recipe.source_root, &recipe.index)?;
    let store = Store::create(&checked_index)?;
    ensure!(
        store.root().canonicalize()? == checked_index
            && !checked_index.starts_with(recipe.source_root.canonicalize()?),
        "index path changed at creation"
    );
    let mut bench = Bench {
        root: recipe.source_root.canonicalize()?,
        store,
        workload,
        sources,
        deadline,
        frozen_sources: BTreeMap::new(),
    };
    bench.frozen_sources = bench.workload.files.clone();
    bench.verify_sources()?;
    let (plan, selection) =
        sample_plan(&bench.root, &bench.workload, recipe.chunk_bytes, deadline)?;
    let coverage = coverage(&bench, &plan)?;
    bench.frozen_sources = plan
        .files
        .iter()
        .map(|f| (f.path.clone(), f.source_sha256.clone()))
        .collect();
    bench.verify_sources()?;
    let snapshot = serde_json::to_vec_pretty(&json!({"coverage":coverage,"selection":selection,
        "sources":bench.frozen_sources,"source_revision":source_revision(&bench.root)?,"workload_sha256":hash(work_text.as_bytes())}))?;
    // Persist the full pre-inference manifest; report binds the exact bytes.
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(bench.store.root().join("benchmark-source-manifest.json"))?
        .write_all(&snapshot)?;
    let options = extract::Options {
        paths: bench.frozen_sources.keys().cloned().collect(),
        chunk_bytes: recipe.chunk_bytes,
        max_source_bytes: 8 * 1024 * 1024,
        allow_syntax_errors: false,
    };
    Ok((bench, coverage, selection, snapshot, options))
}
fn refresh_index(
    bench: &Bench,
    recipe: &Recipe,
    options: extract::Options,
    deadline: Instant,
) -> Result<extract::RefreshReport> {
    extract::refresh_with_progress(
        &bench.store,
        &bench.root,
        extract::Refresh {
            sources: options,
            force_reembed: true,
            batch: recipe.batch,
            embedding: recipe.embedding.clone(),
            graph: Default::default(),
            deadline_seconds: recipe
                .deadline_seconds
                .min(deadline.saturating_duration_since(Instant::now()).as_secs()),
        },
        |progress| {
            let mut journal = std::io::stderr().lock();
            serde_json::to_writer(
                &mut journal,
                &json!({"event":"refresh_progress", "preparation":progress}),
            )?;
            writeln!(journal)?;
            Ok(())
        },
    )
}
fn run(recipe: Recipe, recipe_hash: &str, execute: bool) -> Result<bool> {
    validate_recipe(&recipe)?;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(recipe.deadline_seconds);
    let work_text = include_str!("real_code_index/workload.json");
    let (bench, coverage, selection, snapshot, options) = prepare(&recipe, deadline)?;
    check_deadline(deadline)?;
    if !execute {
        println!(
            "{}",
            json!({"status":"planned","measurement_performed":false,
            "recipe_sha256":recipe_hash,"workload_sha256":hash(work_text.as_bytes()),
            "benchmark_binary_sha256":executable_hash()?,"coverage":coverage,
            "selection":selection,"index":bench.store.root(),"batch_policy":recipe.batch,
            "frozen_source_manifest_sha256":hash(&snapshot),"requested_queries":24,
            "requested_retrievals":144,"counts":totals()})
        );
        return Ok(true);
    }
    let client = embedding::Client::new(recipe.embedding.clone())?;
    let refresh_start = Instant::now();
    let refresh = refresh_index(&bench, &recipe, options, deadline)?;
    let refresh_ms = refresh_start.elapsed().as_secs_f64() * 1e3;
    ensure!(
        Instant::now() < deadline,
        "index preparation censored: deadline overrun"
    );
    bench.verify_sources()?;
    check_deadline(deadline)?;
    let query_start = Instant::now();
    let texts: Vec<_> = bench
        .workload
        .queries
        .iter()
        .map(|q| q.query.as_str())
        .collect();
    let (batch, query_batches) =
        query_embedding::queries(&client, &texts, recipe.batch, deadline, |progress| {
            let mut journal = std::io::stderr().lock();
            serde_json::to_writer(
                &mut journal,
                &json!({"event":"query_preparation_progress", "preparation":progress}),
            )?;
            writeln!(journal)?;
            Ok(())
        })?;
    ensure!(batch.vectors.len() == 24, "missing query embeddings");
    let embedding_ms = query_start.elapsed().as_secs_f64() * 1e3;
    ensure!(
        Instant::now() < deadline,
        "query embedding preparation censored: deadline overrun"
    );
    let reader = Reader::open(&bench.store)?;
    let recall = vector_recall(&bench, &batch.vectors, &reader)?;
    let measurements = vec![
        measure(&bench, &batch.vectors, SearchMode::Exact, true)?,
        measure(&bench, &batch.vectors, SearchMode::Exact, false)?,
        measure(&bench, &batch.vectors, SearchMode::Hnsw, false)?,
    ];
    bench.verify_sources()?;
    println!(
        "{}",
        json!({"workload":"frozen_real_code_retrieval_v1","workload_sha256":hash(work_text.as_bytes()),
        "label_revision":bench.workload.revision,"recipe_sha256":recipe_hash,"embedding_recipe":recipe.embedding,
        "benchmark_binary_sha256":executable_hash()?,"batch_policy":recipe.batch,
        "coverage":coverage,"selection":selection,"frozen_source_manifest_sha256":hash(&snapshot),"refresh":refresh,"refresh_ms":refresh_ms,"query_embedding_ms":embedding_ms,
        "query_embedding_batches":query_batches,"query_input_tokens":batch.input_tokens_total,"queries":24,"top_k":K,"candidates":64,"ef":256,
        "vector_hnsw_recall":recall,"measurements":measurements,"counts":totals(),
        "overall_budget_status":if Instant::now()<deadline {"within_deadline"} else {"censored_overrun"},
        "process_memory":rss()?,"elapsed_ms":started.elapsed().as_secs_f64()*1e3,
        "latency_scope":"first Reader use then warm reuse; OS/model cache uncontrolled; process RSS excludes model server",
        "evidence_scope":"24 targeted queries, 8 cross-module; agent-authored, source-inspected labels frozen before retrieval (Luna); deterministic path/span scoring, no model judges retrieved results; not independent human ground truth or broad relevance proof; label overlap distinct from raw HNSW recall; no improvement claim"})
    );
    Ok(COMPLETED.load(Relaxed) == 144 && Instant::now() < deadline)
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    if args.is_empty() {
        eprintln!("real_code_index skipped: opt in with --run /absolute/recipe.json");
        return Ok(());
    }
    ensure!(
        args.len() == 2 && (args[0] == "--run" || args[0] == "--plan"),
        "opt-in: real_code_index --plan|--run /absolute/recipe.json"
    );
    let raw = bounded(Path::new(&args[1]), 65536)?;
    let recipe: Recipe = serde_json::from_slice(&raw)?;
    let batch_policy = recipe.batch;
    match run(recipe, &hash(&raw), args[0] == "--run") {
        Ok(true) => Ok(()),
        Ok(false) => {
            anyhow::bail!("benchmark incomplete, censored, failed, or outside its deadline")
        }
        Err(error) => {
            eprintln!(
                "{}",
                json!({"status":"failed","requested_queries":24,"requested_retrievals":144,
                "counts":totals(),"batch_policy":batch_policy,"error":format!("{error:#}")})
            );
            Err(error)
        }
    }
}
