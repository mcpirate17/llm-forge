# Bounded real-code retrieval benchmark

`real_code_index` is an explicitly opted-in native Rust benchmark using the production
source planner, embedding Client, refresh/build, code-index Reader and vector Snapshot.
The existing `code_index` benchmark remains a synthetic pipeline microbenchmark.
Running this target without arguments skips it without contacting any service.
Use `--plan /absolute/recipe.json` to validate the frozen source scope and persist
its manifest without contacting the embedding service or measuring retrieval.
Planning consumes its own fresh index directory; use a different unused index
path in the execution recipe. A planned report explicitly says no measurement
was performed and leaves all requested retrievals unstarted.

The targeted fixture contains 24 queries (q17–q24 cross module) with agent-authored,
source-inspected labels frozen before retrieval. Luna authored the labels from source
inspection before embeddings or rankings were examined; no human authored or
adjudicated them. Each labeled file has a SHA-256 binding. The current prospective
fixture revision is `674d258ad2dc9653b883a2923f40932178a49798+embedding-batch-controls-v2`:
the commit identifies its base; the file hashes identify the exact reviewed source.
The initial attempt timed out during embedding with all 144 retrievals unstarted.
Before any rankings existed, v2 rebound only `code_index/extract.rs` and q11's
line ranges to the same planning and refresh implementations, including the new
progress helper. Every query text, ID, path, symbol and rationale is unchanged.
The original fixture remains reproducible from commit `674d258`.
Never change a fixture in response to its rankings. Hash drift fails the run;
future source changes require a separate, source-reviewed prospective revision
before measuring that revision. Preserve each prior fixture and run record.
Scoring is deterministic path/span overlap; no model judges retrieved results.

All labeled files are indexed, followed by the first 100 additional regular Rust files
from `native/forge/src`, in sorted path order, that fit the remaining budget. Selection
occurs before inference. The complete source scope must fit 1024 chunks and 8 MiB;
files that exceed the remaining budget are reported, and subsequent candidates are
considered. No labeled file or query is dropped. If fewer than 100 distractors fit,
report the actual count, skipped files and limit; do not describe it as broad coverage.
The full planned paths, spans and source hashes are persisted to
`INDEX/benchmark-source-manifest.json` before model calls. The manifest must not already
exist, so each run uses a new private index directory. All selected source hashes are
rechecked after refresh and after queries. The final report binds this manifest,
workload bytes, operator recipe, executable, embedding recipe and index components.

## Operator recipe

First finish any shared-checkout validation gate. The benchmark does not start or pull
an inference service. Independently verify the local inventory before running. The
fixture pins `qwen3-embed-cpu:latest`, 1024 dimensions, artifact
`fdf7067d896175e871d925acacde4df653a1e6900a946710342ce972372dcf81`.
The native client verifies that model identity before embedding. Runtime options are
`num_gpu=0`, `num_thread=2`, `num_ctx=8192`, with a
60-second request deadline. The operator recipe below selects at most 2 inputs
and 8192 UTF-8 input bytes per refresh or query-preparation batch for this CPU preparation attempt.
This is a batch-size choice, not a measured throughput claim. Omitted `batch`
retains the existing 32-input/65536-byte defaults. `max_inputs` must be 1..32;
`max_bytes` must be at least `chunk_bytes` and at most 65536. Invalid limits
fail before HTTP, including a byte budget unable to admit one full chunk.
Effective limits are recorded in planned/final/failure reports and refresh evidence.
The same policy batches all 24 query embeddings in their original order; query
batch count and summed input tokens are reported separately from refresh work.
Each completed refresh or query batch also writes a JSON `refresh_progress` or
`query_preparation_progress` event to the stderr journal, with cumulative
completed batches, rows and input tokens. These
are preparation observations, not retrieval successes or in-flight token costs.
A later failure retains prior events; it does not imply index publication. Source chunks are at most 4096 bytes in this recipe.
The benchmark's total deadline defaults to 600 seconds (explicit maximum 3600 seconds); an outer process timeout bounds
planning, filesystem calls and process shutdown too.

Create the parent state directory and save this exact JSON as an absolute recipe path.
Choose an unused index path for every attempt, including failed attempts:

