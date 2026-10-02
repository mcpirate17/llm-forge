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
consistent database snapshot. External graphs without content hashes have
unverified endpoint freshness: an `ok` status never proves exhaustive coverage.

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

The command follows the practical pattern of filtering data inside tools before
returning it to the agent, described in Anthropic's
[code execution with MCP](https://www.anthropic.com/engineering/code-execution-with-mcp).
It uses a host SQLite index with bounded projections. A future MCP adapter can
use the [official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk);
an additional parser or MCP service is not required for these CLI commands.
