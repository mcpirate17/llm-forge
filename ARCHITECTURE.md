# Native tool orchestration architecture

Status snapshot: 2026-11-01. This document describes the current architecture,
its remaining boundaries, and the evidence presently available. It replaces the
chronological implementation log; detailed contracts and historical receipts
belong in the linked documents.

The native migration remains active. The current live `src/` inventory
contains 113 Python source files. The integration baseline before the native
review batch was `838474b`; validation scope is summarized under Evidence below.

## Mission and operating rules

Move the shipped core and its compatibility entry points to native Rust,
reusing existing Forge, conductor-native, and slop-core algorithms rather than
creating duplicate harnesses. Retire interpreted implementations only after
behavior, entry points, and required tests have moved. Keep operator policy
separate from model-generated arguments: structured output can specify a
request, but cannot grant authority or expand capabilities.

Configuration and environment failures are explicit. Candidate and source
identity, ownership, resource bounds, and evidence provenance are checked at
the boundary where they are consumed. A digest establishes byte consistency,
not who produced or approved those bytes. Keep correctness, capability claims,
performance measurements, and migration status distinct.

New core logic is Rust. C/C++ or GPU acceleration requires a measured native
workload. Do not infer end-to-end latency from an in-process benchmark.
Compare realistic workloads and matched controls before claiming improvement.

## Original architecture requirements

The original acceptance requirements remain the architectural north star.
Implementation coverage below is a current snapshot, not a claim that each
requirement has been completed across every consumer.

### A. Structured-call boundary

Provide a reusable Rust schema compiler and validator, deterministic GBNF
grammar generation, closed tool registration, and bounded JSONL dispatch. Reject
unsupported schema features during registration. Any fenced-JSON repair must
be explicit, bounded, followed by validation, and visible in the result.

Success-path validation should borrow caller input and use bounded state;
registration, transport, logging, and grammar generation have separate costs.
Acceptance includes adversarial syntax/schema coverage, allocation behavior on
success and failure, concurrent use, grammar acceptance, interpreter-free CLI
execution, and reproducible release measurements. Validation remains mandatory
even when grammar-constrained generation is used.

### B. Native code intelligence, graph, patching and search

Build on the shared language registry and existing graph/index structures.
Expose native AST definitions, signatures, references and scope queries with
source hashes and explicit unresolved edges. Construct dependency graphs from
those relationships, keeping syntactic resolution distinct from semantic
understanding. Stage patches in memory, bind them to expected source hashes,
support deterministic and explicitly bounded fuzzy search/replace, validate
changed syntax through the AST, reject ambiguous or stale matches, and
atomically replace only the exact approved files.

The search path combines source-bound mmap vector storage, HNSW candidate
search, BM25 lexical retrieval and exact SIMD cosine/reranking controls. Bind
embedding vectors to model recipe, dimension, normalization and source hashes.
Publish immutable index generations and preserve readers during refresh.
Measure recall@k against exact search, RSS and latency on realistic corpora;
synthetic fixtures establish mechanics, not relevance. Search must distinguish
an indexed snapshot from complete current-workspace coverage.

Acceptance includes stale-source, symlink/traversal, ambiguity, syntax
regression, crash, unchanged-file and formatting-preservation cases, plus
corruption, deletion freshness, concurrent refresh and recall controls.

### C. Interactive execution and isolation

Extend durable task ownership with persistent PTY input/output/resize,
backpressure, disconnect/reconnect, cancellation and process-group cleanup.
Use nonblocking descriptors and a single Tokio reactor. Add explicit Linux
filesystem isolation, Landlock, seccomp and delegated cgroup limits. A
preflight must reject unsupported isolation before payload execution; no
unconfined fallback is permitted.

Acceptance includes terminal/job-control, output floods, orphan cleanup and
escape-negative cases. Git-aware rollback may restore only task-owned paths
whose hashes still match, using retained source identities; it must never reset
a shared checkout. Git-ignored artifacts need an explicit retention manifest.

### D. Model-aware context virtualization

Add model-fingerprinted BPE and context compaction to native context
projection. Use AST stubs/folding for code and bounded log and diff compression
for diagnostics and patches. Preserve protected instructions and actionable
errors, and persist recoverable originals before reducing content. Acceptance
includes tokenizer parity, nesting and Unicode, exact token budgets, recovery
round trips, and no lost actionable diagnostics on captured fixtures.

The counted text must be stated precisely. Chat templates, role framing,
multimodal inputs and generation reserves are excluded unless included in the
input or separately accounted for.

### E. Measured hardware acceleration

