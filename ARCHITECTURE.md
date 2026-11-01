# Native tool orchestration architecture

Audit date: 2026-10-31. Baseline: `d9898475006dbc89eea557d9ab4642870ed522fe`.
The initial unrelated changes were verified byte-for-byte against already-landed
upstream commits; the checkout was aligned to `d8ab91b` without changing their
working bytes. A separate active session's candidate-review cache fix remains
outside this migration's current edits.
The open GitHub issue list was empty at the start of the audit.

## Initial audit substrate

This inventory and the gaps below describe the baseline before implementation.
The milestone evidence later in this document records the subsequent replacements.

The visible, nonignored inventory contains 1,508 files, including 595 Rust and
246 Python files. Production Rust lives in `native/forge` (96 source files),
`native/conductor-native` (101), and `native/slop-core` (30). The first two have
37 and 325 Rust test/fixture files respectively; slop-core also has inline tests.
There are three Cargo manifests and no production CMake project. Python package
and wheel manifests remain; a native executable is not evidence that every
configured host hook runs without Python.

| Capability | Existing implementation | Remaining capability |
|---|---|---|
| Structured calls | `native/forge/src/context_tools.rs` discovers schemas | Compiled validation, constrained generation, validated dispatch boundary |
| Code intelligence | `graph_context`, `graph_index`; slop-core Tree-sitter Python/Rust analysis | Unified language registry, C/C++/TS/JS definitions, references and scope queries |
| Patching | Host edit guards and mutation snapshot tools | Transactional tool-facing diff/search-replace with syntax validation |
| Search | SQLite FTS5 notes; `memory_index_sidecar.rs` mmap exact scoring | Python-independent code embeddings and HNSW/BM25 ranking |
| Execution | `task_run`, `task_limits`, `task_resources`, durable task store | Interactive PTYs and actual filesystem/network isolation |
| Context | `context_projection`, spill/recovery and provider usage accounting | Model-specific BPE, syntax-aware folding and diagnostic extraction |
| GPU | Embeddings provided externally | Measured workload justifying a native kernel |

`task_run.rs` explicitly implements process supervision, not a sandbox.
`context compose --max-tokens` uses a conservative byte bound, not BPE.
The mmap scorer is currently behind Python bindings and is an exact scan, not
HNSW. `dispatch.rs` still delegates unsupported hooks to a Python dispatcher.
Existing native code is the starting point; none of these mechanisms should be
duplicated behind a new harness.

A bounded production-source scan found `todo!`/`unimplemented!` only in scanner
documentation and test fixtures, not callable implementation stubs. That scan
does not prove completeness. At baseline, `task_run.rs` polled child completion
every 50 ms; the native reactor described below replaces that delay. Existing unsafe
signal/process/descriptor operations need targeted cancellation and descriptor
lifetime tests when the supervisor changes; their presence alone is not a defect.

The live review graph matched baseline HEAD but cannot certify dirty files.
Its AST tool attempted to parse Rust as Python; semantic search returned no
matching indexed nodes. Bounded source inspection supplied the missing evidence.
These are limits of this audit's graph queries, not a complete graph quality audit.

## Runtime contract

New core logic is Rust (or C/C++ for a measured native requirement), with no
Python/Node subprocess dependency. The updated goal requires every shipped code
path to be native, including compatibility entry points and repository tooling.
Interpreted implementations are deleted after their behavior is migrated and
verified; they are not archived. Reuse existing execution, storage and graph code.
Configuration fails explicitly if a capability is unavailable. Structured model
output conveys arguments, never authorization. Validation must precede handlers;
grammar-constrained sampling does not replace validation or host approval policy.

Registration may allocate. Steady-state argument validation borrows the input,
uses bounded stack state, and returns errors without allocating. Transport,
logging, registration and grammar generation have separately measured costs.
Do not turn microsecond in-process timings into claims about process startup,
model inference, filesystem operations or whole-task latency.

## Independently buildable milestones

1. **Structured-call boundary.** Add a reusable Rust schema compiler/validator,
   deterministic GBNF generator, closed tool registry and bounded JSONL CLI.
   Reject unsupported schema features at registration. Permit only an explicit
   fenced-JSON repair, revalidate it and expose the repair flag. Acceptance:
   adversarial syntax/schema tests, allocation-counted success and error paths,
   concurrent use, grammar acceptance checks, interpreter-free CLI tests and a
   reproducible release benchmark. See [structured output](docs/structured-output.md).
2. **Native code tools.** Extend existing graph parsing through a shared language
   registry. Add definition/signature/reference/scope APIs with file hashes and
   explicit unresolved edges. Stage patches in memory, require expected source
   hashes, reject ambiguous fuzzy matches, validate syntax, and atomically replace
   exact approved files. Acceptance: stale-source, symlink, traversal, ambiguity,
   syntax regression and crash tests; unchanged-file and formatting preservation.
3. **PTY and sandbox.** Extend the durable task journal with PTY read/write/resize,
   backpressure, disconnect/reattach, cancellation and process-group cleanup.
   Use nonblocking descriptors and a single Tokio reactor. Add explicit Linux
   namespace/mount/network/seccomp policy and cgroup limits. Preflight reports
   unsupported isolation rather than silently executing unconfined. Acceptance:
   terminal/job-control tests, floods, orphan cleanup and escape-negative tests.
   Rollback restores only task-owned paths with unchanged hashes; never reset a
   shared checkout. Git-ignored artifacts require a separate retention manifest.
4. **Hybrid code search.** Separate existing mmap storage from Python bindings,
   attach embedding model/dimension/normalization/content hashes, reuse FTS5 BM25,
   and add an HNSW candidate stage with exact reranking. Acceptance: atomic
   snapshots, corruption rejection, concurrent refresh, deletion freshness,
   recall@k against exact search, RSS and latency at realistic corpus sizes.