```json
{
  "source_root": "/home/tim/Projects/llm-forge",
  "index": "/home/tim/.local/state/llm-forge/native-migration/real-code-index-run01",
  "embedding": {
    "endpoint": "http://127.0.0.1:11434",
    "model": "qwen3-embed-cpu:latest",
    "runner": "",
    "artifact_sha256": "fdf7067d896175e871d925acacde4df653a1e6900a946710342ce972372dcf81",
    "dimension": 1024,
    "context_tokens": 8192,
    "threads": 2,
    "gpu_layers": 0,
    "timeout_seconds": 60
  },
  "deadline_seconds": 600,
  "chunk_bytes": 4096,
  "batch": {"max_inputs": 2, "max_bytes": 8192}
}
```

Build and run only when the checkout owner releases its gate:

```sh
cargo test --manifest-path native/forge/Cargo.toml --test real_code_index_metrics --locked --offline
cargo bench --manifest-path native/forge/Cargo.toml --bench real_code_index --no-run --locked --offline
# Parent state directory must already exist. Save recipe outside the checkout.
timeout --signal=TERM --kill-after=10s 630s cargo bench --manifest-path native/forge/Cargo.toml --bench real_code_index --locked --offline -- --run /absolute/recipe.json > /absolute/report.json 2> /absolute/query-events.jsonl
```

Model/server threads and RSS are outside the benchmark process. Record server RSS and
host load separately if comparing costs. Cargo diagnostics share stderr with the query
journal; consume only valid JSON lines for query events. An outer timeout may prevent a
final JSON report; retain its exit status and journal rather than inferring completion.
All 144 requested retrievals partition into completed, executed failed, unstarted,
and censored. A deadline refusal before retrieval admission is unstarted. An admitted
operation finishing after the deadline is censored, with observation success and
budget status recorded separately; its ranking remains inspectable. Censored rows
are excluded from within-budget success/latency summaries and remain in the full
requested relevance denominator. Missing or invalid latency never counts as success.
Any failure, unstarted retrieval, censoring or overall deadline overrun gives a
nonzero exit. Fatal errors preserve the same four totals; source drift invalidates the
run even if all queries finished. The benchmark records outcomes and makes no automatic
promotion or improvement decision.

## Reading the measurements

The report contains every ranking, hit path and byte span, generation, source read
cost and distance evaluation count. For each of BM25, hybrid exact and hybrid HNSW,
24 queries run twice: first use of a newly opened Reader, then warm reuse. Each pass
reports path hit@10, span-overlap hit@10 and all-labeled-spans hit@10, including a
separate q17–q24 cross-module summary. A hit is any returned span overlapping a labeled
inclusive line range on the same path. These agent-authored, source-inspected labels
were frozen before retrieval. This is a targeted agent-labeled fixture, not independent
human ground truth, exhaustive relevance judgments or broad relevance proof.

Raw vector HNSW recall@10 compares unique returned row IDs with the exact cosine
reference. It is separate from relevance and from the hybrid ranker's output. Both
use the same pinned embedding space, graph and source snapshot. Lower approximate
recall cannot establish worse semantic quality; inspect the separate deterministic
path/span scores against the agent-authored labels, with their stated limitations.

Reader open time, first-use versus warm query samples, successful-query latency
p50/p95, refresh time, query-embedding time, source bytes and Linux process RSS/HWM
are separate. First use means a new Reader; OS page cache and model warmth are
uncontrolled, and the raw-vector comparison runs first. Do not call these cold-host
latencies. Query timings include production search, source verification, relevance
accounting and JSON serialization; model embedding time is reported separately.
No faster/better claim is justified until these numbers have actually been measured.

The model-free integration test checks known-vector exact order and approximate recall.
Metric contracts cover duplicate recovery, empty references, byte/line boundaries,
path-sensitive span overlap, missing frozen labels, and failed-query denominators.

## Prospective source re-freeze

If source changes before the first measurement, preserve this original fixture and
its SHA. Before viewing any ranking, inspect the changed source, retain all 24 query
intents and cross-module cases, review whether line spans still express their stated
rationales, and produce a separately versioned fixture with the new source revision,
file hashes and a provenance note explaining each span/hash change. Bind that new
fixture's SHA in its report. A hash failure is not authorization to update fixture
hashes mechanically. Never re-freeze after observing rankings or silently replace the
original fixture. The draft currently binds the original supplied revision.

Operator input reads reuse `forge::bounded_file::read`: final symlinks and nonregular
files are refused, and allocation/read length is bounded even if a file grows. Index
paths must be normalized absolute paths with an existing parent; the canonical parent
and created directory are checked against the canonical source root. Use the dedicated
private state directory in the recipe, including for every failed attempt.
