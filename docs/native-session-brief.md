# Native task briefs

`forge session brief` composes local task context in one Rust process:

```sh
forge session brief --host /path/to/host --task 'compiler diagnostics' \
  --paths src/compiler --identity agent-name --json
```

It reads current policy, up to four active headings, live ownership claims
overlapping the requested paths, an existing mailbox's compact unread snapshot,
and up to three lexical task-note matches. The notes database follows
`conductor.notes_db` in `forge.toml`, the supported legacy manifest setting, or
`CONDUCTOR_NOTES_DB`. Brief search restricts the source to `tasks`; ordinary
`forge notes search` uses the same configured database and searches all sources.

Missing optional state is reported. Invalid policy, claim stores, mailboxes or
indexes fail before any context is printed. No model, receiver, indexing process
or network request is started. The brief does not write active state, touch
claims, or mark messages read or presented.

The default text budget is 3,500 Unicode characters, configurable from 1,024 to
16,384. The CLI's final newline and optional JSON envelope are additional. Task,
mandate IDs and every overlapping active claim are retained together; overflow
requests narrower paths or a larger budget. Other context fits as whole items.
`omitted_items` and an `OMITTED` line disclose omitted items. JSON uses schema 1
and authority `non-authoritative-session-brief`. These observations do not
authorize actions.

## Explicit semantic recall

Memory and knowledge cards share one offline query vector:

```sh
forge memory build --source memory.jsonl
forge session brief --host /path/to/host --task 'compiler diagnostics' \
  --query query.json --memory-index memory.jsonl --cards-index cards.json
```

Native catalog ingestion and full-card creation are documented in
[native memory ingestion](native-memory-ingestion.md). Both builders share the
same native query recipe when given the same pinned embedding configuration.
Their indexes can therefore be queried together; legacy indexes still require
their own exact matching fingerprints.

Either index can be supplied independently. The query file contains exactly:

```json
{
  "schema_version": 1,
  "task_sha256": "<64 lowercase hex digits: SHA-256 of trimmed task UTF-8>",
  "embedding_fingerprint": "sha256:<64 lowercase hex digits>",
  "vector": [0.25, -0.75]
}
```

The vector must match the index dimension, from 1 to 8,192, with finite values
and nonzero norm. Forge normalizes it with scaling to avoid numeric overflow.
Both indexes must match the complete embedding fingerprint. Mismatched tasks,
spaces, dimensions and invalid values are errors. This artifact is
operator-supplied provenance, not attestation of the model that produced it.

Memory uses schema-3 JSONL compiled to an immutable version-2 mmap snapshot. Cards
use existing schema-2 JSON and native card scoring. Each returns at most three
previews, with whitespace collapsed, frontmatter removed, and bodies capped at
240 characters. Ranking preserves stored embedding-space dot-product semantics
and stable ties. No broker or different embedding model is silently selected.
The separate native code-search service retains mmap HNSW/BM25 interfaces;
brief recall never builds a sidecar during a read. The default snapshot is
`MEMORY_INDEX.sidecar`; `--memory-sidecar` selects an explicit artifact. Missing,
stale or corrupt snapshots fail and require an explicit `forge memory build`.
The superseded full-file scan runtime and its exported bindings were deleted.

`forge memory query --source memory.jsonl --query query.json --task 'compiler diagnostics'`
returns 1–64 scored previews (eight by default). `forge memory verify --source
memory.jsonl` additionally audits the complete source SHA-256. Build accepts
`--output`; query and verify accept `--sidecar`. Snapshot output directories must
already exist, belong to the current user, and exclude group/world writes.

## Bounds and consistency

Tasks are limited to 1,024 characters / 4,096 bytes and requests to 64 normalized
relative paths of at most 1,024 bytes each. Heading scans stop after four headings
and reject a prefix over 1 MiB or a line over 8 KiB. Claim/activity documents are
bounded to 1 MiB and reject duplicate JSON keys. These file inputs reject final
symlinks, special files and invalid UTF-8.

SQLite opens existing notes databases read-only with a 250 ms busy timeout,
2 MiB page cache, 4 MiB row/string limit and two-second VM deadline. Files over
4 GiB are rejected. VM deadlines cannot interrupt a stalled filesystem syscall.
Query artifacts are at most 1 MiB; memory JSONL is at most 512 MiB, one million
rows and 1 MiB per row; card JSON is at most 16 MiB and 4,096 cards. Mapped memory
artifacts are limited to 2 GiB and 8,192 dimensions. Opening verifies header and
payload checksums, finite vectors, offsets, layout and source identity. Queries
use held descriptors with positional reads and check device, inode, length,
mtime and ctime before and after retrieval. Only winning JSONL rows are decoded.
An atomic snapshot rebuild does not invalidate existing readers. Changed source
files require rebuilding. The complete source hash is captured at compilation;
normal queries use metadata freshness checks rather than rehashing the source.

Builders hold an exclusive writer lease, publish a unique read-only inode by
atomic rename and fsync the file and directory. Failed builds preserve the
previous artifact. Snapshot symlinks, writable files and hard links are rejected.
Operator-controlled parent directories and unconfined same-UID writers remain
trusted; mapped inodes must never be chmodded or modified in place. Checksums
detect corruption and do not authenticate an owner-supplied artifact.