5. **Context virtualization.** Add model-fingerprinted BPE and AST folding to
   existing context projection. Preserve protected instructions, errors and
   recoverable originals. Extract compiler diagnostics with source spans and
   spill pointers. Acceptance: tokenizer parity, nesting/Unicode, token budgets,
   recovery roundtrips and no lost actionable diagnostics on captured fixtures.
6. **Measured acceleration and integration.** Benchmark end-to-end dispatch,
   cold/warm search and concurrent task throughput. Only add pooling/reranking
   kernels if measured tensor work dominates; compare native PyTorch/Triton/CUDA
   against a matched baseline including transfers, numerical tolerance and VRAM.
   Wire validated calls into native handlers and remove residual interpreter
   execution requirements only after compatibility coverage passes.

Each milestone requires correctness, integration and workload-specific stress
checks before the next depends on it. Use the repository's existing performance
receipt machinery for baseline/current improvement claims. The full architecture
is not complete merely because milestone 1 compiles. No model or GPU jobs are
needed for milestone 1.

## Milestone 1 implementation and evidence

Implemented in `native/forge/src/tool_schema/`, exported through the Forge Rust
library and `forge schema validate|grammar|serve`. This adds a strict compiled
schema profile, zero-allocation validation/repair, exact decimal bounds, immutable
tool registration, complete-call GBNF and a bounded persistent JSONL validator.
No new runtime dependency was added. The existing serde_json dependency now
explicitly declares `arbitrary_precision`; correctness must not depend on that
feature arriving transitively from conductor-native.

Validation on 2026-10-31 with rustc 1.93.1, release optimization, Linux x86_64,
AMD Ryzen 9 9950X3D:

- Forge crate release suite: **1,390 passed, 0 failed, 10 ignored** across its
  test binaries and doc tests. This is not the conductor Python compatibility
  suite or the complete repository landing gate.
- The optional GBNF test was then explicitly enabled: **1 passed**, covering
  56 actual grammar acceptance/rejection cases. Its engine was llama.cpp
  `0ec4027a2a0183de87c007069adddc06e665f7de`, built CPU-only as
  `test-gbnf-validator`; no model was loaded. Nine pre-existing ignored tests
  remain unexecuted.
- `cargo clippy --all-targets -- -D warnings`, Cargo formatting, and scoped
  `git diff --check` passed.
- Allocation instrumentation observed **zero allocations** over 1,000 repetitions
  each of valid, rejected, Unicode, repair and registry paths after registration.
  CLI integration tests passed with an empty `PATH`.

Measured warm in-process batch means (31 batches × 10,000 operations, after
10,000 warmup operations per workload):

| Workload | Input bytes | Median batch mean | p95 batch mean |
|---|---:|---:|---:|
| Escaped argument object | 64 | 148 ns | 155 ns |
| Rejected numeric bound | 34 | 97 ns | 97 ns |
| Registered call, one tool | 95 | 318 ns | 329 ns |
| Registered call, 256 tools | 95 | 405 ns | 413 ns |

One schema registration took 25.1 µs in this run. These are current-only local
microbenchmarks, not a baseline/current improvement receipt or individual-call
tail latency. The host was not isolated. The tested Forge binary SHA-256 was
`950074de80d85d797e30dc7165b5f609434397925fc0c8d4222c98c03a5ccfe1`.

Remaining boundaries: full JSON Schema support is not claimed; numeric grammar
constraints require runtime validation; the CLI validates but does not execute
tools; existing dispatchers have not been rewired. Milestones 2–6 remain open.
The installed Forge binary and running agents were not replaced or restarted.

The structured boundary now also dispatches the operator-enabled native tool
catalog through [the persistent harness](docs/native-harness.md). Large bounded
string grammars use factored repetitions; validator allocation guarantees remain
unchanged. The standalone `schema serve` command remains validation-only.

## Milestone 2 implementation and evidence

The shared native syntax registry now serves seven languages, with lexical
definition/signature/reference/scope inspection, dependency spellings, source
fingerprints and bounded function-body folding. Existing graph indexing reuses
it for C/C++/JavaScript/TypeScript/TSX and adds conservative literal-path edges.
Scoped symbol queries resolve correctly. Cross-file completeness remains false
for these languages until build-system semantics can prove it.

`forge code patch` prepares exact search replacements or unified diffs in memory.
Application requires an unchanged source hash/identity and valid candidate syntax,
retains exact old bytes through native libgit2, atomically replaces the selected
file and preserves mode. `forge code restore` recovers a retained version behind
a fresh current-hash precondition. Capability-based file access replaces duplicate
readers in inspection and the graph. See [native code tools](docs/native-code-tools.md)
for limits, concurrency semantics and recovery behavior.

Development validation on the same host:

- Forge release suite: **1,404 passed, 0 failed, 10 ignored** before the final
  deserialized-span regression was added. The separately verified GBNF engine
  remains optional, and the existing integration skips are not a full landing gate.
- Native conductor library, Python features disabled: **204 passed**.
- Strict all-target Clippy, formatting and whitespace checks passed.
- Patch cases include 1,000 independent line-splice comparisons, LF/CRLF/Unicode,
  final-newline markers, stale hashes, ambiguous recovery, syntax errors, traversal,
  symlinks, FIFOs, hardlinks, competing locks and exact Git recovery. The CLI also
  runs with an empty `PATH`.

Current-only warm release benchmark (31 batches of 100 operations):

| Workload | Input | Median batch mean | p95 batch mean |
|---|---:|---:|---:|
| Rust parse and inspect | 58 bytes | 5.90 µs | 16.27 µs |
| Rust parse and inspect, 1024 functions | 64,340 bytes | 5.15 ms | 5.21 ms |
| Fold inspected 1024-function source, including hash check | 64,340 bytes | 27.97 µs | 28.10 µs |
| Read, hash, prepare and validate a replacement | 58 bytes | 5.60 µs | 5.66 µs |

