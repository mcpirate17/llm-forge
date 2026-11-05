# Measuring performance

## Current program evidence — 2026-11-04

This is the canonical performance and effectiveness report; current mission status
and ordered continuation are in [the roadmap](roadmap.md#current-mission-checkpoint--2026-11-04).
Source baseline is `9117c2fb6436251d0de50038db85ef7fbb81e4b3`; the repair landed in
[PR #185](https://github.com/mcpirate17/llm-forge/pull/185). Pre-change observations,
repaired working-tree measurements and matched binary comparisons are separated
below. They do not certify the eventual PR head or installed runtime. Test and
publication status are in the current [engineering status](roadmap.md).

### Installed native identity — 2026-11-05

The [machine-generated observations](../research/performance/forge-program-20261105-installed-native-identity.json)
record a real automatically generated cargo-mutants 27.1.0 campaign on a tiny
external-host fixture with no vendored Forge sources or Python on the child PATH.
Clean build commit: `3f65b1ee03ed493c1402c5c1e663a30cdd20f831`; executable SHA-256:
`2e63e55b2d2303db8a4014770752790a83b6dca018bd93cb5b23a8930308111a`.
The operator pin came from that binary's identity command before receipt generation.

The engine killed two mutants, one in each selected source, and removed its snapshot.
Both live and commit corpus audits were `CLEAN` without a Python bundle or `uv.lock`.
Candidate receipt verification was `PASS`, with one covered test and no missing
tests, after adding the separately required matching candidate lock. Fourteen
negative cases refused missing/mismatched pins and locks, partial inventories or
source drift. All 22 final command attempts completed; none was censored or unstarted.

**Full candidate review did not pass.** The automatic Cargo receipt lacks complete
per-test attribution and admitted test-value evidence, tracked in
[#190](https://github.com/mcpirate17/llm-forge/issues/190). The fixture also lacks CI
commit attestations and Agent trailers. The original experiment retains
`all_expectations_met: false` and `INCOMPLETE_OR_REFUSED`; identity acceptance does
not change those outcomes. No debt baseline, value waiver, manually authored mutant
or generated receipt/manifest edit was used. Earlier attempts are retained: one
refused an uncommitted fixture test before engine execution (18 commands unstarted);
another completed the corpus cases but its seven review invocations lacked a
required surface and did not reach evidence collection.

The artifact includes fixture text, exact commands, tool/build pins, report hashes,
failure details and unchanged-input/cleanup checks. Raw logs and the original
automatic receipt remain under the recorded private paths. Reproduction requires a
new automatic campaign with the selected clean binary and independently selected
pin; new hashes are new evidence. Private Python only orchestrated the experiment;
Forge and engine processes used a native-only executable search path. Recorded
single-command durations include driver wait/capture overhead and are **not latency
benchmarks**. This demonstrates cooperative consistency on one local synthetic host,
not producer attestation, deployed-client adoption, agent effectiveness or a speedup.

### Fresh current-only CLI baselines

Both workloads completed 31 successful samples after three warmups, with a
10-second per-sample timeout, on the same shared AMD Ryzen 9 9950X3D host.
Each call launches a new process; the filesystem was warm. This is not a cold-host,
isolated-host or matched before/after experiment. The common executable SHA-256 is
`5839e685955891abdd5d888fd039212b2ae456f5530f7a8527f7b060bf23ba36`.

| Workload | Wall p50 / p95, ms | CPU p50, ms | Maximum child RSS, bytes | Retained receipt |
| --- | ---: | ---: | ---: | --- |
| CLI `--help` | 1.649971 / 1.832543 | 1.214 | 11,333,632 | [cli-help-baseline.json](../research/performance/forge-program-20261104-cli-help-baseline.json) |
| Signature context for `relocate_console_scripts`, graph disabled | 2.198525 / 2.371676 | 1.765 | 15,224,832 | [symbol-baseline.json](../research/performance/forge-program-20261104-symbol-baseline.json) |

Receipt seals are respectively
`399008ee5a58a2369b79971dc7c5d927caf1c790c7eb2a52c6a3e7bc0aa29a71` and
`06965a70eee78a057eab806e262b6800fdb85c32be447a99b629cb5376183508`.
These cooperative seals bind recorded content, not cryptographic execution
attestation. Source scopes are deliberately narrow: help binds
`native/forge-cli/Cargo.toml`, `native/forge/src/main.rs` and the executable;
symbol lookup binds `native/forge/src/code_tools.rs` and the executable.
They do not bind every transitive implementation file. The symbol workload digest
also binds the inspected `native/forge/src/worktree_runtime.rs` via `--input`.
Retain the recorded
literal commands, environment/platform/toolchain and workload digests when
interpreting these observations. Broader correctness claims need broader evidence.

Reproduce the declared workloads with new output paths; new binary/source/workload
identities create new evidence, never replacement identities for these results:

```sh
forge performance benchmark \
  --command './native/forge/target/release/forge --help' \
  --source native/forge-cli/Cargo.toml --source native/forge/src/main.rs \
  --source native/forge/target/release/forge \
  --samples 31 --warmups 3 --timeout-seconds 10 \
  --output research/performance/forge-program-cli-help-replay.json
forge performance benchmark \
  --command './native/forge/target/release/forge code --root . context --symbol relocate_console_scripts --no-graph --json -- native/forge/src/worktree_runtime.rs' \
  --source native/forge/src/code_tools.rs --source native/forge/target/release/forge \
  --input native/forge/src/worktree_runtime.rs \
  --samples 31 --warmups 3 --timeout-seconds 10 \
  --output research/performance/forge-program-symbol-replay.json
```

For a broader prospective signature experiment, enumerate affected transitive
source paths as well. A changed receipt scope creates separate evidence and does
not retroactively strengthen this baseline.

### Matched repair comparison

The first post-repair replay retained 31 samples per command:
[help](../research/performance/forge-program-20261104-cli-help-current.json) and
[symbol](../research/performance/forge-program-20261104-symbol-current.json).
Their environment digests differ from the original observations, so the native
comparison refused both pairs. The apparent help p95 increase from 1.833 to
2.122 ms is an unmatched observation, not accepted regression evidence.

A fresh paired experiment used the preserved original binary and repaired binary,
sequentially copied to `.git/forge-program-benchmark/forge`, with identical literal
commands, source path scope, input bytes and a cleared environment containing only
`PATH=/home/tim/.cargo/bin:/usr/bin:/bin`, `HOME=/home/tim`, and `LANG=C.UTF-8`.
Each command completed 101 samples after five warmups, with a ten-second timeout.
Both comparisons pass the existing 10% budget; it was not increased.

| Workload | Wall p50 before / after, ms | Wall p95 before / after, ms | CPU p50 before / after, ms | Max RSS before / after, bytes | Sealed comparison |
| --- | ---: | ---: | ---: | ---: | --- |
| CLI help | 1.557519 / 1.551719 | 1.741850 / 1.681190 | 1.130 / 1.121 | 12,910,592 / 12,468,224 | [help](../research/performance/forge-program-20261104-cli-help-paired-comparison.json) |
| Symbol context | 2.118182 / 2.116422 | 2.227342 / 2.268532 | 1.689 / 1.679 | 16,863,232 / 16,912,384 | [symbol](../research/performance/forge-program-20261104-symbol-paired-comparison.json) |

The comparison files contain both complete sealed receipts; separate copies are
not committed. All 404 measured samples succeeded, with zero failed, censored or
unstarted samples. Scope binds the executed binary, plus the symbol input where
applicable; it does not rebind the old binary to today's source files. Binary
SHA-256 values are `5839e685955891abdd5d888fd039212b2ae456f5530f7a8527f7b060bf23ba36`
and `b87c81950eb293fb60fff90787904866b4cd5df95cab1ce02e90bdd7c2caa35e`.
Both ELF compiler comments report Rust 1.98.0; the receipt's ambient `rustc`
probe reports 1.93.1 and is not a build attestation. The host remains shared,
the filesystem warm and the process newly launched. These observations support
no material regression in these two workloads, not a general speedup or agent
productivity claim. Reproduce using the literal commands and pinned binaries in
the embedded receipts; never edit receipt identities to retrofit a match.

### Prompt-contract behavior and overhead — 2026-11-04

The native `context prompt` implementation exposes literal inspect/suggest/plan/
compare operations through CLI and the existing MCP context worker. It does not
automatically rewrite prompts or infer semantic preservation. Twelve core tests,
four catalog tests and four release-binary CLI/MCP integration tests pass; the
last includes a real custom stdio client, initialized service, discovery, calls,
errors and denied capability. This does not demonstrate coding-client adoption.

An independent reviewer prepared [four held-out cases](../research/performance/forge-program-prompt-eval-cases-20261104.json)
covering UTF-8/CRLF provenance, opaque sections, unknown headings and a contradicted
but literally retained requirement. All four completed and passed, with zero
failed, censored or unstarted cases. The [machine-generated result](../research/performance/forge-program-20261104-prompt-heldout.json)
retains outputs, hashes, exact commands and outcome counts. Its parent harness
uses Python only for measurement orchestration; Forge's executed path is Rust.
Replay each fixture's `request` with `forge context prompt --request FILE` and
check its declared expected spans against byte offsets and SHA-256. The temporary
harness and raw results remain at the recorded local paths. These are literal
behavior checks, not agent task-success, token-saving or provider-cost evidence.

The [example request](../research/performance/forge-program-prompt-request-20261104.json)
produces 1,948 bytes of compact JSON. A [current-only receipt](../research/performance/forge-program-20261104-prompt-baseline.json)
records 31 successful process launches after three warmups, 10-second bounds:
wall p50/p95 **1.651448/2.049500 ms**, CPU p50 **1.231 ms**, maximum child RSS
**13,393,920 bytes**. Source scope binds the core, CLI adapter, MCP context/catalog
and executable; workload identity binds the example request. The host is shared
and filesystem warm; allocations, cold-host behavior and MCP latency are unmeasured.
Reproduce under a cleared environment with `PATH=/home/tim/.cargo/bin:/usr/bin:/bin`,
`HOME=/home/tim`, `LANG=C.UTF-8` and the receipt's literal benchmark arguments.

Matched regression guards copied the checked pre-prompt binary and this working
binary sequentially to the same slot, with identical commands, cleared environment,
101 samples, five warmups and ten-second bounds per workload. Binary-only scope
avoids claiming today's source belongs to the old executable; symbol input is
separately bound. Both comparisons pass the unchanged 10% budget, with all 404
measured samples successful and zero failed/censored/unstarted.

| Workload | Wall p50 before / after, ms | Wall p95 before / after, ms | CPU p50 before / after, ms | Max RSS before / after, bytes | Evidence |
| --- | ---: | ---: | ---: | ---: | --- |
| CLI help | 1.604428 / 1.600538 | 2.129920 / 1.764979 | 1.160 / 1.155 | 12,378,112 / 12,460,032 | [comparison](../research/performance/forge-program-20261104-prompt-help-comparison.json) |
| Symbol context | 2.190680 / 2.113460 | 2.613433 / 2.341541 | 1.737 / 1.697 | 16,891,904 / 17,059,840 | [comparison](../research/performance/forge-program-20261104-prompt-symbol-comparison.json) |

Executable SHA-256 values are
`0a38cfac0effa07a4ff4cf796eda4608f358b42e0ac950778852dc2fd2050042`
(checked pre-prompt build) and
`25e516118bcc460302b853b5659c9add262911151481269fae2ec96887de3a49`
(working prompt build). Shared-host distributions support no material regression
in these two commands, not a general speedup. The new operation's output is larger
than its input; shorter responses and improved wording are not the success metric.
No current measurement establishes better completed coding tasks, provider token
usage, cost per successful task or time per successful task.

### Transport, task value and missing controls

The extended `native/forge/benches/harness.rs` measures real native MCP stdio,
including `harness_mcp.rs::forward` opening a fresh Unix socket per call, alongside
equivalent CLI `code fold` results. Its reused-socket diagnostics remain separately
labeled and now use 31 batches of ten operations; they are not the old 100-operation
batch workload. All CLI/MCP calls must match the complete native reference payload
and source hash. The adapter negotiates `2025-11-25`, discovers the full permitted
catalog, and revalidates it after measurement. A custom wire client is not proof
of Claude/Codex adoption or agent tool selection.

Five process-fresh repetitions ran on the shared host with uncontrolled filesystem
and kernel caches. Each arm makes one first call, ten warmups and 31 batches of ten
measured calls, alternating arm order. All 1,605 CLI and 1,605 MCP calls completed,
including 1,550 measured calls per arm, with zero failed/censored/unstarted calls.
The 20 protocol operations and 4,815 separate Unix diagnostic calls also completed.
Protocol accounting includes the initialized notification as a completed write,
not acknowledged server processing. Every run verifies adapter EOF/exit, service
stop, fixture removal, catalog stability and matching before/after input hashes.

| Phase | Median across five runs | Observed range |
| --- | ---: | ---: |
| Fixture/index preparation | 35.897 ms | 35.304–47.589 ms |
| Service launch through readiness | 37.430 ms | 29.915–49.080 ms |
| Adapter launch and initialization | 1.360 ms | 1.258–1.580 ms |
| Full permitted catalog discovery, including one client schema validation | 0.141 ms | 0.116–0.170 ms |
| CLI median batch mean per operation | 1.061 ms | 1.029–1.070 ms |
| MCP median batch mean per operation | 39.904 µs | 37.303–41.871 µs |
| CLI p95 batch mean per operation | 1.163 ms | 1.132–1.271 ms |
| MCP p95 batch mean per operation | 51.786 µs | 46.306–53.529 µs |

These p95s describe batch means, not individual-call tails. This tiny one-function
folding workload demonstrates lower per-call latency once the service is running;
startup and preparation remain separate costs. It does not establish a general
speedup or faster complete agent tasks. The catalog is 2,775 serialized bytes;
that byte count is not a provider token measurement.

The [outer native receipt](../research/performance/forge-program-20261104-mcp-transport-baseline.json)
records whole-driver p50/p95 658.435/679.279 ms, CPU p50 482.381 ms and maximum child
RSS 15,433,728 bytes. It includes driver/waited-child work and excludes the separate
systemd daemon. Observed process RSS peaks during batch snapshots were 6,225,920
bytes for the driver, 15,273,984 for the adapter and 20,418,560 for the daemon;
these must not be summed into a simultaneous tree peak. Component CPU ticks have
10 ms resolution: zeros in short MCP batches do not mean zero CPU consumption.
The [machine-generated phase projection](../research/performance/forge-program-20261104-mcp-transport-observations.json)
retains per-batch timing/counter deltas, outcome counts, hashes and raw-log paths.
Its collector associates outer receipt and inner log hashes after collection;
the outer receipt seal itself does not bind those logs. Before/after hashes are
point observations and cannot attest absence of transient substitutions.

Benchmark executable SHA-256:
`c50c8c76c5215045ffab0bca47d03c3ffa7bb78f6ab0f9e5044d9f418c8ac161`;
Forge executable SHA-256:
`8785f88753a53a5715aa95c563bf58f6f2f2d98ace6f56b99d20cb4c86da63dd`.
The raw reports and receipt bind the source inventory, explicit build inputs,
fixture/request/policy/catalog identities and environment within their declared
scopes. This remains working-build measurement, separate from publication checks.

Rebuild with Rust 1.98.0 using `make build` and
`cargo +1.98.0 bench --offline --locked --manifest-path native/forge/Cargo.toml --bench harness --no-run`.
Set `FORGE_TEST_BIN` to that absolute Forge path and `FORGE_BENCH_SERVICE=1`.
Run the prebuilt benchmark through `forge performance benchmark` with five samples,
zero outer warmups and `--timeout-seconds 120`, retaining the source paths and
literal command from the receipt. The inner 90-second deadline is cooperative;
setup, synchronous filesystem work and cleanup are not preempted by it. The outer
runner supplies the process bound and the owned daemon has a 120-second lifetime.
Missing opt-in and a nonready native executable each produce an invalid report
and no receipt. Uncertain adapter/service cleanup preserves its fixture and exact
path instead of deleting potentially live state. Twenty-two live CLI/harness tests,
including existing cancellation/capability cases, and strict Clippy passed.

These small dispatch costs do not justify further micro-optimization by themselves.
Next measure useful work per task, including discovery/context reuse, the cost of
recommendation calls, actual client selection and provider usage.

| Evidence | What it supports | What remains unmeasured |
| --- | --- | --- |
| Fresh CLI receipts above | Process-cold/filesystem-warm latency, child CPU/RSS and matched repair comparison for two exact commands | Cold-host behavior, allocation counts, concurrent calls and other workloads |
| Existing schema/context/component benchmarks | Scoped implementation costs and deterministic fixture outputs | Full dispatch, client selection, provider envelope and task success |
| Historical [product-value observations](native-product-value-measurements.md) | Native relocation slowdown and a signature projection's smaller supplied payload | General speedup, net billed savings or equivalent full-file semantics |
| Historical [retrieval run](native-code-retrieval-measurement.md) | 144 completed retrievals; 0 failed/censored/unstarted; frozen agent-authored labels | Independent broad relevance, agent success and full resource attribution; preparation took 23.6 minutes |
| Existing context composition, counting, recovery and usage reports | Reusable native primitives; exact supplied-text token counts when pinned | Actual model usage in a matched experiment, recovery/reread cost and task-quality effect |
| Hook and service integration fixtures and measured raw MCP above | Handler/protocol/control behavior and sequential stdio transport costs within tested scopes | Hooks enabled/disabled overhead, concurrent MCP latency and actual-client enforcement |

No current evidence establishes higher verified agent success, faster complete
tasks, net provider cost savings or an optimal context budget. Prioritize actual
client validation, representative transport/concurrency and hooks controls, then repeated
agent tasks with independent correctness checks and complete outcome counts.
Record model/version/budget, source/binary identities, preparation, cache/load,
provider usage, rereads and failures. Allocate optimization effort from those
measurements; native relocation's recorded slowdown remains visible rather than
being generalized into a language-performance claim.

## Benchmark and comparison contract

### Rust mutation workflow without Python — 2026-11-04

A matched behavior check used two identical, dependency-free Rust fixtures with
the same automatically generated campaign manifest and no `snapshot_python`
setting or host venv. Each child received a private native-tool allowlist on
`PATH`, containing Rust/Cargo 1.98.0, cargo-mutants 27.1.0, Git and native compiler
utilities. Neither `python` nor `python3` was discoverable. The actual Rust test
also asserts those names are absent from its `PATH` and the guest interpreter
variable is unset. The [machine-generated summary](../research/performance/forge-program-20261104-no-python-behavior.json)
records tool/binary hashes, configuration, source/test hashes and raw evidence paths.

| Binary | Inspect / run | Completed mutant work | Receipt interpreter | Cleanup |
| --- | --- | --- | --- | --- |
| Merged repair's verified build (`966ae9a`) | `NOT_READY` (3) / `REFUSED` (4): missing Python | No campaign executed | No receipt | No snapshot created |
| Optional-guest working build | `READY` (0) / `PASS` (0) | 2 planned, 2 completed and killed; 0 failed/censored/unstarted | `null` | Snapshot removed |

The baseline binary SHA-256 is
`fe7145e55fe0c086d581d3a03cde187c87f30c2b966b3617318dc8f64693cd64`;
the changed binary is
`72ca1693a5f60c53a0f09f102b5bcfc112733e3f26d2feb094e25f57b61b2c01`.
The checked live source, test and configuration files were unchanged. The new
runner updated its generated manifest only to record the initial empty survivor
baseline; the original and updated manifest hashes are retained. The Rust
baseline command took 0.142 s and its engine 0.383 s in this one fixture, not a
latency distribution or matched speedup against the refused command.

The parent measurement harness uses Python outside the Forge child environment.
PATH control is not an OS sandbox: absolute interpreter paths remain accessible.
The working binary is dirty and the tiny hosts lack Forge sources, so the PASS
is runtime behavior evidence, not registered governance evidence. Mull has
adapter/environment unit coverage here, not a new real-engine no-Python trial.
Fest retains its Python requirement. No agent-productivity or token-cost claim
follows from this check.

To reproduce, use the two-function Rust fixture described below, omit
`snapshot_python`, and add the two environment assertions to its integration test.
Run each pinned binary's automatic writer, inspect and runner with identical
explicit native-only `PATH`, private Cargo/home/temp directories, offline Cargo,
one worker and a 120-second engine bound. Keep manifests and receipts untouched;
retain refusal, generated baseline updates, accounting, source checks and cleanup.

### Native mutation behavior checks — 2026-11-04

Two tiny, independent Git fixtures used the automatic native campaign writer and
cargo-mutants 27.1.0, with one worker and a 120-second engine bound. No campaigns,
mutants or receipts were authored or repaired by hand. The
[machine-generated summary](../research/performance/forge-program-20261104-mutation-behavior.json)
binds binary, engine, generated manifests, source/test bytes and receipt hashes;
raw fixture evidence remains at its recorded local paths.

| Selected sources | Actual result | Planned / completed / failed / censored / unstarted mutants | Source counts | Snapshot removed |
| --- | --- | --- | --- | --- |
| Boolean function plus a type alias with no mutation sites | `INCOMPLETE`, exit 4 | 1 / 1 / 0 / 0 / 0 | a: 1, b: 0 | Yes |
| Two tested Boolean functions | `PASS`, exit 0 | 2 / 2 / 0 / 0 / 0 | a: 1, b: 1 | Yes |

All reported mutants were killed. The negative case preserves the engine's actual
one-mutant plan while separately rejecting the unmeasured selected source; it
does not invent an unstarted mutant. Source coverage is not per-line coverage.
Both generated manifests retain `Cargo.toml`; the adapter supplies `./Cargo.toml`
to cargo-mutants. The initial root-layout failure and intervening nested-layout
probes are preserved separately in the summary's local attempt history.

The executed binary SHA-256 is
`9d662e91c9d4a808b2ddc09c9f9c644a3c5c625dc1823fec6b4aa400b73617da`.
These checks used the repair working tree, and the small hosts lack Forge's full
native source inventory. Their receipts are **behavior evidence, not registered
PASS evidence or proof of receipt freshness for a deployed host**. They measure
neither agent productivity nor an end-to-end speedup.

To reproduce, create a dependency-free Rust library named `coverage_fixture`,
edition 2021. Its `src/lib.rs` declares `pub mod a; pub mod b;`. `a.rs` contains
`pub fn value() -> bool { true }`; `b.rs` contains either `pub type Byte = u8;`
or the same function. An integration test asserts `a::value()` and either the
alias size (one byte) or `b::value()`. Commit a baseline on `main`, then commit a
comment-only change to exactly `a.rs` and `b.rs` on the fixture branch. Configure
the host's receipt/registry paths and existing snapshot interpreter, then use
`forge mutation write rust --base main --owner fixture --jobs 1 --run-timeout 120
--only src/a.rs --only src/b.rs`, followed by `forge mutation run` on the emitted
manifest with the same base, owner and selectors plus `--allow-mutations`.

### Receipt benchmark rules

`forge performance benchmark` creates numerical evidence for a successful bounded
shell workload. It uses native Linux process measurement, so no profiler needs
to be installed for benchmarking:

```sh
forge performance benchmark \
  --command './native/forge/target/debug/forge --help' \
  --source native/forge/src/main.rs \
  --source native/forge/target/debug/forge \
  --samples 20 --warmups 2 --timeout-seconds 10 \
  --output research/performance/cli-current.json
forge performance compare \
  --baseline research/performance/cli-baseline.json \
  --current research/performance/cli-current.json \
  --max-regression-percent 10 \
  --output research/performance/cli-evidence.json
```

Use exact repeated `--source` paths for the executed code, binary and imported
dependencies. `--input dataset-or-fixture` binds workload bytes separately from
source bytes; source changes are permitted between matched revisions, while
workload input changes make the baseline unmatched. Repeated `--env NAME=value`
sets measured command configuration. The receipt stores only a digest of the
effective environment, so credential values are not published. The output path
must be new; failed or timed-out samples, source drift and existing output files
issue no replacement receipt.

Schema `forge.performance.v1` records exact source SHA-256 values, committed HEAD,
canonical host path, literal command, workload digest, effective environment
digest, CPU/kernel/resource identity, Rust toolchain, warmup count and timeout.
Five to 1,000 successful samples are required. Every sample records wall time,
CPU time and Linux `wait4` maximum process RSS; the receipt recomputes nearest-rank
p50/p95 wall latency, p50 CPU time and maximum RSS. RSS is the largest measured
process, not simultaneous summed tree memory. These local cooperative receipts
are not cryptographic attestation that an untrusted producer ran the workload.

Comparison validates both receipt seals and recomputes summaries. It requires
equal host, command/workload inputs, environment, platform, toolchain, source
path scope, warmups, timeout and sample count. Any p50/p95 wall, CPU or RSS
regression above the selected budget fails. A zero CPU baseline accepts only
zero CPU in the current result. Run matched measurements under comparable load;
these samples provide descriptive latency distributions, not statistical proof
of a speedup. Use representative large graphs, quiet and verbose logs, native
and compatibility test targets, and cold/warm runs separately.

Candidate review's performance-evidence check reads changed JSON
`forge.performance-evidence.v1` artifacts containing both sealed receipts and an
explicit budget. It recomputes the matched comparison and verifies current
changed hot-path source hashes. An unmatched or failing pair cannot pass;
neither can a raw current-only receipt or a budget exceeding the 10% policy
ceiling. Smaller evidence budgets are allowed.
A filename containing `bench` or prose mentioning `latency` does not establish
performance evidence. Existing historical receipts are retained; only new
candidate evidence must satisfy this numerical contract.

Optional external profiler commands are explicit and never installed automatically:

```sh
forge performance profile --tool hyperfine --output timing.json -- \
  --warmup 3 './native/forge/target/debug/forge --help'
forge performance profile --tool py-spy --output profile.svg -- \
  -- python workload.py
forge performance profile --tool cargo-flamegraph --output flamegraph.svg -- \
  --manifest-path native/forge-cli/Cargo.toml --bin forge -- --help
```

Requested tools must exist on PATH and successfully write a new output file.
Their optional runtime prerequisites and installers are declared in
`pyproject.toml`; the commands fail with an actionable missing-tool error.
Profiler output helps locate bottlenecks and does not replace benchmark receipts.
The native wrapper invokes `cargo-flamegraph flamegraph`, preserving the literal
subcommand required by the [upstream CLI parser](https://github.com/flamegraph-rs/flamegraph/blob/main/src/bin/cargo-flamegraph.rs).
On Linux it also requires `perf`, as described in the [upstream README](https://github.com/flamegraph-rs/flamegraph#usage).
Linux pidfd waits remove fixed completion polling from measured check workloads;
older or restricted kernels use a documented bounded 5 ms waitid fallback while
keeping the process-group leader unreaped until descendant cleanup.

## Candidate execution preparation and capture

`cargo +1.98.0 bench --manifest-path native/forge/Cargo.toml --bench candidate_execution`
measures pure native argument preparation and bounded output capture. The
2026-11-01 release run used an unisolated warm host, 10 warmups and 31 batches.
It excludes executable lookup, snapshots, sandbox setup, process startup and
analyzer work. It is current-only evidence, not a matched speedup or a full
tool-dispatch latency measurement.

| Operation | Iterations per batch | Median batch mean | p95 batch mean |
| --- | ---: | ---: | ---: |
| Prepare argv for 32 selected files | 100 | 1.793 µs | 1.833 µs |
| Same, with eight repetitions of dense placeholders | 100 | 4.231 µs | 4.775 µs |
| Push 4 KiB into a preallocated 12,000-character tail | 1,000 | 30.001 ns | 30.760 ns |
| Finish a full mixed UTF-8/CRLF tail, setup excluded | 10 | 65.132 µs | 67.113 µs |

The benchmark checks expected output before measuring. Each tail keeps a fixed
raw-byte ring during ingestion; decoding, newline normalization and final
Unicode-scalar selection occur at finish. The steady push timing reuses hot
buffers and is not a cold-memory bandwidth claim.

Benchmark executable SHA-256:
`61647deb7945432b6833a5905a2812dec38d18cb1f0b087e2625ac86df490a1a`.
Result JSON SHA-256:
`17484d9601100888564a1025827f628d074eda2587b56a5411b3d9f5bc16f6c4`.
Measured source SHA-256 bindings:

| File | SHA-256 |
| --- | --- |
| `native/forge/benches/candidate_execution.rs` | `51cc5ffadb399f992d726454c972a5ff6cd37e7b79da3f287a81126f77105fd5` |
| `native/forge/src/candidate_command.rs` | `6d5d6e080523059412cb002b77ee3306f42e79bde26ab6c320273bbb8d983860` |
| `native/forge/src/process_capture_tail.rs` | `d5392281bfa1c0b12bd5443c06f3d22e4712ae616a1ed5de935e4724a14f19a6` |

## Native batch scheduling

`cargo +1.98.0 bench --manifest-path native/forge/Cargo.toml --bench batch_execution`
measures the native batch owner, task scheduling, cancellation observation and
ordered result collection with 64 no-op jobs on one Tokio reactor thread.
The 2026-11-01 release run used an unisolated warm host, 10 warmup batches and
31 samples of 50 batches per admission limit. Result order and contents are
checked before timing. Total measured execution took 213 ms.

| Admitted jobs | Median batch mean | p95 batch mean | Median per-job mean | p95 per-job mean |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 53.316 µs | 63.739 µs | 0.833 µs | 0.996 µs |
| 2 | 43.845 µs | 49.912 µs | 0.685 µs | 0.780 µs |
| 8 | 34.989 µs | 42.420 µs | 0.547 µs | 0.663 µs |

Per-job values divide whole-batch timing by 64; they are not individual job
latency percentiles. Admission limits here allow overlapping async tasks on
one thread. These current-only measurements exclude candidate selection,
snapshots, cgroups, sandbox/process launch and analyzer work, and establish no
end-to-end speedup or model-quality improvement.

Benchmark executable SHA-256:
`9c5125c2f91531c39b623c4624906a32c80615a10df6e4b21a15a0e33f0e7353`.
Result JSON SHA-256:
`fd39c8aac26fa1a84785240756c42b64e31f262173d6ccab88565681289fcb28`.
Measured source SHA-256 bindings:

| File | SHA-256 |
| --- | --- |
| `native/forge/benches/batch_execution.rs` | `3e5be42c34f0d5039edd1652a8beffac72ee14245f02b72add4dc0988b768e79` |
| `native/forge/src/batch_execution.rs` | `1c3fe4fb6396bdb53420ea6b3e124d4929902a1555848308b3b56670a7b2e3b7` |
| `native/forge/src/confined_execution_cancel.rs` | `e830f7660c36c86507273829f226d422cf51a60b800c15e406b4b4ed56670f80` |
| `native/forge/src/confined_execution.rs` | `5fa31aedfeb950bb9a6544ab17282a5cc23a23920e718fa81bf591b960247db6` |

## Native candidate builtins

`cargo +1.98.0 bench --manifest-path native/forge/Cargo.toml --bench candidate_builtin`
measures Unicode path conversion and the pure candidate-integrity evaluator.
The 2026-11-01 release run used an unisolated warm host, 10 warmups and 31
samples. Each conversion operation processes its entire corpus, including
allocation and output drop; each sample repeats it 100 times. The integrity
sample repeats 20 times, checking 1,024 entries and 128 changes against eight
protected globs and producing 128 findings. Expected outputs are checked first.

| Operation | Corpus | Median batch mean | p95 batch mean |
| --- | --- | ---: | ---: |
| Full casefold | Four ASCII paths, 75 bytes | 62.600 ns | 63.200 ns |
| Python-compatible repr | Same ASCII paths | 133.300 ns | 133.400 ns |
| Full casefold | Four mixed Unicode/control paths, 59 bytes | 148.800 ns | 172.400 ns |
| Python-compatible repr | Same mixed paths | 191.600 ns | 212.600 ns |
| Full casefold | Two paths of 4,096 and 4,093 bytes | 13.630 µs | 14.674 µs |
| Python-compatible repr | Same near-limit paths | 9.555 µs | 9.855 µs |
| Candidate integrity | 1,024 entries / 128 changes | 174.247 µs | 176.694 µs |

Integrity timing includes borrowed payload decoding, admission, tree lookup,
glob preparation/matching, findings and JSON Value allocation/drop. It excludes
payload construction, Unicode metadata preparation, JSON text encoding, source
and snapshot I/O, sandbox/process setup, fingerprints and assessment. These are
component costs, not end-to-end tool latency.

An immediately preceding run of the same benchmark and workloads, before adding
the ASCII casefold fast path, measured casefold medians of 577.610 ns, 445.800 ns
and 61.970 µs for the three corpora. The fast-path run measured lower component
costs by factors of 9.23, 3.00 and 4.55 respectively. This single before/after
pair used an unisolated host; it does not establish an application-wide speedup.
The integrity workload does not include casefold preparation and stayed near
173–174 µs. Both runs record identical workload metadata and input/output hashes.

Final benchmark executable SHA-256:
`13eddc76552a6dfc702e4abdd4e9776bd1ce081e47302e176d8507495752c51d`.
Final result JSON SHA-256:
`5baae2ef0f3e5f3891bef72beab6d3dc342529a126207d5d1f289eefe672aa35`.
Baseline executable SHA-256:
`ce297b7fad12819df252ec535de422727023ed2a5cf8e822dd835c7391a6e5e8`.
Baseline result JSON SHA-256:
`351f41939730929abeb2f0ca97fdae56cc1dffc41846529d00dfbdb63f88861c`.
Measured source bindings:

| File | SHA-256 |
| --- | --- |
| `native/forge/benches/candidate_builtin.rs` | `d19dad9f6e478969d861f1a9a8070beada5eb5114499735f29b1ea696a817dab` |
| `native/conductor-native/src/candidate_checks_integrity.rs` | `034ade32b7232778612f0e1e1b764612f6f90488fbbe5c54c46cf5be18092956` |
| `native/conductor-native/src/candidate_unicode.rs` (final) | `ce3b258c4bc1b6e3671d0c36dd37120079eafb8124aae00de2c1afb06f3fd070` |
| `native/conductor-native/src/candidate_unicode.rs` (baseline) | `aba654cfa4b78bc4f91c481970a7bcba22b05ec08b0967702db07ae4246d2a02` |
| `native/conductor-native/src/candidate_unicode_table.rs` | `9c4b4f9a4ab3f7dfc3d7c46d789d5020b6a152307d19434a7cdb75ca39edf007` |

## Native scan admission and line lookup

`cargo +1.98.0 bench --manifest-path native/conductor-native/Cargo.toml --features bundled-sqlite --bench candidate_scan`
compares the pure secret/native scan APIs before and after bounded admission,
producer output limits and indexed line lookup. Both 2026-11-01 release runs
used Rust 1.98.0, the same benchmark source and an unisolated warm host. Every
workload contains one file with 4,000 lines. Clean inputs are 64,000 bytes; dense
secret/native inputs are 84,000/52,000 bytes and each produces 4,000 findings.
The independent output oracle verifies count, order, line numbers, messages,
severity and secret redaction. Input and output hashes match across both runs.

| Scan | Baseline median | Final median | Final p95 |
| --- | ---: | ---: | ---: |
| Secret, clean | 5.689 µs | 5.591 µs | 5.995 µs |
| Secret, dense findings | 30.597 ms | 2.111 ms | 2.370 ms |
| Native, clean | 2.750 µs | 2.808 µs | 3.082 µs |
| Native, dense findings | 19.449 ms | 1.865 ms | 1.904 ms |

Each run uses five warmup calls and 31 samples; clean samples average ten
evaluations and dense samples time one evaluation. Dense-case median ratios
are 14.49× and 10.43×. Clean-case differences are small in this single run pair;
these measurements do not establish an application-wide speedup.

Previously each finding rescanned the source prefix to count LF bytes. Now the
first match builds an LF index reused by subsequent findings; clean files build
none. Input admission checks a minimum and worst-case JSON size from byte
lengths, using exact serialization accounting only when those bounds cannot
decide admission. This preserves the exact accepted byte boundary without
rescanning ordinary small payloads. Output admission counts each finding before
retention. Those checks and output Value allocation/drop are included in the
final timings. Source/snapshot reads, payload JSON parsing, final JSON text
encoding, hashing, fingerprints and full review remain outside the measurement.

Baseline/final executable SHA-256:
`62353038d87f89a151ed416508e986393626822bc8ce2dcd8d6f864c44ba6920` /
`8d9bb8c6e9982de00989ec175fd7d00b1e022c76796808ef323673f8a76c859e`.
Baseline/final result JSON SHA-256:
`76413aba2e13b9ee3919f595108ed9cbf7994a8174371a9e8daf991a0b980417` /
`f2f469931014a66eb977baca635e6c0afcdd1db8bc5ad7c16bf811cb851f3e1b`.

| Measured source | SHA-256 |
| --- | --- |
| `native/conductor-native/benches/candidate_scan.rs` (both) | `38dcc773efb0e9669b011601a44889b9c4920a9aad782a1ea5d5b80906c04840` |
| `native/conductor-native/src/candidate_checks_scan.rs` (baseline) | `461d82a167d3b89b17d71bea35898ef2ac0b661ab4e8ee0fb9391482796ed019` |
| `native/conductor-native/src/candidate_checks_scan.rs` (final) | `838ea3e828e0ff99acce13101f70e307e6d15e6ae51aba78a38d96eec164c312` |
