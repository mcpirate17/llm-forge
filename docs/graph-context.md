# Native graph context

`forge graph index` builds a syntax-only Python/Rust index at `.forge/graph.db`.
CLI context, Python context and graph test selection share one native storage
adapter: they prefer that index and otherwise read `.code-review-graph/graph.db`
without modifying it. Context retrieval starts no interpreter, embedding model
or background service. Semantic similarity is not inferred from syntax edges.

```sh
forge graph --host /path/to/project index
forge graph --host /path/to/project context src/module.rs
forge graph --host /path/to/project context src/module.rs --symbol process --max-tokens 2048 --max-depth 2 --max-nodes 24
forge graph --host /path/to/project refs --text 'Check src/module.rs:42 before editing'
forge graph --host /path/to/project refs --body-file message.txt
```

Commands return JSON. `context` retrieves caller and callee relationships;
`refs` extracts repository file references and retrieves bounded supporting
context. Use the command's `--help` to inspect its input and output limits.
Both retrieval commands accept `--max-bytes` for the JSON output budget. `context`
also accepts `--max-tokens`: its `utf8-byte-upper-bound` estimate budgets every
serialized UTF-8 byte as a token. This conservative estimate is explicit; it is
not a measured count from a model tokenizer. It caps the final JSON, including
metadata, and preserves the requested source before relationships. The newline
printed after JSON is outside the budget. Reference scans
deduplicate paths before reading source, cap the number of files, and report
`input_truncated` when a body-file prefix is used. Omission counts ending in
`_at_least` are lower bounds; the tool does not scan the entire index to count
everything it leaves out.

An absent, incompatible or detectably stale graph is reported explicitly. Source
hash mismatches suppress indexed relationships; requested symbols are reparsed
from the current source so old line ranges cannot select unrelated text. Dirty
or deleted relationship endpoints are also suppressed. Each projection reads a
consistent database snapshot. Missing, malformed or conflicting content hashes
produce `unverified` status and withhold affected indexed relationships. Hash
evidence includes both absolute/relative path aliases and every matching row in
the available `files` and `nodes` tables; a valid first row cannot hide conflicting
evidence later. An absent file/symbol stays `unindexed`, and `indexed (no matching
symbols)` requires a matching file hash. An `ok` status verifies only the returned
rows' source hashes; it never proves exhaustive coverage or semantic correctness.
The compatibility relationship API labels additional textual matches
`calls (syntactic)`, keeping them distinct from indexed edges.

Source access uses pinned directory capabilities, rejects symlinks, traversal,
`.git` components and special files, and checks descriptor and current-path
identity after reading. Hash-only reads use a fixed 64 KiB buffer; text and its
hash come from the same bytes. Each source is at most 1 MiB, each projection
hashes at most 128 distinct files including its target, and CLI excerpts admit
at most 256 KiB. In-place edits, inode replacement and root replacement detected
during reads fail closed. Linux backslashes remain literal filename characters.

SQLite admission rejects symlinked ancestors, nonregular or multiply linked
database/sidecar files, and files over 4 GiB. Readers require ordinary graph
tables, use read-only/query-only connections, disable trusted schema, and bound
value size (4 MiB), columns (256), expression depth (128) and SQL bytes (64 KiB).
Nested operations share a two-second deadline and 20-million-VM-operation
budget; lock waits are bounded to 250 ms. These are SQLite execution limits,
not filesystem I/O deadlines. Normal read-only WAL access remains supported.

Context expansion is bounded by depth (0..3), nodes (1..100) and edges per
direction (1..50). Tests rank before other relationship peers when the schema
provides test classification. Output includes a graph generation and requested
source hash. Reissue an expansion with `--expected-generation` and
`--expected-source-hash` to reject changed snapshots. Relationship paths identify
the next source to retrieve; large connected components are never dumped whole.