These measurements include the named operations, not tool transport or model
latency. They do not establish a baseline/current speedup. Native execution
supervision, isolation, hybrid search, BPE context and remaining interpreted-code
migration are still in progress under the active goal.

## Execution reactor progress

The existing task runner now uses the shared native Tokio descriptor reactor,
pidfd exit observation, signalfd interrupts, bounded asynchronous log capture and
one process-group owner. Its old 50 ms monitor sleep, 5 ms pipe polling loops and
dedicated reader threads were deleted. Existing task leases, admission, receipts,
process-group cleanup and cgroup verification retain their contracts.

The focused execution run passed **26 tests**, including all four previously
ignored real-systemd cases. Those verified actual CPU/memory controls, isolated
memory-limit failure, detached descendant cleanup and a success receipt after
cleanup outlasted the normal execution lease. Descriptor tests cover a 1 MiB
backpressure workload, cancelled reads, unreaped pidfd observations, bounded
storage, inherited open pipes and output failure propagation. Native syntax's
additional deserialized-span and UTF-8 signature tests also pass.

A current-only release benchmark of native spawn, pidfd wait and reap measured
**253 µs median / 292 µs p95 batch mean** (31 batches of 50 commands). This includes
process startup and is distinct from the sub-microsecond in-process schema
dispatch measurement. Benchmark binary SHA-256:
`40d35365a0354f45f27a9c9985488b0bc2342404bfe5495b5e32375ea7fd04a7`.
The host was not isolated. No matched speedup claim is made.

Linux PTY, bounded byte replay, strict Landlock/seccomp confinement, and direct
delegated cgroup primitives are now implemented and exercised together. The host
refused user-namespace UID mapping, so the implemented backend requires Landlock
ABI 6 and seccomp instead of changing host security settings. It does not claim
namespace isolation. See [native execution](docs/native-execution.md) for the
policy and its boundaries.

The latest focused execution suite passed **17 tests**, including four explicitly
enabled delegated-cgroup tests. Kernel readback and actual workloads demonstrate
CPU throttling, a 64 MiB memory ceiling, a process-count limit, and termination of
a detached descendant after its leader exits. The parent cgroup's controls and
the supervisor's membership remain unchanged. Sandbox tests cover filesystem and
network denials, signal scope, alternate ABIs, descriptor leakage, metadata races,
native C compilation/execution, and PTY interaction. The six sandbox tests include
one child fixture entry point; they are not six independent security proofs.
Strict all-target Clippy passed after these changes.

The persistent terminal actor now passes six integration tests (including its
native child fixture). It provides independent replay cursors, bounded requests,
kernel backpressure, partial-write receipts, 64-entry sequenced retry history,
explicit history expiry, timeout/close handling, and bounded completed-session
retention. Logs share the pipe runner's bounded sink. The journal and resource
admission modules are exported once through the Rust library for service reuse.

Resource probes now wait on pipes and pidfds directly; the old 10 ms polling
loop, duplicate process teardown, and hook waiter-thread/external-kill path were
deleted. The focused task/process suite passed 38 tests, including the existing
live systemd resource tests. This is correctness evidence, not a speedup claim.

For abrupt supervisor death, the service uses the existing systemd user manager to own
the daemon and all nested cgroups, with verified `Type=exec`,
`KillMode=control-group`, `DelegateSubgroup=supervisor`, and a bounded runtime.
The host runs systemd 255; DelegateSubgroup requires 254. The lifecycle is now
implemented and live-tested, including daemon SIGKILL, detached descendants,
runtime expiry and idempotent unit cleanup after systemd garbage collection.
Unmanaged launches fail. `harness start` verifies a ready private socket before
reporting success; no permanent service is installed.

The native harness now connects schema validation, AST/patch handlers, PTY actors
and the existing durable task journal. It bounds frames, connections, blocking
workers, sessions, replay, logs and task history. Operator policy supplies
capabilities and aggregate resources; model arguments cannot widen them. Leases
renew during execution, and graceful shutdown waits for hashed completion
receipts. Scratch disk quotas remain an explicit backend limitation.

`harness stdio` exposes the exact catalog through native MCP. It implements
2026-07-28 per-request metadata plus legacy handshakes for 2025-11-25, 2025-06-18
and 2025-03-26. Live tests use an empty PATH and cover concurrent calls, duplicate
keys/IDs, cancellation and adapter disconnects while service ownership persists.
The 18 focused lifecycle/API/PTY tests passed before MCP; the later integrated
run passed 14 library, 8 harness and 6 descriptor tests. Strict all-target Clippy
also passed before the final benchmark addition. These are development results,
not a complete repository delivery receipt. See [native harness](docs/native-harness.md).

A release benchmark of a validated `code_fold` call over one persistent Unix
socket, including source read/hash/AST folding and client JSON parsing, measured
**15.6 µs median / 27.2 µs p95 batch mean** over 31 batches of 100 calls after a
100-call warmup. The fixture was 26 bytes; service RSS was 11,836 kB, with a
one-CPU aggregate quota on the shared host. These are current-only measurements,
not a matched speedup or per-call tail latency. The Forge binary SHA-256 was
`f7324d5e3d67d1a0031883a0bdb26f4c910d3659acdcaedae111c8533c2722fc`;
benchmark executable SHA-256 was
`437c5d7c3bffb3f2ede6a9f770ba57702644de178fc10d9e5c28b1a89390ff02`.
Run explicitly with `FORGE_BENCH_SERVICE=1 cargo +1.98.0 bench --locked
--manifest-path native/forge/Cargo.toml --bench harness`. The benchmark owns and
stops its finite-lived service; it installs no persistent timer or unit.

