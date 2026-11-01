# Native memory ingestion and session lifecycle

The native memory service builds immutable JSONL and mmap generations from an
explicit source catalog and a pinned local Ollama embedding configuration:

```sh
forge memory ingest --host /path/to/host --store /private/memory-store \
  --embedding /private/embedding.json
forge memory search --store /private/memory-store \
  --embedding /private/embedding.json --task 'compiler diagnostics' --top-k 8
forge memory inspect --store /private/memory-store
```

The store path must be absolute, its parent must exist, and the leaf is private
to the current user. The embedding configuration uses the same explicit endpoint,
model tag, artifact SHA-256, dimension, runner and numerical resource fields as
[native code search](native-code-search.md). Schema-3 memory requires
`context_tokens=2048` and `gpu_layers=0` or `99`. The operator must have provisioned
that exact artifact. Forge does not pull models or choose another route on failure.
The embedding service is external; ingestion and search orchestration are Rust.

Catalog selection honors the host's `memory_sources` setting, the conventional
`conductor/memory_sources.toml`, then the embedded small relative-only catalog.
An explicitly configured missing catalog is an error. `--catalog` overrides the
selection. Schema 1 supports index entries with `root`, `roots`, `absolute_root`
or `include_dirs`, filename glob/exclusion filters, and `heading` or `whole`
chunking. Root traversal, symlinks, unsupported types, invalid UTF-8 and malformed
catalogs are refused. Catalogs are capped at 1 MiB and 256 sources; scans at
100,000 files and one million examined entries.

The first build must select all sources. Subsequent `--source ID` arguments
update selected available sources and preserve the other available sources.
Removing an ID from the catalog removes its rows globally, including during a
partial update: catalog membership is separate from file selection. Reuse binds the exact file
SHA-256 and normalized catalog/chunker recipe. Changed embedding models or
numerical settings require selecting every source. `--full` forces re-embedding
of selected files. Missing roots that previously contributed selected rows fail
unless `--allow-missing-roots` explicitly permits removal. Missing unused roots
are reported. An empty result is refused without replacing the current index.

The native recipe includes the query instruction prefix and differs from legacy
broker fingerprints. Existing Python indexes are not silently adopted: build a
new native store. Native code search's HNSW/BM25
index is a separate surface; this memory service performs exact mapped scoring.

Publication validates and seals the JSONL, sidecar and manifest before atomically
changing `current`. Failed reads, embeddings or final source rechecks retain the
old generation. Builders serialize; pinned readers keep their generation during
refresh and collection. Collection retains current, previous and leased
generations, bounds its inventory, and refuses unexpected components. Private
owned directories are the trust boundary against another process of the same UID.

Inputs are bounded to 512 MiB per run, one million output rows and 1 MiB per row.
Embedding batches contain at most 32 chunks and 64 KiB. `--timeout-seconds` bounds
the ingestion deadline from 1 to 3,600 seconds (120 by default); individual HTTP
calls retain the configured finite deadline. The report distinguishes embedded,
reused, preserved and removed rows and reports embedding input tokens. Unchanged
runs reuse rows without HTTP requests or a new generation.

## Knowledge cards

```sh
forge cards build --host /path/to/host --notes /path/to/host/notes \
  --output /private/cards.json --embedding /private/embedding.json
forge cards search --index /private/cards.json \
  --embedding /private/embedding.json --task 'compiler diagnostics' --top-k 8
```

The builder embeds complete top-level `kb_*.md` and `KB-*.md` cards in stable
filename order. Without `--notes`, it resolves the host's configured notes root.
It requires an existing output parent, preflights the output and writer lock,
and checks the source inventory and bytes again before atomic publication.
Failed inference or changed inputs leave the previous index intact.

Card text is preserved whole. Cards over the embedding client's 16 KiB document
limit are refused before inference; they are not silently shortened. The index
uses schema 2, with at most 4,096 cards and a 16 MiB serialized limit. Native
card search and memory search use the same query instruction and recipe
fingerprint, so an offline task-bound query can retrieve both indexes when their
embedding configurations match. Online search refuses an index with a different
recipe. Existing legacy schema-2 indexes remain usable for explicit offline
recall with their own matching fingerprint, without adopting their vectors into
the native recipe.

The Python memory, card, session-state and session-policy adapters are retired.
Use `forge memory`, `forge cards`, `forge session state` and `forge session
preamble`. The unwired Python auto-index marker queue and recency/dedup utility
are deleted; indexing is explicit through ingestion or configured session close.
Their obsolete memory-specific equivalence replay substitutions are also removed.

## Session status and close

```sh
forge session append --host /path/to/host --owner agent-name \
  --title 'Native index ready' --body 'Validated the fixture index.' --json
forge session close --host /path/to/host --owner agent-name \
  --all-owner-claims --memory-embedding /private/embedding.json \
  --memory-store /private/memory-store --json
```

Append preserves existing log bytes, adds a bounded newest entry under a
cooperative lock and refreshes active state. Titles are limited to 120 characters;
bodies to 12 lines and 1,200 characters. Unsafe sinks and injected top-level
headings are refused. If the append succeeds but refresh fails, exit code 3
reports the durable partial result.

Close preflights the memory configuration and claim selection before appending
optional status. It then refreshes state, releases selected claims in one locked
transaction, refreshes again, and ingests memory. Use repeated `--claim-id`
arguments or `--all-owner-claims`; no selection releases no claims. Memory sync
requires explicit `--memory-embedding` and `--memory-store`; use
`--no-memory-index` to deliberately skip it. The Make wrapper accepts these
choices through `SESSION_CLOSE_ARGS`.

These stages are not one atomic transaction. A later error returns code 3 with
completed stages and publication uncertainty where relevant. It never reports a
failed memory sync as a completed close or rolls back already released claims.

## Workspace runtime evidence

```sh
forge workspace-runtime --root /path/to/host --output /private/runtime-receipt
forge workspace-runtime --evaluate-receipt /private/runtime-receipt/receipt.json
```

The native nine-cell matrix checks local state, provider/hook configuration,
controls, launcher availability, explicit recall and supplied graph evidence.
Model/launcher execution needs `--run-embedding`, `--run-launchers` or
`--run-clerk`. Missing probes stay `NOT_READY`. Offline evaluation is not proof
that those probes executed. A complete receipt requires exactly the recognized
nine required cells, valid metadata and statuses, and a consistent aggregate.
Receipts are local assertions, not cryptographically attested execution.

Clerk execution additionally requires a dedicated non-default loopback port,
server PID, explicit `nvidia-smi` path and `--assert-exclusive-clerk-server`.
The runtime checks ownership, PID start time, listener identity, claims and GPU
state before execution and identity again before cleanup. Exclusive use remains
operator-attested; it is not inferred from an empty model inventory. Launcher
calls use isolated fixtures with finite time/token budgets and redacted hashed
logs. No live model or GPU probe is required by the fixture test suite.

Reconciliation can replace graph evidence, re-read launcher logs or explicitly
rerun the clerk cell. It validates the receipt, holds a private writer lock and
retains exact previous receipt bytes with their SHA-256 before publication.
This is a new native format; legacy Python evidence directories are not rewritten
in place.
