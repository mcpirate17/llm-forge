# Native tool orchestration architecture

Status snapshot: 2026-11-02. This document describes the current architecture,
its remaining boundaries, and the evidence presently available. It replaces the
chronological implementation log; detailed contracts and historical receipts
belong in the linked documents.

The native migration remains active. The current live `src/` inventory
contains 83 Python source files, including fixtures under
`src/conductor/testdata/`; this is not a count of remaining production cores.
The integration baseline before the native review batch was `838474b`;
validation scope is summarized under Evidence below.

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
separately operated inference server. Refresh can reuse a prior vector only when
the prior generation has a verified refresh origin and matching canonical source
root and embedding space, and the source path, file hash, byte span and chunk text
match exactly. Changed, new or unmatched chunks are embedded anew;
`force_reembed` disables reuse for that refresh.

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

`make gate` now invokes `forge candidate gate` with explicit review grants and
pinned Git/build-profile/mutation-runner inputs. Native code exports the selected
candidate, runs the preflight/corpus/cost phases and coordinates the fresh CI
review; live import closure deliberately observes the live index/worktree.
Preflight-only or skipped phases do not establish a complete review. See
[native gate retirement](docs/native-gate-retirement.md).

Current candidate test evidence is bounded to the named candidate and selected
profile. A passing local report is not merge authorization and does not show
that an external analyzer examined every relevant input. Native Cargo audit,
Cargo lint, clang-format, clang-tidy, cppcheck and scan capabilities have their
own grants, scope and evidence contracts; link those contracts instead of
repeating their detailed limits here.

### Native test, benchmark and corpus orchestration

The native `graph-test-select` command plans advisory affected tests and can run
selected pytest, Cargo and contract tests under native supervision. Python remains
an explicit compatibility guest where selected tests require it; selected contract
and pytest stages share freshly provisioned artifacts. This selection is not a
governance PASS or proof of complete test coverage. The native candidate benchmark
prepares captured fixtures and, only with explicit execution, measures the real
review coordinator; optional Python equivalence guests remain external inputs.
Its wait4 RSS field is the largest individual waited-process peak, not aggregate
simultaneous tree memory. The mutation-corpus auditor checks live or explicitly
captured corpus state using operator-pinned Git, Git build-profile and runner
inputs. The profile's system-attributes declaration is a trusted provisioning
assertion, not binary discovery. Corpus reproducibility status is not a mutation
campaign receipt or review approval. See [native graph test selection](docs/native-graph-test-selection.md),
[native review benchmark](docs/native-review-benchmark.md), and
[native mutation corpus audit](docs/native-mutation-corpus-audit.md).

### Native workspace policy and explicit fixes

`forge branch-policy` owns name validation, project integration-line resolution,
binding persistence, local audit/status and full read-only workspace exposure.
The branch status/audit Make targets call it directly. Bind/unbind update only
locked common-Git-dir metadata; exposure provides PR/claim/commit/dirty/worktree
rows and a conservative cleanup preview. It does not mutate Git refs or remove
worktrees. Optional GitHub observations use the operator's `gh`; missing or
failed observations remain visible. `forge workspace-hygiene` owns the full
report, including validated in-process claims, registry fragments, import
closure, leases and checkout drift. Its Python implementation has been deleted.
See [native branch policy](docs/native-branch-policy.md) and
[native workspace hygiene](docs/native-workspace-hygiene.md).

`forge checkout-sync` owns offline preview, staged/unstaged reconciliation,
guarded index/worktree/ref publication and conditional journal recovery. Preview
shares planning with execution and creates no Git objects or snapshots. Applying
transactions require an explicit private recovery parent outside the checkout
and Git directories. Git lock reacquisition, inode/content observations and
durable phase records detect intervention; affected path writers must honor the
cooperative lease. The native CLI and Make target replace the deleted Python
actor. See [native checkout sync](docs/native-checkout-sync.md).