## Delivery

The user explicitly authorized direct commits and pushes to the integration
branch, overriding the usual pull-request workflow. No feature branches or
worktrees may be created. The user clarified delivery as **existing `main`, with
no feature branches**. Only local `main` remains.
Preserve unrelated active-session changes. Forge's own AGENTS.md does not require
host governance claims or mutation evidence. Run the complete native replacement
for the existing local checks before delivery; dirty-tree tests alone are not
delivery evidence.

No task artifacts may be stored under `/mnt/data`. Native state resolution uses
`LEDGER_ROOT` when explicitly configured, otherwise
`${XDG_STATE_HOME:-$HOME/.local/state}/llm-forge/ledger`. Resolver failures are
reported; context composition retains the full input if it cannot safely spill.
Migration logs are under the user's local state directory, outside the checkout.

The active goal includes milestones 2–6, removal of all residual interpreted
implementations and build dependencies, integration validation, commit, and push.
It is not complete at a milestone boundary.

## Milestone 4 implementation and remaining integration

The initial native memory APIs compiled under `vector-search` without Python.
Candidate retention uses bounded O(k) heaps instead of sorting the whole corpus.
The later version-2 migration replaced the v1 prefix-freshness implementation and
deleted the full-file scan runtime. Shared bounded JSONL framing and strict
finite/dimension checks remain part of snapshot compilation and preview reads.

The new [vector index](docs/native-vector-index.md) provides seeded HNSW, exact
cosine controls, SIMD f64 scoring, preallocated search workspaces and validated
immutable mmap snapshots. It binds model, dimension, normalization, source-set and
payload identities. Allocation instrumentation observes zero allocations in
prepared successful queries and reranking. Release native checks at this stage
passed 210 library tests, two native legacy-memory API tests, five vector
integration tests and one allocation test; later SHA-256 JSON formatting is also
covered by its focused unit tests. This is not the final whole-repository gate.

The 32,768-row × 384-dimension benchmark explicitly records a difficult uniform
case: ef=400 recovered 63.2% and ran slower than exact scanning. For 32 clustered
groups, ef=160 recovered 99.0% at 169.5 µs versus 948.4 µs exact. These are matched
synthetic CPU retrieval measurements, not learned semantic relevance or
whole-tool latency. Exact search stays selectable and is the code-index default.
No GPU workload was launched or justified by these results.

[Code search](docs/native-code-search.md) adds a source-bound JSONL importer,
SQLite FTS5 BM25, exact reranking of the candidate union, reciprocal-rank fusion,
and `forge code-index build|query|status`. Immutable generations publish atomically;
reader leases protect old snapshots while refresh and retention run. Freshness is
verified for each returned source span. Seven integration tests passed, including
empty-PATH CLI use, failed-build preservation, concurrent publication, deletion,
checksums, escaped-output limits and failed-read byte budgets.

The native operator-pinned embedding client now supports bounded batches against
an existing local service. Four HTTP fixture tests passed; no real model or GPU
was launched. It binds artifact/runtime settings, rejects remote model entries,
proxies and redirects, and verifies identity before/after inference.

Native source planning and batched refresh are now available through `code-index
plan|refresh`. Six fixture tests passed for UTF-8/AST boundaries, batch limits,
source drift, provider failures, cooperative build cancellation and empty-PATH
CLI use. Empty index publication is also supported and tested without destroying
existing reader generations. No real embedding/model job was launched.

The persistent harness now exposes `code_search` for operator-named indexes.
Reader counts, rows and mapped bytes have explicit admission limits and memory
reservations. Cached readers observe atomic generation changes, and requests
are checked before provider calls. All 11 live harness tests and nine code-index
tests passed, followed by strict all-target Clippy checks. The provider checks
use a local HTTP fixture; they do not establish real-model retrieval quality.

The complete local retrieval pipeline has now been benchmarked: on the recorded
4,096-row synthetic corpus, BM25 took 80.23 us median, exact hybrid search
118.89 us and HNSW hybrid search 149.06 us. All 64 queries per mode completed;
HNSW top-ten overlap with exact was 1.0. Exact remains the default. These figures
include source validation and serialization, excluding model inference and
transport; they do not demonstrate real-code relevance. See `docs/native-code-search.md`.
Native BPE/context work is recorded below; the remaining interpreted-code
migration is still open. Nothing here marks the complete architecture ready.

## Milestone 5 implementation and evidence

[Native context virtualization](docs/native-context.md) now uses offline,
artifact-pinned Rust BPE tokenization, deterministic AST/log/diff projections,
chronological whole-exchange retention and immutable exact recovery. Counts cover
the final rendered text and an explicit caller reserve. Protected overflow is
reported rather than silently discarded. The persistent harness loads an
operator tokenizer once and admits one context operation at a time; recovery
streams bounded base64 chunks from private service state.

Six tokenizer tests passed, including an independent byte-BPE control and a
separately pinned GPT-2 artifact. Eight context tests cover 64 varied Unicode
histories, protected content, group integrity, diagnostics, recovery and failures.
All thirteen live harness tests passed and strict all-target Clippy passed at
this stage. A bounded SHA-256/count cache retains no prompt text and passes a
zero-allocation warm-hit test. Release measurements record both miss and hit
costs: 4 KiB counts take 694 µs uncached and 1.50 µs cached; repeated 212 KiB
compaction takes 2.65 ms versus 75.87 ms before this cache. These warm-repeat
measurements do not establish cold-path or model-task improvement. No model
weights were loaded. Full integration validation remains pending. The legacy
byte-budgeted projection remains for existing hook callers during migration.

