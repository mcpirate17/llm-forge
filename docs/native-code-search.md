# Native code search

`forge code-index plan|refresh|build|query|status` combines source-bound embeddings, native
HNSW/exact cosine search, and SQLite FTS5 BM25. Indexes live in an explicit owned
0700 directory outside the source root. This CLI and library work with an empty
`PATH`. Native source extraction and an operator-configured embedding client are
available, including bounded persistent harness discovery and named index access.

Build input is JSONL: one header followed by exactly `rows` embedding records.
Rows must be unique and sorted by `(path, start_byte, end_byte)`. A zero-row build
publishes an empty searchable generation while preserving existing reader leases.
Each row contains
`path`, `source_sha256`, `start_byte`, `end_byte`, and a `vector` array. Paths are
relative to the source root, hashes are canonical lowercase SHA-256, and spans
are UTF-8 boundaries of at most 16 KiB. The index reads actual source text through
the existing directory capability; caller-provided text is never indexed.

```json
{"version":1,"rows":1,"space":{"dimension":2,"model_sha256":"1111111111111111111111111111111111111111111111111111111111111111"}}
```

The hash above is a format example, not a real model identity. An optional
`config` selects HNSW neighbors, construction ef and seed; defaults are 16, 200,
and the fixed native seed. Model identity must match the actual embedding recipe.
The importer verifies source bindings and dimensions, not whether a model truly
produced the submitted numbers. Use genuine learned embeddings for semantic
retrieval; synthetic vectors are only correctness fixtures.

```sh
forge code-index build --index /home/user/.local/state/forge/code-index \
  --source-root /home/user/project --input /home/user/embeddings.jsonl
forge code-index query --index /home/user/.local/state/forge/code-index \
  --source-root /home/user/project --request /home/user/query.json
forge code-index status --index /home/user/.local/state/forge/code-index
```

The query contains `text`, `vector`/`space` together (or both null for BM25 only),
`mode` (`exact`, the default, or `hnsw`), `limit` (1..100), `candidates` and `ef`.
Require `limit <= candidates <= ef <= 4096`. Text is bounded to 4 KiB and 64
literal terms; FTS operators are quoted, never executed as query syntax. BM25
candidates and vector candidates are united, reranked by exact cosine, then
combined by reciprocal ranks with constant 60. Original row ordinals break ties.
The result includes cosine and lexical rank, so fusion is inspectable.

```json
{"text":"persistent terminal","vector":null,"space":null,"mode":"exact","limit":10,"candidates":100,"ef":160}
```

Every returned span is read from current source and must match its indexed hash.
Changed and unreadable candidates are excluded, with counts and up to four bounded
read-error details. New files are absent until refresh. The index is an explicitly
identified snapshot, not a claim of complete current-workspace coverage. Reads
are bounded to a 64 MiB charged budget per query (a failed read charges the
maximum source size). Responses stop below 1 MiB, including JSON escaping.
Budget exhaustion and output truncation are explicit flags; fewer than `limit`
hits can also reflect candidate limits or stale files. Source hashes describe
read-time state and do not lock out subsequent edits.

Publication writes and syncs a new immutable generation before atomically replacing
the `current` pointer. Builders use a separate exclusive lock; readers hold a
generation lease, so refresh never modifies their mapped vectors or SQLite file.
Collection keeps current and previous generations plus any active leases. Busy
publication/build locks return an explicit retryable error. Failed unpublished
builds remove only their newly created staging directory and preserve `current`.
Unexpected files in retired generations stop collection. Files left by a crash
are inspected on later publication; unknown contents are not silently removed.

Opening verifies component hashes, vector graph structure and row/model/source-set
identities. The owner must not modify sealed inodes or rewrite manifests; checksums
detect damage, not malicious same-user modifications. SQLite stores at most 4 GiB,
and the vector limits are described in [native vector index](native-vector-index.md).
Approximate recall depends on the corpus: that report includes both a favorable
clustered case and a high-dimensional case where exact scanning wins.

Nine integration tests cover BM25/vector fusion, source changes/deletion, empty corpora,
failed-build rollback, concurrent refresh, lease-aware collection, checksums,
empty-PATH CLI use, framing bounds, escaped output and failed-read budgets.

## Native embedding client