Use hardware acceleration only when tensor work embedded in a real Forge
workload is measured to warrant it. First measure end-to-end dispatch,
cold/warm search and concurrent task throughput, including transfer and
orchestration costs. Add Triton,
libtorch/PyTorch or CUDA kernels only when tensor computation is the measured
bottleneck. Compare matched inputs, numerical tolerance, device transfers and
VRAM accounting against native CPU and existing implementations. No GPU kernel
or speedup is implied by an existing native binary, CPU SIMD path, embedding
client, or synthetic correctness fixture.

Every requirement needs correctness, integration and workload-specific stress
evidence before another milestone depends on it.

## Current architecture at a glance

| Layer | Main implementation | Boundary |
| --- | --- | --- |
| CLI and operator workflows | `native/forge-cli` entry point and `native/forge` library | Native command coverage does not prove every host adapter is retired. |
| Shared algorithms and policy | `native/conductor-native` | Compatibility bindings and consumers remain in migration. |
| Test indexing and numerical equivalence | `native/slop-core` | Optional Python/Torch guests are separate from the default native core. |
| Search and persistence | Native graph, vector and SQLite stores | Indexes are source-bound snapshots with bounded freshness checks. |
| Runtime and process ownership | Tokio actors, sandbox, cgroup and process supervisors | Linux support and operator delegation are required; kernel and hostile same-UID boundaries remain. |
| Context | Rust BPE, syntax projections and recovery artifacts | Operator tokenizer identity and supplied text define counting scope. |
| Host integration | Hooks, providers, packaging and project tooling | Interpreted entry points remain until callers and install paths move. |

The CLI is not itself the authority boundary. Operator-owned policy, grants,
explicit surface/profile selection, source identity and resource admission
supply that boundary. Any report must state its scope and must not imply
approval, promotion, or downstream model capability.

## Shipped capability

### Structured output and dispatch

Forge has a native schema compiler/validator, deterministic grammar generation,
closed registry and bounded stdio harness. Tool requests are validated before
dispatch. Repair is an explicit fenced-JSON path and repaired content is
revalidated. The native persistent harness connects validation to registered
handlers, PTY actors and bounded tool responses. See
[structured output](docs/structured-output.md) and [native harness](docs/native-harness.md).

Measured structured-call figures are process-local costs only. A current-only
Rust 1.98 release benchmark on an unisolated host measured median batch means
of 153.85 ns for escaped-argument validation, 98.06 ns for a rejected numeric
range, 317.68 ns for a registered call, and 443.80 ns for 256 registered
tools; registration took 23.83 µs. Those figures exclude transport, process
startup and model inference. They are not a historical speedup claim.

### Code analysis, graph and patch boundaries

Native Tree-sitter analysis supports the shared language registry and returns
bounded source-bound structures. Graph indexing and context queries use
explicit snapshots, source hashes, freshness checks and bounded reads. Native
file context discovery applies authored ignore rules, refuses unsafe source
types, and distinguishes textual discovery from indexed graph status. The
native patch and edit paths use exact-source identity and bounded validation;
they do not turn fuzzy or unresolved matches into authorization.

Current code-index search combines FTS5 BM25 with exact cosine or HNSW
candidates, exact reranking and runtime-detected SIMD f64 scoring. It validates
source hashes at read time, drops stale candidates, atomically publishes
immutable generations, and retains reader leases. New files are absent until refresh. A published index is an
identified snapshot, not complete live-workspace coverage. The embedding client
pins an operator-selected model artifact and recipe, but does not attest to a
separately operated inference server.

A current measured synthetic retrieval workload used 4,096 Rust-file chunks,
64-dimensional clustered vectors and 64 perturbed queries. Median query times
were 80.23 µs for BM25, 118.89 µs for exact hybrid, and 149.06 µs for HNSW
hybrid; HNSW matched exact top ten on these synthetic queries but was slower
at this size. This is not real-code relevance evidence and does not predict
larger-corpus behavior. Exact remains the default. See [native code search](docs/native-code-search.md),
[native graph](docs/native-graph.md), [graph context](docs/graph-context.md), and
[native code tools](docs/native-code-tools.md).

### Execution, PTY and isolation

Native runtime components provide PTY sessions, replay buffers, process
supervision, filesystem/syscall confinement and owned cgroup v2 limits. The
reactor uses readiness notifications rather than periodic child polling. The
harness owns sessions independently from clients, bounds queues and replay,
and retains explicit operation receipts. Candidate check execution gives each
stage a private snapshot and runtime workspace, empty child PATH, bounded
filesystem/network access, rlimits and a delegated cgroup. Isolated tasks
publish selected artifact hashes only after verified cleanup; they do not
write results back into the source checkout.