Release validation subsequently passed 15 library tests, eight context tests,
thirteen live harness tests, six tokenizer tests and one allocation test. A later
recovery-staging ownership fix passed all eight context tests. These results
describe the tested working tree, not a final clean-host or committed-tree gate.

## Remaining native migration

The current tracked inventory still contains 63,000+ lines of Python under
`src`, shell adapters, Python test fixtures and interpreter-dependent packaging.
Native implementations added above do not by themselves retire those paths.
The next independently compilable steps are:

1. Make native libraries build without interpreter discovery by default. The
   conductor core now defaults to native source analysis and vector search;
   211 tests and strict all-target Clippy passed with `PYO3_PYTHON` deliberately
   pointing at a nonexistent executable. Its default dependency graph contains
   no PyO3. Forge already selects bundled SQLite. Transitional bindings still
   explicitly select Python and share that runtime's SQLite; they remain to be
   removed. Slop-core now also defaults to an interpreter-free build; its remaining
   CPython-only profiling/consolidation adapters require an explicit feature.
2. Finish native lifecycle hooks, handoff continuity and graph refresh, then
   delete the dispatcher/interpreter fallback and its superseded adapters.
   Preserve protected context, claim checks and additive host extensions.
3. Expose remaining governance, mutation, analysis and package-management
   operations through native APIs/CLIs, using existing Rust algorithms. Port
   their contract tests to native entry points before deleting Python callers.
4. Replace Python packaging, bootstrap/install logic and repository check recipes
   with native build/install/check paths. Native-language analysis fixtures may
   include other languages as data; no interpreted fixture runner remains.
5. Delete superseded implementations and compatibility bindings, verify the
   entire native dependency closure and clean-host installation, then complete
   exact-source release checks and delivery directly to `main`.

Unrelated dirty changes in the shared checkout remain outside this task's edits
and staging. The complete goal remains open until migration, integrated checks,
commit and push have all finished. No feature branch or worktree is needed.

Native lifecycle progress: verified handoff prepare/stage/activate/hook commands
now pass three unit and six empty-PATH CLI tests. State revalidation, envelope
integrity, idempotent activation, lineage, Unicode caps and durable owned writes
replace the handoff algorithm's interpreter dependency. The aggregate session
dispatcher has since been converted, as detailed below. Provider adapters still
require migration before all interpreted entry points can be deleted.

Background graph refresh now launches this same native binary, keeps a durable
inflight marker, inherits its exclusive worker lock, and supervises bounded index
children. Query waiting uses kernel notifications. Native libgit2 replaces the
indexer's Git subprocess. Six refresh integration tests, 27 graph-index tests and
49 post-tool/unit corpus tests passed, including failed-build preservation and
stale-query refusal. Existing frozen hook expectations were updated where native
refresh intentionally replaces tool-not-installed warnings and now queues source
deletions. Full integrated migration checks and final delivery remain outstanding.

SessionStart, SessionEnd and SubagentStop now use native lifecycle composition.
Startup retains policy, graph gate, bounded mailbox preview, at-most-once handoff
activation and additive host extensions; a compiled source fingerprint reports
stale Forge binaries. Journal publication atomically replaces the bounded daily
file before consuming source records. Native mailbox startup validates readiness,
preserves occupied listeners, scopes retry to one sender and bounds receiver
lifetime. Fifty integration tests and strict all-target Clippy passed at this
stage. The old explicit tool-dispatch fallback and its fixture have since been
deleted. Native dispatch always runs matching guards, and budget warnings no
longer bypass command denials. Provider installation adapters, interpreted
package surfaces and their tests still require retirement; this is
not a complete native migration or final delivery receipt. Details and measured
lifecycle costs are in `docs/native-lifecycle.md`.

The subsequent selected release sweep passed 592 tests with two existing ignored
tests across the native binary, lifecycle/dispatch, both frozen hook corpora,
graph refresh, handoff, mailbox, journal, process I/O and packaging contracts.
This precedes the next installation/doctor cleanup and is not a committed-tree
or clean-host receipt.

The analysis library now builds without PyO3 by default. Its silent-fallback
detector uses a native Ruff AST instead of CPython, and its Tree-sitter ABI is
aligned with the rest of Forge. A checked ablation API rejects malformed source,
unbounded trees and invalid edit spans. These changes passed 181 native release
tests and strict all-target Clippy with interpreter discovery disabled. Forge
now links this library for `analyze scan|rules|ablations`; CLI integration checks
are tracked in `docs/native-analysis.md`. Optional profiling/consolidation
bindings and the interpreter-driven equivalence runner still require migration.

Combined Python source checks now share validated native ASTs. On the recorded
128-function fixture this reduced the median combined scan from 8.710 ms to
4.962 ms with unchanged findings. Checked ablations now have explicit expansion
budgets and validate candidate syntax with both Tree-sitter and Ruff. Details,
artifact hashes and the associated validation costs are recorded in
`docs/native-analysis.md`.

The native analysis integration sweep passed 554 release tests with one existing
ignored test across Forge's binary tests, both frozen hook corpora, source
analysis, code-index retrieval, hook installation/doctor and SessionStart.
All-target Clippy passed for Forge and slop-core. The default Forge dependency
graph contains the native slop-core library and one shared Tree-sitter 0.27 ABI,
with no PyO3 or libpython dependency. This does not retire the root Python package;
provider/bootstrap adapters and the remaining core migrations are still open.

Native provider output and installation adapters are now implemented. Claude and
Codex use the native dispatcher; Qwen and Grok retain bounded A2A startup only.
`forge providers` offers read-only planning, explicit legacy-entry migration,
durable per-provider snapshots and drift-checked rollback. Grok startup uses a
bounded success set and permanent nonblocking lock; a duplicate-descriptor test
guards explicit lock release. The selected release sweep passed 511 tests with
one existing ignored test, seven shared-transform tests passed separately, and
strict all-target Clippy passed. See `docs/native-providers.md` for capability
and crash boundaries. Interpreted bootstrap/package entry points still need
retirement; this milestone does not complete the whole migration.