`forge::embedding::Client` uses the existing native HTTP dependency and the
[Ollama embedding API](https://docs.ollama.com/api/embed). Operator configuration
pins a literal loopback/private IP and port, explicit `model:tag`, optional runner,
artifact SHA-256, dimension, context, thread count, GPU layers and deadline.
GPU layers default to zero. No endpoint or model identifier comes from a tool's
model-generated arguments. The client does not pull models or launch services.

Before and after inference, it checks the matching installed model/runner in the
[model inventory](https://docs.ollama.com/api/tags), including digest, nonzero size
and absence of remote-backend fields. It disables environment proxies and
redirects. Each batch has one shared deadline, at most 32 inputs, at most 16 KiB
per input and 64 KiB in total. Truncation is explicitly disabled. Responses must
match model, batch order/count and dimensions, report bounded token usage and
contain finite nonzero vectors. Batched token counts describe the whole batch.

The `Space.model_sha256` value produced by this client is the fingerprint of the
embedding recipe, including artifact digest and numerical/runtime settings. It is
distinct from `Config.artifact_sha256`. Changing context, runner, threads or GPU
layers changes the space; endpoint and deadline do not. Source indexing and query
embedding must use that same space. This prevents accidental configuration mixing;
it is not cryptographic attestation of the independently operated server. Server
resource ownership remains external, and an HTTP timeout does not prove server-side
inference has stopped. The client requests a bounded 60-second keep-alive.

Four local HTTP fixture tests pass, including failed pre/post identity checks,
remote models, wrong dimensions, zero vectors, body limits, redirect rejection,
whole-call timeouts and batch ordering. These tests launch no actual model or GPU.

## Source planning and refresh

`code-index plan --source-root ROOT --options OPTIONS.json` reads an explicit
path list and reports file/chunk counts, total source bytes and syntax-error
previews. It makes no model requests and does not create an index. Options require
`paths`, `chunk_bytes` (128..16384) and `max_source_bytes` (at most 1 GiB).
Duplicate, control-directory, traversal and non-normalized paths fail. The library
plan also exposes every source hash and byte span.

Chunks prefer native AST symbol boundaries, then line boundaries, then complete
UTF-8 characters. Concatenating all spans exactly reconstructs each file. Long
functions may span chunks; this operation does not rewrite or fold source. For
recognized languages, syntax errors fail by default. Explicit
`allow_syntax_errors: true` permits line splitting and retains diagnostics in the
plan and refresh report. Unknown text extensions use line splitting. This stage
uses byte limits; model-specific BPE sizing remains part of the context milestone.
The embedding request uses `truncate: false`, so a context overflow is an error.

## Persistent harness search

Add named `code_indexes` entries to the operator harness policy, for example:

```json
{"workspace":{"directory":"/private/index","max_rows":50000,
 "max_mapped_mib":128,"readers":2,"embedding":null}}
```

Each directory must be owned, mode 0700 and disjoint from source, harness state,
and all terminal read/executable capabilities. Admission reserves the complete
mapping, query workspace and SQLite cache for each reader, including readers
pinning different generations. There are at most eight indexes, four readers
per index and sixteen readers overall. The aggregate memory limit must cover
these reservations in addition to the supervisor and terminal reservations.

`code_search` selects an operator index name, text, kind (`lexical`, `hybrid`,
`semantic`), mode (`exact`, `hnsw`), limit, candidates and ef. Lexical search
requires no provider. Hybrid/semantic search uses only the optional pinned
embedding configuration in the policy. Cross-field and byte bounds are checked
before provider calls. The default kind is hybrid when a provider exists and
lexical otherwise; exact vector search remains the default mode.

Bounded native reader pools check the current generation on each checkout.
Publication can overlap an in-flight request; the response identifies its pinned
generation. A refreshed index exceeding row/mapping limits fails explicitly.
Responses include provider usage and space identity. Empty indexes skip embedding.
The live fixture tests cover concurrent calls, generation refresh, admission,
provider pinning and rejection before HTTP; no real-model relevance is inferred.

`code-index refresh --index INDEX --source-root ROOT --config REFRESH.json`
combines `sources` (the options above), `embedding` (the pinned client config),
optional `graph` settings, and `deadline_seconds` (1..3600). Planning finishes
before requests begin. Batched embeddings preserve sorted path/span order, and
source hashes are checked again during import and before publication. A report
contains the published manifest, source bytes, complete bounded syntax diagnostics,
batch count and aggregate input-token usage. Empty plans publish empty indexes
without contacting the model service.

The deadline is enforced across HTTP operations and checked between source files,
database rows, normalization and HNSW insertion, and before publication. Filesystem
syscalls and an individual native/SQLite operation are not forcibly interrupted;
the deadline is cooperative outside HTTP. Index publication remains atomic when
a check fails. Six source-refresh tests cover exact Unicode spans, explicit syntax
fallback, sorted batching across count/byte boundaries, concurrent edits, provider
failures, construction cancellation, empty refresh and empty-PATH CLI use.


## Full retrieval pipeline measurement

`cargo +1.98.0 bench --locked --manifest-path native/forge/Cargo.toml --bench
code_index` builds 4,096 synthetic Rust-file chunks, 64-dimensional clustered
vectors and 64 perturbed queries. Each query requests ten hits from 64 candidates;
HNSW uses `ef=256`. The measured operation includes BM25, cosine search, fusion,
current-file reads and SHA verification, and JSON serialization. It excludes
model inference, transport and process startup. All queries completed with ten
fresh hits and no output/freshness budget exhaustion.

Rust 1.98 release build, AMD Ryzen 9 9950X3D, shared Linux host and warm caches:

| Mode | Median | p95 | Mean distance evaluations |
| --- | ---: | ---: | ---: |
| BM25 only | 80.23 µs | 83.87 µs | 0 |
| Hybrid, exact vector scan | 118.89 µs | 121.01 µs | 4,191.9 |
| Hybrid, HNSW | 149.06 µs | 152.10 µs | 2,330.5 |

Each mode completed 64 queries with zero failures. HNSW's final top ten matched
the exact pipeline on all 64 synthetic queries, but took 25% longer at the median.
Exact remains the default. The reduction in distance evaluations did not offset
traversal overhead for this small corpus; this measurement does not imply that
HNSW is slower at larger sizes or that either mode retrieves relevant real code.

Fixture creation took 31.3 ms, index construction 274.0 ms, and the first verified
reader open 2.415 ms. Retrieval read about 1.04 KiB of current source and produced
about 3.7–3.9 KiB of JSON per query. Relevance and real embedding-service latency
remain unmeasured; no model or GPU was launched.

Source-set SHA-256:
`a4d0a57c8b7c7df1be80101b00186f9d72e07deeeeb5351ac74afa99b7b2c2a1`.
Benchmark executable SHA-256:
`fad7c023c5645e667079ff2d709fb7fe2c39f9e80b834291e755ca5645765c59`.
The benchmark uses private XDG state storage and removes only its exclusively
created fixture after readers have dropped their leases.