Projections are cached in `.forge/context-cache` (128 entries, at most 128 KiB
per entry). Keys bind generation, source content, symbol and expansion bounds.
Every cached dependency's current hash is checked before a hit. This cache
represents indexed relationships; newly added callers require index refresh.
`cache_status` and omission counts make cache use and truncation observable.
Only validated results are cached. Stale, unverified, unavailable and unindexed
results remain misses. Expansion validates each projection before it admits
dependencies or follows edges, and checks at most 101 unique dependency hashes
before publication or a hit. Core validation may reread shared endpoints in
successive projections; these bounds do not promise a single read per file
across an entire expansion.

Cache reads and publication use no-follow directory capabilities and owned,
single-link regular files. New directories are private; staging files use
exclusive creation and unpredictable names. Publication renames and syncs the
new entry before pruning only validated hexadecimal cache filenames, with a
4,096-entry scan cap. Unrelated JSON, symlinks and special files are preserved.
Failures after rename explicitly report visible publication and incomplete
pruning or durability. Lock admission allows 51 attempts and at most 250 ms
total retry sleep; it does not bound filesystem I/O or `fsync` time.

Configured-host ancestors, the mount namespace and other processes with the
same UID remain operator trust boundaries. Identity checks detect many races;
they do not provide a custom SQLite VFS or an OS sandbox against a same-UID
writer acting after the final check.

Index refresh reuses syntax facts only when content hashes and parser version
match. Ambiguity-aware module lookup maps avoid scanning every file at every
call. Unchanged refreshes preserve the exact database snapshot and generation;
edits and deletions publish a complete replacement atomically after successful
parsing. Dependency coverage is stored per file; metadata contains aggregate
counts and hashes rather than an unbounded unresolved-path manifest. Changed
refresh currently rewrites the bounded snapshot and resolves
its calls; it does not claim incremental edge updates. Reports expose parsed and
reused file counts, unresolved/dynamic calls and elapsed milliseconds.

`conductor.graph_context` keeps Python's AST skeleton and uses the native Rust
parser for Rust signatures, impls, traits and attributes. The bounded source
skeleton cache keys on actual content, so same-size/same-mtime edits are safe.

Graph test selection walks reverse dependencies transitively with cycle, depth
and node caps. It checks native source inventory against indexed content before
claiming complete selection. Dirty/unindexed source, unknown graph coverage or
cap exhaustion broadens to the declared test inventory, with `complete=false`
and explicit reasons. Missing or corrupt databases fail loudly. Candidate
validation retains its exact expected-commit check; broad fallback is not
evidence of complete selected coverage. Rust inline tests and contract planning
remain separate from the pytest runner.

Per-file coverage aggregates all absolute and relative aliases: only present
integer-zero values establish resolved dependencies. Any NULL, invalid or
nonzero value makes coverage unknown. Missing per-file coverage requires a
valid explicit legacy `unresolved_files` inventory; absent or malformed
metadata broadens selection. Matching source hashes alone cannot conceal a
conflicting coverage flag.

The current-only Rust 1.98.0 release benchmark uses 31 batches of 20 operations
after 10 warmups. With warm filesystem caches on an unisolated host, a confined
4 KiB hash measured 6.585 microseconds median batch mean (p95 6.755), and a
4 KiB text-plus-hash snapshot measured 6.858 microseconds (p95 6.991). A
projection with 12 caller edges across thirteen 2 KiB files, including source
hash validation and generation checks, measured 207.902 microseconds (p95
232.349). These exclude fixture setup, SQLite open, JSON serialization and
process startup; they are not a historical speedup comparison. Run
`cargo +1.98.0 bench --manifest-path native/forge/Cargo.toml --bench graph_context`
to reproduce the workloads. Benchmark executable SHA-256:
`5e9ff91f9692dab65a0b50a02ffb71b7b832a5367a2684eea88db712b8deb814`.

The command follows the practical pattern of filtering data inside tools before
returning it to the agent, described in Anthropic's
[code execution with MCP](https://www.anthropic.com/engineering/code-execution-with-mcp).
It uses a host SQLite index with bounded projections. A future MCP adapter can
use the [official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk);
an additional parser or MCP service is not required for these CLI commands.