Build, install, default tests and the local-check/land/sweep policies now select
native Cargo artifacts. Installation uses an isolated prefix; runtime smoke
tests run with an empty executable search path. Dirty previews no longer
discover a venv or build a Python extension overlay, and report excluded adapter
contracts. Cargo now requires the compatibility feature before compiling those
220 adapter test executables, avoiding empty executables in native-only builds.
The native discovery planner remains in the default inventory. These changes do
not by themselves retire the root Python package or its maintenance recipes.

A shared bounded manifest reader now supplies paths, integration-branch choice
and SessionStart text from `forge.toml`, with legacy host-data fallback only when
that file is absent. Five native configuration tests passed, and the subsequent
Forge selection passed 440 tests with five existing ignored tests across unit,
provider/startup, preview, local-check, sandbox/cgroup and reactor contracts.
See `docs/native-project-config.md` and `docs/local-checks.md` for precedence,
resource bounds and verification scope.

The dependency audit identified vulnerable locked Zstandard crates and the Tokio
AsyncFd I/O-safety advisory. Zstandard is now 0.14.3 (zstd-safe 8.2.0 and zstd-sys
2.1.2), Tokio is 1.53.3, and reactor registrations explicitly document descriptor
ownership at the new unsafe API boundary. All three updated lockfile audits
report no blocking vulnerabilities. Forge retains the unmaintained `paste`
warning through tokenizers; no advisory was suppressed. These audits describe
the fetched RustSec database on 2026-10-31, not a general security certification.
Exact lockfile and advisory database hashes are in `docs/native-dependencies.md`.

The subsequent default release inventory has passing results for 375 conductor,
181 slop-core and 1,503 Forge tests, with 34 Forge tests ignored by default. The
full run exposed four obsolete delegation expectations in the edit/Read tests;
those now require native denials and refresh handling even with obsolete selector
variables. The affected targets and source-refresh contracts were rerun (23
passed, two optional latency probes ignored). This is full-inventory coverage
across the run and corrective reruns, not a single uninterrupted PASS receipt.
The 20 live harness/manager tests were also explicitly enabled and passed against
Tokio 1.53.3. Formatting and strict all-target Clippy passed for all three crates.
These working-tree checks do not complete the remaining migration or attest a
clean committed checkout.

Native host bootstrap now provides `forge project init|show|rollback`. It
validates the captured manifest, declared path choices and session policy before
fsynced publication; existing native bytes are preserved. Provider and project
setup share the same bounded snapshot/backup/CAS store. `forge mcp` can bind,
check, remove or roll back a `.mcp.json` connection to an explicitly selected
running service. It checks private socket ownership, source-root identity,
protocol, liveness and compiled catalog schemas within a three-second deadline;
the generated stdio command rechecks the host at startup. No service, timer or
client approval is implicitly created. Read-only native inspection calls now
participate in the existing hook marker; edit calls use the claim/access guard.
Native patch results bypass post-edit formatting so their source hashes remain
valid, and only applied successes queue graph refresh.

The bootstrap integration selection passed 444 tests (12 optional probes/live
tests left ignored in that selection); the graph-hook selection passed another
403 tests, with one existing latency test ignored. All 14 live harness tests
were explicitly enabled and passed, including launching the generated MCP
entry, inspecting source and restoring prior configuration. Five core manifest
tests also passed. These overlapping counts describe specific runs, not unique
test totals or a committed-tree receipt. The issue tracker was empty at this
milestone. Native configuration and MCP contracts are documented in
`docs/native-project-config.md` and `docs/native-mcp-setup.md`. Interpreted
bootstrap/package adapters and broader governance/mutation CLI migration still
require retirement; no final delivery is claimed. The unused checkout-local
`.claude/hooks/dispatch.py` launcher was deleted; no live client settings or
shared installed packages were changed. Its compatibility inventory row was
removed with it. Strict all-target Clippy and formatting passed for Forge and
conductor-native after the native bootstrap implementation.

The superseded `src/tooling/hooks` dispatcher, hook bodies and shell launchers
have now been deleted, together with the interpreted bootstrap entry point,
provider provisioning, interpreter-sync doctor and their unused fixture helpers.
No archive was created. Thirty-four interpreter-dependent contract targets and
their manifest/registry entries were retired; native hook, lifecycle, provider,
graph and configuration suites remain. The default native contract-inventory
suite passed all 33 checks after updating its expected inventory. Retired local
bytecode was deleted too. `project_init.py` contains only its pre-existing native
executable-selection accessor for ledger/A2A adapters that still await retirement;
it no longer implements bootstrap. The `conductor` script entry was removed.
`make crg-probe` and `crg-check` now verify an explicitly named native MCP socket;
the interpreter-installing `crg-sync` recipe is gone. Existing installed host
packages and client configurations were not modified.

The post-retirement bootstrap selection passed 548 native tests (two optional
tests ignored). Its release selection passed 604 tests (13 explicitly gated
tests ignored); the separately enabled 14 live harness tests passed as noted
above. These are overlapping working-tree runs, not a clean committed receipt.

`forge mailbox supervise` now replaces the interpreted retry supervisor. It
uses inotify, signalfd and pidfds, a persistent read-only SQLite connection,
finite native flush children, sender-lock deferral and durable lifecycle state.
No flush process starts on an idle cycle. Receiver ownership remains explicit:
existing endpoints survive supervision; owned endpoints are stopped and reaped
on expiry, error or interruption. Owned receivers also receive a finite lifetime.
The Python supervisor, CLI route and compatibility target were deleted rather
than archived. Remaining Python mailbox presentation and brief adapters still
need migration. Ten new supervisor integration tests and the affected transport,
startup, process-probe, queue, server and task suites passed (65 tests total).
The retired-target inventory passed all 33 checks. Tests used only disposable
local endpoints; no existing host receiver or real peer mailbox was changed.