Isolation requires supported Linux facilities and operator-provisioned
capabilities. There is no fallback to unconfined execution. Cgroups must be
pre-delegated with controllers enabled; Forge does not alter parent limits.
Metadata visibility, kernel vulnerabilities, abrupt supervisor SIGKILL,
uninterruptible I/O and hostile unconfined same-UID processes remain outside
the guarantee. GPU memory is not isolated. See [native execution](docs/native-execution.md)
and [native harness](docs/native-harness.md).

### Candidate review and governance workflows

The native candidate CLI resolves exact Git candidates, materializes bounded
snapshots, classifies candidate policy, plans checks, executes granted checks,
assesses supplied results, and seals/export reports. The current
`candidate review` coordinator resolves the complete active profile and
collects native-owned engine-integrity, attestation, test selection, source,
ownership, mutation, equivalence, regular-check and targeted-test observations
as scheduled. It requires explicit surface selection and `--execute` for
execution. Missing results, errors, cancellation, drift and cleanup failures
block completion. `approval` remains false. Receipt checksums and installed
runner byte pins detect inconsistency; they do not authenticate historical
execution, approval, or external-tool completeness. The standalone supplied-
results assessment is not a full review. See
[native candidate review](docs/native-candidate-review.md).

Current candidate test evidence is bounded to the named candidate and selected
profile. A passing local report is not merge authorization and does not show
that an external analyzer examined every relevant input. Native Cargo audit,
Cargo lint, clang-format, clang-tidy, cppcheck and scan capabilities have their
own grants, scope and evidence contracts; link those contracts instead of
repeating their detailed limits here.

### Context and recovery

Native context commands count with an operator-supplied, hash-pinned BPE
artifact, compact ordered entries using syntax-aware projections, and persist
immutable recovery artifacts before reducing content. Protected entries and
actionable diagnostics are retained; overflow is explicit. Counting applies to
the exact supplied text through the pinned tokenizer pipeline; external chat
templates and multimodal framing are outside scope unless rendered into that
text. Recovery hashes establish integrity, not protection against a trusted
same-UID writer. See [native context](docs/native-context.md).

On a shared Ryzen 9950X3D host, Rust 1.98 and a pinned GPT-2 tokenizer, an
uncached 4 KiB count measured 694.2 µs median and a warm hash/count hit measured
1.50 µs. A 1 MiB uncached count measured 263.1 ms; a warm hit measured 372.7
µs. These results show the input-size and cache distinction; they are not
whole-request latency or downstream quality measurements.

### Lifecycle, providers and project services

Session lifecycle hooks compose native policy, graph gating, bounded mailbox
preview and one-time handoff activation. Background graph refresh uses a
native binary and durable worker ownership. Provider configuration and startup
have native read-only planning and bounded local adapters, but installation,
bootstrap and client-specific compatibility surfaces still require migration.
Native notes, claims, project configuration, local clerk and telemetry
commands cover additional operator-facing workflows. Each document defines
which operations mutate state, which remain read-only, and which legacy entry
points still exist. See [native lifecycle](docs/native-lifecycle.md),
[native providers](docs/native-providers.md), [native project config](docs/native-project-config.md),
[native claims](docs/native-claims.md), [native notes](docs/native-notes-index.md),
and [native clerk](docs/native-clerk.md).

## Evidence and what it establishes

Evidence has three distinct levels:

1. **Focused tests and fixtures** exercise specific semantics and failure
   boundaries. They do not establish full integration or clean-host install.
2. **Workspace suites and real owned Stage tests** exercise current native
   composition at the reported working-tree revision. They do not establish a
   committed source identity or full local-check receipt.
3. **Committed full-gate receipts and clean-host sweeps** bind evidence to an
   exact tree and declared environment. Only these support claims about the
   corresponding committed release gate; they still do not imply unmeasured
   model quality, external service behavior or production deployment.

Before committing the native review batch, `cargo test --release` passed
2,283 Forge tests with 104 environment-dependent cases ignored. Separate
explicit runs passed 13 workspace/sandbox cases, six review CLI cases, and two
Torch Stage cases. These exercise captured imports, missing `/proc`, Torch
shadowing, resource cleanup, and fresh receipt/report publication. The conductor
library passed 515 default and 555 optional-binding tests; Slop passed 187
default and 201 optional-binding tests. Another 21 mutation-value compatibility
cases passed using the freshly built external conductor extension. These
overlapping selections are not additive totals. Full-gate receipts must still
be bound to the committed tree by `forge check --all` and `forge verify --require-all`.