`forge worktree-runtime` repairs console launchers in two explicitly named,
existing environments without creating worktrees or launching an interpreter.
Native copy-on-write publication preserves hardlinked source files, destination
modes and script bodies. Bounded capability-relative access and cooperative
directory locks reject unsafe paths and retain detected raced entries; the
operation is not a batch transaction or hostile same-UID isolation. The Python
adapter and its compatibility test are deleted. See
[native runtime relocation](docs/native-runtime-relocation.md).

`make governance-fix` invokes `forge candidate fix` with a JSON paths file,
operator grants and a SHA-pinned native Ruff executable. The default CLI path
only admits inputs; explicit execution supervises Ruff 0.16.5 stdin filters with
read-only source access, bound hierarchical configuration and exact bounded
output. All filters must succeed before native validated edits are published.
Publication uses one cooperating-writer lock, retained Git recovery blobs and
sequential renames with guarded rollback; it is not filesystem-wide atomic
visibility. The displaced Python fix subcommand is removed. The Python review
entry point and execution graph are also retired; ownership, identity, Git/model
and Vulture helpers still serve other interpreted callers. See
[native candidate fixes](docs/native-candidate-fix.md).

Targeted full review captures each enabled shard's private recorder database after
verified child cleanup. The nearest captured configuration governs each test;
all shards use candidate-root source paths. Native evaluation matches source
blobs and intersects missed functions across fixture/test windows, with bounded
SQLite queries and decompression. Missing or disabled recording is explicitly
not evaluated. See [review retirement](docs/native-review-cli-retirement.md).

### Commit recovery

The native commit wrapper captures a recovery ref before invoking Git and its
normal hooks. The captured trees preserve staged index content and admitted
worktree content as of snapshot capture. Snapshot capture itself does not modify
the index bytes. The subsequent real Git command and hooks can update index
metadata or files; the wrapper does not roll back those side effects. Fresh native
attestation runs a fresh complete review through `forge candidate attest-message`;
host hook configuration remains operator managed.
See [native commit recovery](docs/native-commit-recovery.md).

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

Focused validation of the native gate, branch/exposure and explicit Ruff fix
batch passed 610 Forge library tests, 552 conductor source-analysis library
tests, 11 branch-policy CLI cases, seven fix admission cases, and ten explicitly
provisioned fix execution cases including native Ruff 0.16.5. Two actual Cargo
fixtures passed source/build-input and Git-ref freshness checks. These are
overlapping working-tree selections, not full-gate or clean-host evidence. The
eight opt-in gate execution cases still require separate validation with a clean,
source-matching executable. Committed full-gate evidence remains in the generated
check receipts, not in these test counts.

Mutation review defaults to strict behavior. Portable grandfathering requires an
external operator grant pinning the repository commit, tree, inventory path and
inventory SHA-256. Candidate policy cannot authorize its own exemptions.
A positive external-invariant waiver still requires a genuine automatic receipt
bound to a manifest declaring that invariant. That receipt evidence remains
outstanding.

Performance data must identify binary/source hashes, runtime, hardware,
workload, warmup, repetitions and included operations. Values in this document
are bounded examples from linked contracts, not universal guarantees. Every
microsecond or nanosecond value excludes process/model costs unless the
measurement explicitly includes them. No unconditional microsecond whole-task
latency claim is supported.

## Remaining migration

The native migration remains open. The live source inventory is 83 Python
files under `src/`, including testdata fixtures; that count says nothing by
itself about reachability,
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

- [Native gate retirement](docs/native-gate-retirement.md)
- [Native branch policy and exposure](docs/native-branch-policy.md)
- [Explicit native candidate fixes](docs/native-candidate-fix.md)
- [Native graph test selection](docs/native-graph-test-selection.md)
- [Native review benchmark](docs/native-review-benchmark.md)
- [Native mutation corpus audit](docs/native-mutation-corpus-audit.md)
- [Native complexity analysis and baseline ratchet](docs/native-complexity-cli.md)
- [Native mutation receipt target-tree verification](docs/native-mutation-receipt-cli-migration.md)
- [Measured real-code retrieval at 6630b37](docs/native-code-retrieval-measurement.md)

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