The supervisor release selection passed 463 tests, with one optional latency
test ignored. Strict all-target Forge Clippy, formatting and whitespace checks
passed. The CLI now boxes its enlarged mutation-plan argument variant; the
concurrent mutation-priority implementation and its arguments were preserved.

Native mailbox `resolve` and `hold` now expose the existing durable lifecycle
operations. Resolution requires acknowledgment, preserves the original timestamp
under concurrent calls, returns bounded metadata and avoids loading bodies when
lifecycle state already exists. Legacy provenance reconstruction is capped at
1 MiB and remains pinned. Holds normalize bounded reasons and affect only inbound
state. The mailbox dispatcher was split into small handlers. Six new lifecycle
tests plus affected mailbox, retention and supervisor tests passed (46 total),
alongside all three core A2A store tests. These were development checks; combined
release results follow below. Brief adapters and broader native migration remain open.

Native `forge mailbox watch` now presents bounded batches from a read-snapshot
query that excludes presented rows and superseded informational updates while
retaining actionable requests. It has an exclusive consumer lease, finite lifetime,
inotify reconciliation, signal cancellation and bounded nonblocking output. Successful
output precedes presentation acknowledgment; interruption can replay a batch but
never marks an unwritten batch. Explicit unread replay does not spin on its own
SQLite writes. The stdio flag guard is shared with the MCP adapter and restores
inherited flags on normal teardown. Eight native watch integration tests passed
within a 52-test affected selection (10 opt-in harness tests ignored there); all
14 live harness tests then passed explicitly. The Python wait/watch implementation,
automatic graph attachment and two compatibility targets were deleted. Two mixed
Python-wrapper watch tests were replaced by native contracts. Source context is
available explicitly through native `forge graph refs` / `code inspect`.

The combined mailbox presentation/lifecycle release selection passed 470 tests,
with one optional latency test and ten opt-in harness tests ignored in that run.
The 14 live harness tests passed separately as noted above. The core store and
retired-target inventory passed 36 tests, and strict all-target Forge Clippy,
formatting and whitespace checks passed. These results describe overlapping
working-tree selections, not a clean committed-tree receipt or completed migration.

Native `forge session brief` now composes current policy/heading/claim state,
bounded unread inbox previews and configured task-note search without spawning
an interpreter. Explicit task-bound query artifacts can add memory and knowledge
cards from the same pinned vector space. Essential task/mandate/claim information
cannot be silently truncated; optional items fit atomically with omitted counts.
Missing optional stores are visible and corrupt stores fail before output. The
heading reader now streams a bounded prefix instead of materializing the entire
status document, claim/activity reads reject duplicate JSON keys and unsafe file
types, and Unicode-invalid timestamps return errors instead of panicking on byte
slices. Notes search honors the configured database and opens it read-only with
bounded strings, cache, lock wait and VM execution time. Native default handoffs
reuse this brief, and the superseded Python handoff and its dedicated fixtures
were deleted. Development validation passed 440 Forge tests and 41 core/inventory
tests; strict all-target Forge Clippy passed. The release selection exposed an
argument-parser edge case for tasks starting with hyphens; the task option now
accepts them, preserving the complete task text. The corrected release selection
passed 473 tests, with one optional latency probe ignored, and the final core/
inventory selection passed 41 tests. Final strict all-target Clippy passed for
Forge and conductor-native, as did formatting and whitespace checks. See `docs/native-session-brief.md` for
explicit contracts. These are working-tree checks, not final committed receipts.

The lifecycle benchmark measured the native brief at 0.817 ms median / 0.893 ms
p95 including CLI startup with optional stores absent. The explicit exact memory
scan over 4,096 rows x 64 dimensions (2,987,860 JSONL bytes) took 22.515 ms median /
23.010 ms p95. Both use 31 warm samples after three warmups on the shared host;
binary SHA-256 is recorded in the brief document. This measures software costs,
not a Python comparison or improved model quality. It records the baseline before
the mapped recall migration.

Native memory compilation now publishes version-2 checksummed read-only mmap
snapshots under an exclusive writer lease. Queries bind a held source inode,
perform positional preview reads, and reject changed source metadata. A complete
source SHA is captured during compilation and can be audited explicitly. The
superseded full-JSONL scan and its Python bindings were deleted; numerical oracles
remain in tests. The core release selection passed 13 tests covering concurrency,
replacement, stale metadata, checksums, file types and bounded parsing.

Shared session policy, active state, ownership, timestamps, note search, mailbox
presentation and brief composition now live in the Rust library. CLI and native
MCP use those implementations. Operator `workspace` and `memory_indexes`
capabilities control tool visibility, source/identity selection, vector space,
row/mapping admission and memory reservations. Recall retains one checked reader
per index, revalidates replacements, serializes per-index work asynchronously and
uses the bounded blocking pool. Neither tool performs implicit writes, message
acknowledgment, model launch or network lookup. Integrated release validation
passed 639 Forge tests plus all 13 live harness tests, including the new MCP
interfaces. The core memory, candidate-check and retired-target inventory
selection passed 58 tests. Harness vector dimensions are limited to 4,096 by the
closed grammar engine; standalone native memory supports 8,192. Ordinary catalog
tests compile optional tool schemas and exercise their numeric and vector bounds.
These working-tree results are not a clean committed receipt.

