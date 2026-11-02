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
Build with `make build`, then run explicitly with
`FORGE_TEST_BIN="$PWD/native/forge/target/release/forge" FORGE_BENCH_SERVICE=1 cargo +1.98.0 bench --locked
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

The card milestone adds native `forge cards build|search`.
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
The milestone is pushed to `main` as
`b8ba54e81100295ced844ad12409e1c33a00d9f3`. The full committed release gate
passed 2,214 tests with 39 optional tests ignored; publication and the updated
`origin/main` were verified with receipt
`.git/forge-checks/run-1793508061255354039-244882/receipt.json`, SHA-256
`ec3f4c39bf8f4332b44cf6378970c4e716fa168663c99503a6a7ad2a719c4d6d`.

The broker milestone replaces the live Python embedding broker with native
`forge embedding-broker serve|ensure|status|warm`. It preserves accepted legacy
route fingerprints and the port-7317 health/model/embedding protocol, including
quality-bound selection, pinned vector spaces, explicit paid-route enablement,
bounded native TLS and Ollama resource controls. Concurrency admission, absolute
header deadlines, body limits and an overall backend deadline bound work.
Native startup serializes contenders, refuses mismatched occupied endpoints,
verifies newly started listener ownership and never downloads models. Service
lifetime remains persistent by default; an explicit finite limit is available.
Paid attempts reserve estimated cost against both their route cap and a cumulative
request ceiling, retaining reservations after ambiguous failures. Reservations
are not actual billing evidence. See [broker contracts](docs/native-embedding-broker.md).

The broker, routing and HTTP Python modules and their exclusive compatibility
fixtures are deleted. Unwired Python project-context and wheel-resource APIs
are also deleted; native context primitives/tests and embedded runtime assets
remain. Optional package metadata no longer directly requires Starlette or
Uvicorn. That milestone leaves 133 Python source files under `src/`;
candidate review, mutation orchestration and other adapters still require migration.

Native fixture tests exercise HTTP/CLI startup with an empty `PATH`, concurrent
ensure, saturation/recovery, slow GPU probes, finite shutdown, TLS trust and
credential refusal, routing, budgets and validation. No live model, paid backend,
shared service or installed environment is used. A shared bounded JSON document
parser now supports explicit storage/backend limits while preserving the 1 MiB
tool-call default and duplicate-key checks; this fixes the card reader's former
1 MiB parse limit beneath its documented 16 MiB index bound. Full native core
and Forge development suites passed 2,068 tests with 39 optional tests ignored;
strict all-target Clippy passed for both crates. All 54 selected compatibility
checks passed. Concurrent ensure and saturated-worker recovery each passed
25 repeated runs, with no fixture brokers left running. The milestone is pushed
to `main` as `2dbfa2be139b90f09b8fe727ed8a98638f52d476`. Full release checks
passed 2,249 tests with 39 optional tests ignored before and after publication.
The published verification receipt is
`.git/forge-checks/run-1793511114083464008-324395/receipt.json`, SHA-256
`7f9c5749469ecac5759fcc1bac02f69897137722d57003cb5a361836770c71b3`.

A current-only Rust 1.98.0 release benchmark measured median batch means of
153.85 ns for escaped-argument validation, 98.06 ns for a rejected numeric
range, 317.68 ns for a registered call and 443.80 ns with 256 registered tools.
The corresponding p95 batch means were 154.39, 115.95, 318.98 and 445.25 ns.
Each workload used 31 batches of 10,000 operations after 10,000 warmup
operations. Registration took 23.83 microseconds. These are in-process costs on
an unisolated host, not HTTP/model latency or a matched improvement claim.
The benchmark executable SHA-256 was
`312ba978e8af7af3b99611cb7d29e951d23f3510a8a296cb6dc77b33a064ff9b`.

The notes milestone completes `forge notes index|search|tables`. It honors
configured roots, bounds file/entry/text work, checks sources through directory
capabilities and preserves both indexes and unrelated tables on rebuild failure.
Existing SQLite files and sidecars are checked before use; schema impostors and
custom index-table triggers are refused. Entire rebuilds, including DDL, use an
immediate transaction and bounded lock contention. Table queries retain legacy
dimensions, headings and Unicode JSON content with bounded output. See
[native notes contracts](docs/native-notes-index.md) for limits and trust boundaries.
The retained old-versus-native parity target passed its two tests before deletion,
including the full 18-note fixture and three ordered queries. The Python notes
implementation and exclusive compatibility target are deleted; shared native and
compatibility helpers remain. There are 132 Python source files under `src/`.
This migration remains incomplete; candidate review, mutation orchestration and
other Python adapters still need native replacements.
Notes validation passed 17 unit tests and 14 CLI integration tests, including
rollback, schema/sidecar refusal, WAL compatibility and concurrent snapshots.
The initial `9df7722` release gate passed 2,272 tests with 39 optional ignores,
but its post-push concurrency check exposed a transient journal-unlink race:
path lookup could return an inode whose link count had just reached zero.
The repair performs bounded sidecar reinspection and retains refusal of unsafe
replacements. Deterministic regressions use actual held, unlinked inodes.
After repair, 100 further concurrency-test repetitions passed (1,600 competing
rebuild attempts and 800 read queries, with successful retries and final integrity
checks). Native core tests passed 406 cases with one optional ignore; strict
all-target Clippy passed for both crates. The selected contract discovery,
runner and project-path compatibility targets passed 53 tests. The repair is
pushed to `main` as `664dd76a685611063dc040a4c2dd7a1ddf35bbaa`. Full committed
release checks passed 2,274 tests with 39 optional ignores before and after
publication. The published verification receipt is
`.git/forge-checks/run-1793514496983537219-452360/receipt.json`, SHA-256
`49a71ca2303ac14fd530eca388649aba0dc3a1faee07956365466c5b52df90ee`.

The candidate-review foundation adds `forge candidate inspect`,
`materialize`, and `verify-receipt`. It resolves staged, commit and range
candidates through libgit2; validates and writes one Git blob at a time into
a private standalone snapshot; and verifies legacy candidate receipt digests
using native canonicalization of finite JSON values. Snapshot preflight checks
paths, modes, object sizes and transitive relative symlinks before writing;
publication refuses existing destinations. A valid receipt digest is not an
approval. Optional ref binding compares the exact candidate tree with a resolved
commit. See [candidate contracts](docs/native-candidate-review.md).

At that foundation boundary, the remaining candidate slices were policy and
waiver classification; bounded check execution and evidence collection; then review
orchestration, reporting and attestation. The existing Python package remains
until these interconnected consumers move. No Python candidate implementation
is retired by this foundation alone. Targeted validation passed 27 unit tests
and seven CLI integration tests; every CLI fixture runs with an empty `PATH`.
The receipt canonicalizer matches 7,525 finite-number reference cases and 26
hashed CPython fixtures without an interpreter at test or runtime. Strict
all-target Forge Clippy passed. Fifty further concurrent-publication runs
passed, each with 16 competing writers, exactly one winner and no staging leaks.
Index admission now parses the exact checked bytes from a sealed memory file,
closing the original-path replacement race. It is pushed to `main` as
`229a54e677426c10609f72b00ad76bf8cebfda97`. Full release checks passed 2,308
tests with 39 optional ignores, plus two installed-binary smoke tests, before
and after publication. The published verification receipt is
`.git/forge-checks/run-1793516427617319797-496007/receipt.json`, SHA-256
`724144368c70aba9cf3298564355369243774be2a9cc63624c3292eb36a16c2d`.

The current-only Rust 1.98.0 release candidate benchmark measured median batch
means of 2.773 microseconds for receipt digest verification, 29.601 microseconds
for a one-file staged candidate, and 4.149 microseconds for one-file snapshot
preflight. Corresponding p95 batch means were 2.970, 29.881 and 4.319
microseconds. Each workload used 31 batches after 100 warmup operations, with
1,000 operations per receipt batch and 100 per Git/snapshot batch. These are
unisolated-host, in-process costs, not model/HTTP latency or a matched improvement
claim. Benchmark executable SHA-256:
`816c72281995161c694c7e3a98d6667a590f8508bacadedac00d1ec46346b6e5`.

Native `forge candidate plan` now loads policy and required baselines from a
private snapshot of the exact Git candidate, classifies both sides of renamed
paths, preserves ordered profile checks, and reports selected files. It does
not execute checks or issue review approval. Candidate-only configuration
capture prevents mutable-path reopens; confined policy reads support safe
internal symlinks. Ordered TOML conversion preserves check/tool declaration
order and literal calendar dates. Raw source bytes bind the policy digest.
Explicit UTC-date validation and bounded classification/selection work fail
before unsafe or excessive inputs are processed. Successful planning explicitly
discards its private snapshot before returning.

The shared Rust classifier now compiles glob patterns once per batch and reuses
path/matching buffers, replacing allocations for every pattern character and
match. Stateless compatibility APIs retain their contracts. Check file
selection borrows the captured changes instead of copying them for every check.
Targeted validation passed 34 Forge unit tests, 14 candidate CLI tests, 250
native-core unit/contract cases, and 34 selected optional Python compatibility
cases using a private extension overlay. Strict all-target Clippy passed for
both crates. Twenty-five further runs of eight concurrent plans completed with
no private snapshots left behind. Non-UTF-8 environment overrides now fail
explicitly instead of falling through to another policy; CLI and core
regressions preserve higher-priority override semantics. The policy milestone
was pushed as `79bb6ac70a8a3c3aa25b1da41f5b1f46612f5444`. Its published-head
gate exposed a doctor test whose native child could exit without consuming its
input. `26b18e55379882a37c012471a352ceb526fa3f2b` replaces those children with
Rust fixtures that require input and drain it before returning each outcome;
production probe behavior is unchanged. The focused test passed 100 consecutive
runs. Full release checks then passed 2,326 tests with 39 optional ignores,
plus two installed-runtime tests, before and after publishing the fix. The
published verification receipt is
`.git/forge-checks/run-1793519625028255297-588061/receipt.json`, SHA-256
`815d2445911a2c597f092c36f3493e7575f241595f82862713cae54d4dbe5e47`.

The final policy benchmark used the same 128 renamed-path inputs, glob rules,
Rust 1.98.0 executable and measurement loop for both classification APIs.
Prepared classification measured 1.397 microseconds per change (p95 batch mean
1.419), versus 2.332 microseconds (p95 2.475) for the stateless API that recompiles
patterns on each call. Each used 31 batches of 1,000 operations after 100 warmup
operations. This isolates API reuse within the current implementation; it is
not a historical-version or end-to-end planning comparison. The host was not
isolated. Benchmark executable SHA-256:
`14492324d575ab6d4d13a2abc3d5083837403f31d047e919feb0b301c53ed867`.

Native `forge candidate assess` now evaluates supplied results against the same
candidate policy and snapshot preparation as planning. It recomputes exceptions
and attribution, preserves stable finding fingerprints, and reports stale
exceptions only when their own check examined matching paths. Results remain
explicitly unverified and incomplete; the command does not run checks or issue
approval. Execution errors and reserved engine-integrity failures cannot become
inherited debt or be suppressed by an exception. This closes a legacy assessment
case where a pathless crash from a diff-attributed check could cease to block.
Exceptions are grouped by check and use shared compiled glob programs. Glob
compilation now avoids repeated suffix scans for unmatched opening brackets.
Input counts, structural size, paths and matching work are bounded. Targeted
validation passed 17 assessment tests, seven new CLI tests, seven existing policy
CLI tests, four native glob tests, ten native policy contracts and 19 optional
policy compatibility cases through a private extension overlay. Strict all-target
Clippy passed for both crates. Twenty-five runs of eight concurrent assessments
mixed passing and invalid results without leaving snapshots. Native AST inspection
found no function above 100 lines in the 13 inspected implementation, test and
benchmark files. Assessment was committed and published as
`ac8d2448d46eef52672b200a22f949c1f4a68fdd`. Full release checks passed 2,351
tests with 39 intentional ignores, plus two installed-runtime tests, before
and after publication. The published receipt is
`.git/forge-checks/run-1793521322933453734-637358/receipt.json`, SHA-256
`e193da64dc600469d235985ecfbd9e0ba6f055e3984057a88e17f4f6f4553f2c`.

The current Rust 1.98.0 release benchmark measured prepared glob matching at
0.116 microseconds (p95 batch mean 0.124), versus 0.286 (p95 0.294) for stateless
matching on the same six cyclic Unicode/bracket inputs. Each used 31 batches of
1,000 operations after 100 warmups. A complete supplied-results assessment of
32 findings, with diff/rename attribution, matching and stale exceptions, measured
36.700 microseconds (p95 batch mean 37.367) over 31 batches of 100 operations.
Preparation of the benchmark inputs is outside the timed loop; assessment's
validation, matching, hashing and output allocation are inside it. These are
unisolated-host, in-process measurements, not end-to-end review latency or a
historical-version improvement claim. Benchmark executable SHA-256:
`191a5983dad8f4e3e3ca4b33b3f5344a5224586f6c2690b9eeea94c748aaafb2`.

The remaining candidate work is bounded check execution, evidence and waiver
activation, then complete review reporting, attestation
and caller migration. Python candidate policy wrappers still have callers in
gate, verification and shared tooling; deleting them before those consumers move
would break existing workflows. The overall native migration remains incomplete.

The current-work Python guard is now deleted. Native hook input normalization
closes an empty-object alias bypass in Read calls and makes every downstream
handler inspect the same selected object. Empty aliases defer to populated
alternates; nonempty canonical objects keep precedence; malformed populated
inputs deny. Notebook paths receive the same protected-file check. Canonical
input selection borrows its payload without cloning large tool arguments.
Root scalar `input` strings remain available as provider patch/text data.
Conflicting path fields cannot hide a protected notebook path behind a safe
file path. Direct guard calls also reject malformed populated tool-name and
command aliases; dispatch retains its stricter conflict checks.
At the guard milestone, the independent Python local-clerk policy remained
with its own compatibility target and 131 Python source files remained under
`src/`. The clerk replacement below advances that separate retirement.

Focused release validation passed 601 native tests with two intentional ignores,
including the frozen hook corpus, dispatch, Read, protocol and workdir checks.
All 34 contract-discovery tests and the two affected optional compatibility cases
passed using a private extension overlay. Strict Forge all-target Clippy and
scoped compatibility-target Clippy passed. Six inspected Rust files had complete
AST results and no function above 52 lines. Guard retirement was published as
`1c9ffa4d6796ff25b144e06a254ac31bf2466003`. Full release checks passed 2,375
tests with 39 intentional ignores, plus two installed-runtime tests, before and
after publication. The published receipt is
`.git/forge-checks/run-1793523438983199297-688428/receipt.json`, SHA-256
`d8081194559a94a372115f84b019864def76839beca6e769142c8c7b9beaf4d2`.

The current-only guard benchmark used Rust 1.98.0 release, 31 batches of 1,000
operations and 100 warmups on an unisolated host. Canonical normalization of a
payload carrying 64 KiB of content measured 0.012 microseconds median batch
mean; alternate-object normalization, including reset and clone, measured
0.124 microseconds (p95 0.127). Full guard verdicts measured 0.451 microseconds
for a protected Read (p95 0.454) and 1.540 microseconds for a protected Bash
command (p95 1.843). These timings exclude JSON parsing, process startup and
other hook handlers; they are not a historical comparison or model latency.
Benchmark executable SHA-256:
`ee1d05b7ce6c62e3a4a989f635e1016f2460185c7c7ddd869d659ee2efaa0306`.

The local clerk now runs through native `forge clerk inventory|draft|validate`.
The CLI captures configuration explicitly and reuses the native local-model
authority policy. Full source hashes remain distinct from bounded prompt
prefixes. Generation uses bounded, proxy-free loopback HTTP with an explicit
clerical task, strict response parsing, exact reported model-label checking,
and full source revalidation after inference. The model label is not artifact
attestation and a valid document grants no approval. A read-only destination
preflight precedes inference; exclusive, descriptor-relative publication
rechecks roots, parents, sources and staging identity. Competing destinations
remain intact, and errors retain cleanup failures. Source files are never
rewritten. These are capability/path checks, not protection against an
unconfined same-UID process after the final check.

The Python clerk and local-policy modules and their dedicated compatibility
targets are deleted. Shared hook behavior uses the same library policy and
lexer. Existing hook fixtures now import that policy instead of recompiling
duplicate unit tests; the independent policy edge case is covered natively.
There are 129 Python source files remaining under `src/`; migration is still
incomplete. Focused release checks passed 369 native tests, including the
broker's TLS transport fixtures and native-only CLI execution. Strict
all-target Clippy passed. The 12-file native AST scan retained every symbol;
one reference list hit its explicit cap, while all function spans were retained
and no function exceeded 79 lines. The native discovery suite passed 33 tests;
the affected optional compatibility selection case passed using a private
binding overlay. The clerk milestone was published on `main` at
`5971c1c6d41d3026faec5a6c37f77cc96aadb22b`. Full checks passed before and
after publication: 2,406 release tests and two installed-runtime tests, with
39 intentional ignores. The post-publication receipt SHA-256 is
`fa6c43f9ee678ee7de412521e409764025882ccda90801ff0e6a2cfc4d7cc2c5`.

Current-only inventory timing measured three 2 KiB sources at 27.252 microseconds
median batch mean and one full 1 MiB hash at 404.523 microseconds on a warm,
unisolated host. See [native clerk](docs/native-clerk.md) for workloads,
measurement limits, source bounds, trust boundaries and executable hash.

## Graph evidence and cache hardening

The shared native graph store now admits read-only SQLite databases through
regular-file, sidecar and ancestor checks, bounded schema/value limits and a
nested execution budget. Source hashes and UTF-8 snapshots use one confined
reader with bounded streaming and descriptor/path/root identity checks.
Missing, invalid or conflicting hashes cannot produce verified indexed rows.
External absolute/relative aliases participate in the same evidence check;
conflicting dependency-coverage flags broaden test selection. Missing coverage
also broadens selection unless a valid explicit legacy inventory supplies it.

The native CLI validates graph rows before following edges or admitting cache
dependencies. Cache hits require matching generation, requested source, bounds
and dependency hashes. Stale and unverified projections remain misses. Cache
publication uses private exclusive staging, validates destination identity,
syncs the renamed entry before pruning, preserves unrelated files and reports
partial publication failures explicitly. Textual fallback callers retain their
`calls (syntactic)` provenance. Current AST symbols remain available when
indexed evidence is rejected.

These checks establish evidence for bounded returned rows, not global graph
completeness or an OS sandbox against same-UID writers. SQLite VM deadlines do
not bound filesystem I/O. At this milestone the Python skeleton/CLI adapter
remained; the replacement below retires it without changing a shared installation.
See [graph context](docs/graph-context.md) for limits and trust boundaries, and
the native `graph_context` benchmark for reproducible source-read and graph
projection workloads.

## Native file context and adapter retirement

`forge code context` now provides the integrated Python/Rust signature, graph
relationship and Markdown/JSON surface without an interpreter or `rg` subprocess.
Tree-sitter byte spans replace Python AST unparsing and the Rust projector's
repeated source-prefix scans. Python retains signature spelling, decorators,
annotations and documentation; selected classes include their methods. Rust
retains signature/attribute text and reconstructs scope wrappers. Both formats
are explicitly versioned context data, with bounded admission and output.

Requested source snapshots use the confined graph reader, recheck their content
hash after projection and relationships, and support an expected-hash guard.
There is no skeleton cache. The separate graph projection cache still requires
verified source evidence. Syntactic discovery uses native authored ignore rules,
skips hidden/ignored/symlink entries, and reports textual provenance separately
from indexed graph status. Visible Python FIFOs and unsafe requested source or
ignore files fail explicitly. Its five-second budget is cooperative, not a
filesystem I/O timeout; see [graph context](docs/graph-context.md) for all limits.

The superseded `src/conductor/graph_context.py`, its dedicated compatibility
target and target mappings are deleted. Shared graph/test-selection bindings
remain for their existing consumers. This leaves 128 Python source files under
`src/`; the complete native migration remains active.

Focused validation passed 66 graph-core tests, 18 native graph integration
tests, seven CLI tests with an empty executable search path, and 33 adapter
discovery checks. Eleven remaining graph-selection compatibility tests passed
through a private binding overlay; they are separate from the native runtime
gate. Both native crates passed strict all-target Clippy. CLI tests cover
same-size/same-mtime edits, old-hash rejection, graph freshness, unsafe source
types, bounded escaped output and source immutability. A real-checkout smoke
test also passed with no executable search path.

Current-only release timings measured 256-function Python and Rust projection
at 1.719 ms and 1.192 ms median batch means, respectively. A 32-file native
syntactic scan measured 298.137 microseconds. The benchmark records hashes,
workload sizes, included operations and limits in [graph context](docs/graph-context.md);
these are costs, not a measured speedup against the deleted adapter.

Native `forge candidate execute` now prepares or runs one external policy check
using a separate operator grants file. Default operation validates and reports
both stages without launching them; `--execute` runs a version probe followed
by the analyzer only when version evidence is usable. Every stage has its own
candidate snapshot, private detached Git metadata and index, empty executable
search path, filesystem/network sandbox and newly owned delegated cgroup.
Neither source refs nor the source index are rewritten. Candidate policy cannot
expand operator capabilities. This is an execution result, not a full review,
an attestation, or approval to land.

The resource owner survives client cancellation and owns launch failures that
occur after a child exists. It observes both pipes through bounded readiness
work, kills the cgroup before reaping the process group, and removes the private
workspace only after cleanup succeeds. Otherwise it retains recovery paths and
returns failure. CPU time, virtual address space, cgroup memory, CPU quota,
process count, wall time and Unicode output tails remain separate limits. Both
version and analyzer stages obey operator CPU/address-space ceilings. SIGINT
and SIGTERM request cleanup before the CLI runtime exits. Runtime destruction,
supervisor SIGKILL and uninterruptible kernel I/O remain outside this guarantee.

Focused validation exercised 11 delegated-cgroup supervisor cases and 10 CLI
cases, including detached descendants, flooding, dropped clients, finalizer
errors, real Git index behavior, SIGTERM, source preservation and observed
version-process rlimits. Existing process probes retain their strict aggregate
byte cap; candidate capture instead retains separate bounded Unicode tails
while continuing to drain. Details and operator configuration are in
[native candidate review](docs/native-candidate-review.md).

Native `forge candidate execute-batch` runs selected regular checks under a
separate aggregate operator budget. In addition to external commands, the native
batch currently evaluates `candidate-integrity`, `dependency-integrity`,
`secret-scan`, `native-source`, and `cargo-audit` in-process. Candidate integrity requires
explicit `--surface` when scheduled,
even in dry-run; dry-run validates and prepares its private snapshot without
evaluating the check. These builtins require source grants and a batch budget,
but no analyzer program grants. Results remain in policy order and identify
`forge-builtin/1`; integrity reports its Unicode 15.0.0 compatibility version.
Secret scanning preserves explicit legacy skip cases; native-source treats
unexpected missing regular files as incomplete, with sanitized ledger findings
appended after core matches. Scan reads and findings have finite byte/count caps;
their host-process memory and elapsed time remain outside per-stage cgroup limits.
Other scheduled unsupported checks remain visibly unexecuted and make the
selected scope incomplete. Execution is fresh, without result-cache reuse. The
independent batch owner cancels queued work and joins builtin workers through
bounded evaluation and snapshot cleanup; a worker may finish its current bounded
CPU operation before cancellation returns. The same runtime/SIGKILL/kernel-I/O
boundary applies. This adds no approval or complete-review authority. Operator
contracts are in
[native candidate review](docs/native-candidate-review.md#execute-a-selected-batch).
Focused scan validation passed 590 release cases, including eight scan CLI
cases and four private reader regressions. These exercise scope, redaction,
strict decoding, symlinks, incomplete findings, resource refusal, cancellation,
size drift, retained read credit, multiple admitted checks and source preservation.
Exact JSON input boundaries are checked against serde serialization for ASCII
controls, Unicode, empty inputs and multiple files. Component benchmarks record
their measured scopes and source bindings in
[performance](docs/performance.md#native-scan-admission-and-line-lookup).

Standalone receipt sealing and Human/JSON/SARIF/JUnit export are native. Export
verifies self-consistency, labels execution unverified and publishes complete
bounded output atomically through pinned parent directories. It does not infer
review completeness from imported records. See the
[report contract](docs/native-candidate-review.md#export-a-recorded-receipt).

Cppcheck now runs directly through native command supervision. Shared Rust
suffix selection and confined file-list preflight replace its deleted Python
wrapper, preserving current-path selection and refusing ambiguous or missing
inputs. The [file-input contract](docs/native-candidate-review.md#native-file-selection-and-cppcheck-inputs)
defines the remaining legacy metadata bridge and explicit no-file skip.

Full review orchestration, cache integration, evidence and waiver verification,
test-engine migration, review-integrated reporting and attestation remain. The existing Python
command runner still has these callers and has not been deleted prematurely;
117 Python source files remain under `src/`. The native migration goal stays
active until those consumers are replaced and their interpreted code is removed.

Cargo audit evaluates pinned offline evidence directly with native RustSec,
including exact crates.io checksum and yanked-state coverage. Its Python command
wrapper is deleted; the remaining legacy dispatcher forwards metadata to the
same evaluator. Missing, stale or inconsistent evidence fails. See the
[audit contract](docs/native-cargo-audit.md).

Native [Cargo lint](docs/native-cargo-lint.md) plans bounded workspace/borrowed
scope and runs fmt/clippy through granted stages with private runtime directories.
Its Python wrapper, ambient shim and compatibility target are deleted.
Native [clang-format](docs/native-clang-format.md) and [clang-tidy](docs/native-clang-tidy.md)
use captured ranges/styles, explicit ELFs, pinned databases and typed path rebasing.
Their wrappers and shared diff consumers are deleted; see [native scans](docs/native-candidate-scan.md) and [candidate analysis](docs/native-candidate-analysis.md).

## Executable identity and reusable builds

The Forge workspace separates the `forge` library from the `forge-cli` binary
package. Both are default members; only the CLI runs the Git/source stamp build
script. Source identity still covers all native libraries, the shared lockfile,
CLI manifest and stamp script. Package versions and installed identity remain
checked. Integration tests select an explicit executable at runtime, avoiding a
compile-time dependency on each Git revision.

Automated checks and previews hand off the executable from a successful isolated
Cargo install. This remains correct when Cargo target configuration changes its
compiler output directory. Preview regressions exercise split packages, a host
triple, unchanged-cache reuse, same-length source edits and an obsolete binary.
The standalone test command uses its own installation prefix; optional verified
fixtures may compile on demand. See [native local checks](docs/local-checks.md).