Policy, headings, claims, inbox and indexes are independent observations, not a
cross-store transaction. Claim expiry is evaluated during state capture.
Native `forge handoff prepare|stage` uses this operational brief when neither
`--context` nor `--context-file` is supplied. An explicit context file can carry
semantic results selected by the caller.

## Persistent service

The native harness and MCP adapter use the same Rust brief and recall library.
Tools are absent unless enabled in the operator's policy:

```json
{
  "workspace": {"identity": "agent-name", "max_chars": 3500},
  "memory_indexes": {
    "notes": {
      "source": "/private/index/memory.jsonl",
      "snapshot": "/private/index/memory.jsonl.sidecar",
      "fingerprint": "sha256:<64 lowercase hex digits>",
      "dimension": 1024,
      "max_rows": 20000,
      "max_mapped_mib": 128
    }
  }
}
```

These are optional fields added to the complete harness policy. `session_brief`
accepts `task`, optional relative `paths`, and a `max_chars` no greater than the
operator limit. Root and mailbox identity come from policy. A null identity
disables inbox lookup and does not inherit an environment identity. The workspace
capability grants access to the source root's configured policy, claims and notes.

`workspace_recall` accepts an operator index name, `task`, the same task-bound
query object shown above, and optional `limit` from 1 to 64. It accepts no path,
model endpoint, fingerprint override or build instruction. The client supplies
the embedding artifact; this tool does not generate one. Results include snapshot
metadata and `non-authoritative-workspace-recall`. Fingerprint and task binding
are consistency checks, not model-origin attestation.

Admission validates canonical existing source/artifact paths, vector space, row
and mapping limits. The harness accepts dimensions from 1 to 4,096, matching the
bounded grammar engine; the standalone memory API/CLI accepts up to 8,192.
The catalog derives vector bounds from admitted indexes, and runtime checks the
selected index's exact dimension. Up to eight indexes are allowed. Aggregate memory must cover
each mapping plus 16 MiB scratch, and 32 MiB for briefs. Each index retains one
reader. Queries serialize per index behind an async semaphore before entering the
bounded blocking pool, leaving the reactor free. Subsequent calls reuse a checked
mapping; changed paths drop that mapping and admit a new matching snapshot.
Source changes without a valid rebuild fail without writes. Atomic rebuilds are
visible on the next call. Metadata checks surround every query.

Python `conductor.inplace_handoff` and its interpreter-dependent fixtures have
been deleted. The Python CRG MCP server, workspace/response/embedding adapters,
session brief and their dedicated interpreter-dependent fixtures are also
deleted. Native `forge mcp bind --replace-legacy` retains exact old-entry
recognition so existing host configuration can be migrated explicitly.

`native_session_brief`, `native_handoff`, `native_session_start`, native memory
contracts and notes-reader unit tests exercise these boundaries. The selected
release validation recorded 639 ordinary tests, 13 live tests and 58 core tests
passing. These are selected suites, not a claim that the complete repository
validation ran.

For release binary SHA-256
`0beb68a9b66aba24b908465b72ff3d8b3987dbd627162e5f46f14d897ff57650`, the lifecycle
benchmark produced the following 31-sample warm measurements after three warmups
on the shared development host. Every sample includes process startup:

| Operation | Median | p95 |
| --- | ---: | ---: |
| CLI version startup | 0.790 ms | 1.019 ms |
| Brief, optional stores absent | 0.817 ms | 0.893 ms |
| Brief with exact legacy memory scan | 22.515 ms | 23.010 ms |

The memory fixture is 4,096 rows x 64 dimensions and 2,987,860 JSONL bytes. These
measurements are the pre-migration baseline; the scan implementation has since
been removed. Run `cargo +1.98.0 bench --locked --manifest-path
native/forge/Cargo.toml --bench lifecycle` to reproduce on another host.

## Current memory benchmark

The later measurements used the same 4,096 x 64, 2,987,860-byte memory fixture
and release binary SHA-256
`19a9ca7d0bfe066edfe9f8484e8421483df105b36a7d3f9ecf327d4857bee956`.
They were collected sequentially on the shared development host, so the CLI
comparison is an observed workload comparison, not a controlled causal speedup
estimate.

The new CLI brief with validated mmap memory used 31 warm process samples and
includes process startup. Against the historical exact-scan CLI baseline above,
the median was 1.805359 ms (p95 2.083070 ms), versus 22.515 ms (p95 23.010 ms).
The current lifecycle log also records the brief with optional indexes absent
at 0.947845 ms median / 1.068186 ms p95. Building the mmap snapshot took
23.213515 ms for the fixture.

Persistent harness measurements are current-only and cover validated calls over
a persistent Unix socket, including client parsing, under a one-CPU service
quota. Each percentile is over 31 batch means, with 100 calls per batch; the
first-call latency is reported separately:

| Operation | First call | Median batch mean | p95 batch mean |
| --- | ---: | ---: | ---: |
| Mapped workspace recall, 4,096 rows x 64 dimensions | 818.614 µs | 275.211 µs | 281.223 µs |
| `session_brief` | 60.410 µs | 14.840 µs | 15.908 µs |
| `code_fold` | 225.881 µs | 14.235 µs | 15.949 µs |

The harness process reported 16,408 kB RSS. These persistent-call measurements
exclude CLI process startup and should not be compared directly with the CLI
table. Both logs describe timing on a shared host; they do not establish an
improvement in model capability.