The Python CRG MCP server, response/workspace/embedding adapters, session brief,
six dedicated compatibility harnesses and three exclusive test helpers have now
been deleted. Native MCP configuration migration still recognizes the exact old
entry so operators can replace it explicitly. The legacy Python memory caller
now delegates complete retrieval to the held native Reader; its independent
rank/seek and speculative oversampling code were deleted. Source replacement can
no longer attach old scores to newly opened rows. Checkout classification now
propagates malformed Git markers to claim-gate denial instead of treating those
targets as unrelated files.

The Python messaging startup/hook pair (`a2a_session_start.py`,
`hook_installer.py`) and the identity-default test were retired with two
dedicated compatibility targets (`python_contracts_a2a_session` and
`python_contracts_hook_installer`). Native `forge mailbox fleet` now provides
the former operator-facing fleet-status report. Seven native aggregation and
three release CLI tests passed; eight transitional Python contracts passed
before the fleet adapter, bindings and target were deleted. The unused
historical research-pool launcher was deleted; explicit-identity native startup
and finite supervision remain. The remaining A2A server, registry and CLI
wrappers and their six compatibility targets have now been deleted, together
with the exclusive PyO store bridge and compaction/retention adapters. The
unused A2A SDK dependency and eleven now-unneeded transitive packages were
removed by regenerating the lockfile. Filtering
`rg --files src` for `\.py$` found 157 Python source files at the foundation
milestone. The whole native migration remains incomplete. This memory
surface is distinct from the existing mmap HNSW/BM25 native code-search service.

The native telemetry CLI now supplies record, byte-preserving hook tee and
bounded summary operations. Its writer serializes append and rotation through
an adjacent lock, rejects unsafe sinks and prunes only validated generated
rotation names. The Python telemetry adapter, optional PyO module and two
dedicated targets were deleted after the six native CLI integration tests passed.
State publication also uses exclusive private staging files and durable rename;
native claim lifecycle and activity updates now share a transaction lock;
remaining unlocked Python writers still prevent a mixed-runtime concurrency
guarantee. Stored claim paths are now
normalized and validated before identity verification and overlap matching.

The committed native foundation milestone at `15593893e5a6baf414c58a89a16c98f30a30b35d`
is pushed to `main`. Its manual suite passed **2,161 tests** with **39 ignored**;
the canonical full local check and verification also passed for that commit, as
recorded in `.git/forge-checks/run-1793499534968020846-3980898/receipt.json`.
At this milestone, `src/` contained 157 Python source files and the native
migration remained incomplete. The receipt covers that committed tree only; it
does not validate later uncommitted claims or ledger work.

The next migration adds `forge claim create|release|touch|list` and
`forge ledger budget rollup|audit|report|gate`. Native claim transactions
serialize overlapping creation and activity updates, with explicit lock errors.
Ledger history and origin lookup now use libgit2, and cache paths reject
spoofed GitHub hosts and unsafe path components. Budget CLI tests run with an
empty `PATH`; the cost verdict algorithm is shared with native ledger audit.
The cost-ledger, cost-budget and binary-selection Python adapters and their
two dedicated compatibility targets were deleted. Current Python source count
is 154. The remaining gate adapter only resolves and dispatches the native cost
phase; the wider governance and session migration is still open. See
`docs/native-claims.md` and `docs/ledger.md` for command contracts and limits.

The memory and session milestone adds bounded catalog-driven native memory
ingestion with pinned embedding recipes, immutable mapped generations, streaming
incremental reuse and reader-safe collection. Native status append and session
close now connect status, claims and explicit memory sync without interpreter
dispatch. The native workspace runtime matrix supplies opt-in finite probes,
strict receipt evaluation and evidence reconciliation. The superseded status,
session-close, session-preamble and workspace runtime Python adapters and their
dedicated compatibility targets are deleted; shared compatibility checks remain.
See [native memory and lifecycle contracts](docs/native-memory-ingestion.md).
Review tightened receipt validation, missing-root preflight and rejection of
dangling generation pointers before session mutations. Working-tree
release suites passed 2,204 tests with 39 optional tests ignored; 91 selected
compatibility checks also passed. The installed-layout fixture now stages the
fresh native extension instead of inheriting an older shared-venv dependency.
The workspace embedding mock reuses the bounded complete-request HTTP fixture;
100 consecutive repetitions passed after replacing its unbounded accept/read loop.
The milestone is committed and pushed to `main` as
`c587a2efca6451452cbbd4ba65d497b399925f37`. Full canonical local checks and
verification passed before publication and again after `origin/main` advanced.
The published receipt is
`.git/forge-checks/run-1793506230933333695-146382/receipt.json`, SHA-256
`7918fff3838b49694dab5f47f2e9e427319f2f813b1aef3ee927e0f0e8174b2d`.
That committed milestone leaves 146 Python source files under `src/`.

The current working-tree milestone adds native `forge cards build|search`.
Builders retain complete bounded card text, use the same pinned embedding/query
recipe as memory ingestion, recheck sources before atomic publication and keep
the previous index intact on failure. Offline recall validates native metadata,
text/source hashes and vector dimensions; partial removal of native metadata
cannot downgrade validation to the legacy schema. Valid legacy indexes remain
available for explicit offline queries with the exact matching fingerprint.
Queries borrow stored text and vectors and copy only selected results.

The Python memory ingestion, indexing, card retrieval, automatic indexing,
memory-vector utilities, state/policy adapters and obsolete inspection/replay
helpers are deleted, together with their exclusive bindings and compatibility
targets. Native memory/session commands supply the active behavior; unused
auto-index queues, recency/dedup utilities and memory-only replay substitutions
are retired. Shared compatibility coverage remains, with 98 selected checks
passing. There are 138 remaining Python source files under `src/`; this is an
intermediate migration milestone, not a completed native-only repository.
The complete native core and Forge development suites passed 2,033 tests with
39 optional tests ignored, and both crates passed strict all-target Clippy.
The committed-tree release receipt is a separate publication check.