The invariant leaf tests do not establish a positive full mutation-collector
run. Legacy grandfathering still requires fixed historical LLM repository
objects, and an external-invariant waiver needs a genuine automatic receipt
bound to a manifest declaring that invariant. Portable clean-host baseline
configuration and complete positive collector evidence remain unfinished.

Performance data must identify binary/source hashes, runtime, hardware,
workload, warmup, repetitions and included operations. Values in this document
are bounded examples from linked contracts, not universal guarantees. Every
microsecond or nanosecond value excludes process/model costs unless the
measurement explicitly includes them. No unconditional microsecond whole-task
latency claim is supported.

## Remaining migration

The native migration remains open. The live source inventory is 113 Python
files under `src/`; that count says nothing by itself about reachability,
behavioral coverage or runtime usage. Remaining work is tracked by live
callers and installation boundaries, not a stale count of old milestones.
Requirements A–E remain system-level obligations: native components and focused
suites cover portions of them, while repository-wide callers, install paths and
clean-host operation still require migration and evidence. In particular, the
hardware-acceleration requirement is conditional on measured embedded workloads;
no unconditional GPU implementation is a completion criterion.

The remaining program is:

1. **Retire interpreted callers.** Find each reachable Python implementation,
   command wrapper, hook fallback, provider/bootstrap adapter and package
   surface. Move its contract to the native library/CLI and migrate callers
   before deleting the implementation. Compatibility shims are transitional
   migration aids and must also be retired; the final shipped core has zero
   interpreted dependencies.
2. **Close governance and mutation surfaces.** Inventory remaining mutation,
   waiver, audit, policy, package and repository operations. Reuse native
   algorithms; expose explicit bounds, provenance, ownership and failure
   results. Preserve mutation-engine rules and never treat a receipt hash as
   approval or execution proof.
3. **Finish packaging and clean-host paths.** Remove interpreter discovery
   from default native builds; declare shipped dependencies in package
   metadata; replace Python bootstrap/install/check dependencies only after
   native replacements are exercised from a clean install. Keep test-only
   dependencies in test configuration.
4. **Prove integration on an exact tree.** Run the required full local check
   and verification after the final source changes, bind receipts to the
   committed tree and remote main, resolve required review, and complete the
   authorized direct-to-main delivery. Current working-tree counts do not
   satisfy this step.
5. **Reconcile documentation.** Keep this file architectural and current.
   Preserve detailed limits, commands, measurements and historical evidence in
   focused contract documents and the migration-history archive. Remove stale
   remaining-count claims when their inventory is no longer current.

No chronological implementation diary is normative. Historical commit-by-
commit results remain available in [native migration history](docs/native-migration-history.md)
and [native migration history archive](docs/native-migration-history-archive.md).

## Detailed contracts

- [Native runtime installation and compatibility distribution](docs/native-install-packaging.md)
- [Local checks and verification receipts](docs/local-checks.md)
- [Native execution boundaries](docs/native-execution.md)
- [Native harness and MCP service](docs/native-harness.md)
- [Native candidate review, execution, and receipts](docs/native-candidate-review.md)
- [Native candidate flow map](docs/native-candidate-flow-contract-migration.md)
- [Native candidate mutation map](docs/native-candidate-mutation-contract-migration.md)
- [Native Cargo audit](docs/native-cargo-audit.md) and [Cargo lint](docs/native-cargo-lint.md)
- [Native scans](docs/native-candidate-scan.md), [candidate analysis](docs/native-candidate-analysis.md),
  [clang-format](docs/native-clang-format.md), and [clang-tidy](docs/native-clang-tidy.md)
- [Native code search](docs/native-code-search.md) and [vector index](docs/native-vector-index.md)
- [Native graph behavior](docs/native-graph.md) and [graph context](docs/graph-context.md)
- [Native context virtualization](docs/native-context.md)
- [Native lifecycle](docs/native-lifecycle.md), [providers](docs/native-providers.md), and
  [MCP setup](docs/native-mcp-setup.md)
- [Native analysis](docs/native-analysis.md), [dependencies](docs/native-dependencies.md),
  and [project configuration](docs/native-project-config.md)
- [Performance methodology and receipts](docs/performance.md)

## Change and evidence discipline

Update this architecture summary when a capability boundary, migration status,
or evidence state changes. Do not append another chronological milestone here.
For a focused implementation, update its contract and record the exact evidence
there. For repository-wide completion, use a committed, source-bound full gate
and identify the remaining live Python entry points before making a completion
claim.
