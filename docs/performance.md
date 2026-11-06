# Measuring performance

## Current program evidence — 2026-11-05

This is the canonical performance and effectiveness report; current mission status
and ordered continuation are in [the roadmap](roadmap.md#current-mission-checkpoint--2026-11-05).
Source baseline is `9117c2fb6436251d0de50038db85ef7fbb81e4b3`; the repair landed in
[PR #185](https://github.com/mcpirate17/llm-forge/pull/185). Pre-change observations,
repaired working-tree measurements and matched binary comparisons are separated
below. They do not certify the eventual PR head or installed runtime. Test and
publication status are in the current [engineering status](roadmap.md).

### Confined MCP test report — 2026-11-06

The operator-enabled `python_test_report` uses the native classifier through an
owned confined worker. Release source `2d23d1c` and observer source `a3ac295` were
measured on the same authored repository: one Python test and two modules.
The [evidence index](../research/performance/forge-program-20261106-mcp-test-report.json)
retains the initial pre-launch admission failure, correction, live controls,
native performance receipt, raw observations and independent audits.

| Five scored observations, 31 calls per arm each | Inner median range | Inner p95 range |
| --- | ---: | ---: |
| Direct CLI | 2.221–2.357 ms | 2.533–2.841 ms |
| MCP through the persistent service | 79.993–80.594 ms | 85.572–88.397 ms |

All 396 calls completed: six observations including one outer warmup, each with
separate first calls, one inner warmup per arm and 31 alternating pairs. Complete
report values matched, inputs were rechecked and owned cleanup was verified.
These are as-configured costs: the worker has 500 milliCPU/128 MiB limits while
the CLI has no equivalent cgroup cap. Every MCP call launches a fresh worker;
only the service/adapter are reused. Filesystem caches were uncontrolled.

Service RSS point samples ranged from 20.6–27.9 MiB and adapter samples from
15.3–16.0 MiB. Worker/direct-CLI CPU and RSS were unavailable. Recorded process
ticks include hashing and observation work; they are not per-call attribution.
The outer receipt measures the entire debug observer, including large executable
hashes, setup and cleanup: median/p95 20.268/20.328 seconds, CPU median 17.284
seconds and maximum RSS 354.9 MiB. It excludes separately managed service/worker
resources and is not a product latency or aggregate-memory result. No speedup,
model-token savings, external-client adoption or task benefit is established.

### Full native test-import report — 2026-11-06

`forge analyze tests` shares the existing classifier and replaces the Make target's
Python collector. The first matched run completed 48 executions: native median
510.60 ms versus Python 180.01 ms. Profiling identified unnecessary rename scoring.
The [evidence index](../research/performance/forge-program-20261106-test-report.json) retains that baseline, profiles, failures, commands and pins.

| Actual Forge report | Median ms | p95 ms | CPU median ms | Maximum RSS MiB |
| --- | ---: | ---: | ---: | ---: |
| Python CLI | 181.06 | 189.88 | 181.73 | 31.0 |
| Native `6a29c89` | 510.75 | 523.75 | 507.50 | 42.9 |
| Native `1195b04` | 218.97 | 231.67 | 217.17 | 32.8 |

All 108 executions completed across six arm orders, with 30 scored samples per arm
and 18 warmups. Independent audits verified exact report bytes, inputs and receipts.
Relevant rename competitors and input checks remain; median native latency fell 57%,
but remained 21% above Python. Fresh processes shared filesystem caches; RSS is a
wait4 observation, not aggregate process-tree memory. These are semantic comparisons
across distinct command identities, not compatible receipt comparisons or agent gains.
The pinned Python extension's build provenance is unknown. Full release status is in the roadmap.

### Native report migration baseline — 2026-11-06

The native evidence, changed, coverage and canary commands preserve Python JSON
and exit codes. The [reporting evidence index](../research/performance/forge-program-20261106-native-reporting.json)
retains the slower 52-observation baseline, rejected path-lookup experiment,
profiles, failures, commands, comparators and final matched 90-observation run.
The rejected syscall change saved 0–10 ms while adding unsafe code. Accepted
`d1fc4dd` skips supporting-input scans only when no contract can be eligible;
eligible contracts and final verification retain their checks. The Forge corpus
has no eligible Cargo contracts, so these gains do not predict that case.

| Actual Forge report | Python median | Native `88a63ef` | Native `d1fc4dd` |
| --- | ---: | ---: | ---: |
| Explicit evidence | 40 ms | 130 ms | 40 ms |
| Changed evidence | 40 ms | 130 ms | 50 ms |
| Coverage | 100 ms | 270 ms | 180 ms |
| Canary | 100 ms | 270 ms | 180 ms |

All six arm orders completed with exact JSON/exit parity and no failures,
censoring or unstarted observations. Coverage remains slower than Python;
median maximum RSS was 44,858 versus 62,272 KiB. Native reports add input and
inventory rechecks. Six fresh-process samples per arm, 10 ms clock resolution,
uncontrolled filesystem caches and unknown Python-extension build provenance
limit inference; empirical p95 is the sample maximum. CPU/RSS, source/binary
pins and raw hashes are indexed. Debugger timings include instrumentation;
perf was denied and gprofng samples were unreliable. No agent gain was measured.

A separate automatic two-mutant fixture validated installed-runner receipt
acceptance, missing-identity debt and wrong-pin refusal; all 17 controls and 22
commands completed with verified cleanup. Make traces with `UV=false` contain
Make, shell and native Forge. These controls do not establish full mutation
coverage or completion of issue #196.

### Reproducible native timeout fixture — 2026-11-06

The opt-in Rust control at `5f78e9b` uses automatic plan/write, explicit pins and
the confined owner; [setup](mutation.md#reproduce-the-real-engine-timeout-control)
requires operator-supplied external tools and grants. Its [three-attempt index](../research/performance/forge-program-20261106-native-mutation-controls.json)
retains two pre-payload admission failures (missing engine read grant, then a
group-writable cargo-bin ancestor) and the successful private-copy control.
Engine bytes and test assertions were unchanged across attempts.

The third test passed in 11.12 seconds, but its native receipt remains incomplete
ERROR: two discovered mutants, zero completed outcomes and unknown remainder.
Metadata, baseline and engine times were 21/95/10,010 ms. Independent audit checked
all 13 command ledgers, report/source/test pins and absence of three owned cgroups;
the unrestored snapshot and automatic-mutant marker remain. Either mutant times
out; no positive completed prefix is claimed. Outer GNU time recorded 11.21 seconds,
0.91/0.28 seconds user/system CPU and maximum RSS 131,424 KiB, not aggregate memory
or matched speed evidence. Focused checks passed seven supporting-input tests,
five CLI tests, ten synthetic controls and strict Clippy. The added predicate
regressions have not been mutation-rerun; agent benefit remains unmeasured.

### Interrupted Cargo progress controls — 2026-11-06

Clean native `ad6f12f` preserved bounded partial engine progress after confirmed
cleanup. All 12 diagnostics matched retained raw hashes and canonical identities:
two discovered mutants, zero completed outcomes, unknown remainder. The first
mutant was interrupted; positive reconciliation has unit coverage, not a nonzero
live completed-prefix demonstration. The [three-attempt index](../research/performance/forge-program-20261106-partial-progress.json)
retains two failed predecessors, each with 10 completed checks and one unstarted
final check. They exposed timeout-reason propagation and JSON key-order defects;
the oracle and authored fixture stayed unchanged when producer fixes were tested.

Metadata/baseline/engine times were 13/155/10,004 ms under one CPU, 1 GiB and a
10-second engine bound. Preparation, CPU/RSS, allocations and agent usage were
not measured. All three receipts remain incomplete ERROR; diagnostics cannot
change scoring or passing admission. Independent audit confirmed streams and
absence of owned scopes; the unrestored snapshot remains. The final tree passed
18 publication checks and verification at `6369de3` (4,819 passing executions,
zero failures, 188 ignored); [PR #206](https://github.com/mcpirate17/llm-forge/pull/206)
merged the identical tree as `c95592a`. No shared-host upgrade is implied.

### Compiler-header control and actual Forge timeout — 2026-11-06

A small authored workspace passed its C-header build probe with `/usr/include`
explicitly added to the existing read grants. The support crate's build script
compiled C containing `#include <stdio.h>` and required a nonempty regular object;
the retained baseline log records 1,240 bytes. Both automatic Rust mutants were
killed, all 31 driver checks completed, and corpus drift/restoration checks passed.
Independent audit verified all 146 retained command streams and positive absence
of the three owned cgroups and snapshot. The object itself was removed with the
snapshot. This is a cooperative compile-only probe, not full compiler closure.

The subsequent actual Forge attempt used source `83990e2`, the same clean native
runner at `4cba4eb`, the 338-package offline seed, and only
`mutation_cargo_supporting.rs` as mutation source. It selected the library filter
`mutation_cargo_supporting::tests::`. Its fresh baseline passed five tests, with
1,153 filtered out. The engine then exceeded its separate 300-second bound.
The [evidence index](../research/performance/forge-program-20261106-compiler-and-timeout.json)
binds both attempts and their independent audits.

| Workload | Native metadata / baseline / engine wall time | Outcome |
| --- | --- | --- |
| Tiny header workspace, one CPU / 1 GiB / 60 seconds per command | 24 / 236 / 318 ms | Two mutants killed, PASS |
| Actual selected Forge library, two CPUs / 8 GiB / 300 seconds per baseline or engine | 104 / 156,996 / 300,101 ms | Baseline passed; engine timed out, ERROR |

Cargo used one build worker in both attempts. These are different workloads and
resource limits, not a matched speed comparison. Command timings exclude seed
staging, snapshot creation, input hashing and driver overhead. CPU, RSS,
allocation and model-token measurements are unavailable; grants are limits, not
observed usage. Neither attempt measures agent productivity.

The actual engine's retained discovery file lists 55 mutants. Its partial report
contains 44 completed mutant scenarios: 34 caught, seven missed and three
unviable. The other 11 have unknown states; they must not all be called unstarted.
The native ERROR receipt imported no mutant rows and reports unknown planned and
unstarted counts, because timeout handling returned before importing progress.
This gap is tracked in [#205](https://github.com/mcpirate17/llm-forge/issues/205).
The 13 completed driver checks establish diagnostic collection, not campaign PASS.
All three owned cgroups were absent after the run; its snapshot remains with an
unrestored automatic mutation. The three owned repository outputs were moved
unchanged into private evidence storage with device, inode and hash verification.

### Selected workspace test controls — 2026-11-06

The clean release at `4cba4eb` exercised four automatic cargo-mutants controls
using the same authored workspace and two boolean mutation subjects. The
[evidence index](../research/performance/forge-program-20261106-selected-workspace.json)
includes fixture source, binary/source identities, observed outcomes and retained
command-stream hashes. Each successful control completed 31 checks; no checks
failed, timed out or remained unstarted. An earlier launch refused a mistyped
toolchain hash before any commands ran (one check completed, one failed, 29
unstarted); that attempt is retained too.

| Selected target and filter | Baseline passed / filtered | Automatic mutants | Native baseline / engine wall time |
| --- | ---: | --- | ---: |
| Integration `contract`, `chosen::strong` | 1 / 1 | 2 killed, PASS | 125 / 369 ms |
| Integration `contract`, `chosen::weak` | 1 / 1 | 2 survived, RATCHET_HELD | 117 / 321 ms |
| Integration `contract`, no filter | 2 / 0 | 2 killed, PASS | 144 / 325 ms |
| Library, `chosen::weak` | 1 / 0 | 2 survived, RATCHET_HELD | 94 / 318 ms |

An unselected target contains `compile_error!`, so successful engine execution
exercises target selection during compilation as well as testing. Another
unselected integration target has the same weak-test name but assertions that
kill the mutants. The surviving controls therefore check that unselected tests
cannot make a weak selected scope appear to pass. Direct and engine baseline
counts and test-name/status digests matched. Survivor baselines were recorded
only by the native runner; `RATCHET_HELD` is not mutation PASS.

All four original and exactly restored fixtures were accepted by live and
committed corpus checks; supporting-source and test-data drift were refused.
Corpus acceptance means reproducibility of the scoped receipt, including
RATCHET_HELD, not candidate approval. Independent audit checked all 584 retained
stdout/stderr streams, current input/evidence hashes and positive absence of the
12 owned cgroups and four snapshots.

The timings are single command observations that exclude admission, snapshot,
hashing and driver overhead. Coverage differs deliberately between cases, so
they establish no matched speedup, latency distribution or agent benefit. CPU,
RSS, allocation and model-token measurements are unavailable for these controls.
The original internal engine logs and direct-baseline output were removed with
the snapshots; their recorded parser observations cannot be independently
re-read. Other raw evidence and the private driver remain host-local: this index
is neither a portable replay bundle nor execution attestation. The selected
profile's zero-test/ignored-only refusals are covered by unit tests; no new
selected-profile timeout workload was run.

### Cargo workspace mutation evidence — 2026-11-06

The first bounded Cargo seed preparation attempt refused the local cache's
group-writable permissions before copying any files. The native task receipt
recorded exit 1 without timeout, and its owned scope was independently confirmed
absent. This failed admission is not a provisioning benchmark. The revised
import treats public cache bytes as untrusted, verifies archives against the
held lockfile and reads only required index records. Index bytes remain an
operator-selected observation, not authenticated registry metadata.

One subsequent dirty debug-build observation prepared all 338 registry packages
from Forge's lockfile: 647 archive/index files totaling 79,078,410 bytes, with
16,351 expanded files totaling 428,479,979 bytes verified from the archives.
Under a native one-CPU, 1 GiB, 120-second task bound, preparation took 19.28
seconds wall time (19.13 user, 0.09 system) with 87,328 KiB peak RSS. The task
exited zero, retained all output and its scope was confirmed absent. This is a
single preparation observation, not a cold/warm distribution, matched comparison
or runtime staging test. No performance or task-benefit improvement is claimed.

The subsequent dirty-build nested workspace/path-dependency diagnostic killed
both automatically generated mutants and observed one passing test in both the
direct and engine baselines. Its three command cgroups and snapshot were confirmed
absent. Two preceding failed attempts are retained: metadata capture exceeded the
selected API's limit, then cargo-mutants rejected `--jobs` with `--in-place`.
The corrected engine phase took 318 ms in one observation; this excludes Forge
admission, input verification and driver overhead and is not end-to-end latency.

A separate one-registry-package trial provisioned a 25,761-byte seed but Cargo
metadata failed during timestamp restoration while extracting the archive.
Forge refused the incomplete extracted-source inventory, retained the snapshot
and confirmed command cleanup. A diagnostic rerun preserved the underlying
`Operation not permitted` error. Native pre-extraction then passed a fresh
registry-backed trial: both automatic mutants were killed, direct/engine baselines
each observed one passing test, and all three cgroups plus the snapshot were
confirmed absent. Its 325 ms engine observation excludes admission and staging.
The dirty-build trial completed 15 checks with 19 clean-build/corpus checks left
unstarted. Subsequent clean-release trials are recorded separately below; the
dirty attempts do not establish clean runner identity or corpus acceptance.

The [17-trial evidence index](../research/performance/forge-program-20261106-workspace-mutation.json)
retains every attempt, source/executable identity, native outcome, orchestration
accounting and command-log digest. It is an evidence index, not an admission
receipt or execution attestation. Repeated trials of the same authored fixtures
are not independent task samples.

| Group | Trials | Observed result |
| --- | ---: | --- |
| Full-lock seed preparation | 2 | Initial permission refusal, then successful preparation |
| Dirty workspace diagnostics | 6 | Four failures and two core-behavior successes; no clean admission |
| Initial clean workspace fixtures at `0522270` | 2 | Scoped core and corpus checks satisfied |
| Real-mutant timeout | 1 | Incomplete ERROR receipt, process cleanup and unrestored snapshot retained |
| Actual Forge workspace | 4 | Three admission refusals, then BASELINE_FAILED |
| Final clean workspace fixtures at `fee612b` | 2 | 31 and 34 checks completed; none failed, censored or unstarted |

The final clean release has executable SHA-256
`e690d44f1baec3e2d3d64d7b2459f80a3ae73ddf1b77b598fb70b82980807f96`
and compiled source SHA-256
`1f252decab4d1c16b7aab22bda12515face03b3f327035c87aea67a329bc31c3`.
Each final fixture killed two automatically generated mutants and observed one
passing test in each direct/engine baseline. Both live and committed corpus
checks accepted the original inputs, rejected supporting-source and test-data
drift, and accepted exact restoration. All three owned command groups and the
snapshot were independently confirmed absent for each fixture. These are scoped
receipt checks, not whole-candidate approval, test value or agent effectiveness.

The timeout trial used a generated mutant that wrote an observation marker and
then slept beyond the ten-second command limit. The marker and changed source
were observed, the nonzero direct baseline had passed, and the engine timed out.
All owned groups were removed; the unrestored snapshot remained with an incomplete
ERROR receipt. Its 12 completed oracle checks do not constitute mutation PASS.

Actual Forge attempts identified and repaired exact-scope handling of generated
outputs, confined parent paths in Cargo metadata, and missing unused dependency
examples. Inspection of the locked `nom` archive found six declared examples
absent from its contents; that explains the third attempt's opaque missing-path
error by inference. New errors identify the package, target and path.

The fourth actual trial consumed the 338-package seed and admitted metadata:
96 ms, exit zero, 1,720,304 captured stdout bytes. Its unmutated package baseline
exited 101 after 29.961 seconds, without timeout, because `ring` compilation could
not read `/usr/include/stdc-predef.h` under the supplied grants. The native receipt
is BASELINE_FAILED; two owned groups and the snapshot were confirmed absent.
The driver has 8 completed, 1 failed and 4 unstarted checks. Its unstarted
projection is not proof no native work occurred: the native ledger records both
metadata and baseline commands. No mutation engine or passing Forge suite is
established by this trial. Phase durations exclude preparation and admission.

No matched speedup, p50/p95 distribution, model-token saving or task-benefit claim
is made here. Resource grants are caps, not observed CPU or peak-memory usage.
Raw binaries, fixtures, logs and private collector scripts remain on the measurement
host; the committed index preserves hashes and references but is not a standalone
replay bundle. Failed generated outputs were preserved byte-for-byte outside the
active campaign registry, with pinned relocation records. No failed, censored or
unstarted phase is counted as passing evidence.

### Actual Codex approval diagnostic — 2026-11-05

The [retained failed attempt](../research/performance/forge-program-20261105-codex-mcp-approval.json)
used Codex CLI 0.166.0, requested `gpt-6-luna` with low reasoning, and the checked
`bab611f` Forge build from PR #201. Invocation-only MCP configuration exposed
`code_inspect` and the operator-pinned `outbound_read_file` fixture. The prompt
requested exactly three calls: inspect an authored Rust symbol, read its allowed
source, then attempt an explicitly ungranted sibling path. This is a prescribed
interoperability workload, not autonomous tool selection.

**The attempt failed.** Codex discovered and attempted all three tools, but each
client event reported `MCP tool call requires approval, but approval policy is never`.
There were zero successful tool calls and zero outbound endpoint launches/calls.
The client exited zero; the independent result oracle correctly rejected that
false success signal. Eleven diagnostic phases completed and one failed, with
none censored or unstarted. Both native managed scopes and the socket were
positively absent after cleanup and independently rechecked. The user's persistent
configuration hash was unchanged; client approval settings were not relaxed.

The final client event reported 50,293 input tokens, including 39,296 cached input
tokens, and 196 output tokens. These are client-reported counts for this failed
attempt, not independently verified billing or savings. The source fixture was
46 bytes; input usage includes context outside the requested file, whose exact
composition was not measured. No task benefit, denied-read enforcement, serving
model attestation, CPU/RSS comparison or dollar-cost conclusion follows.

The artifact retains literal client arguments, executable/source/driver hashes,
task resource enforcement, phase observations, client results and raw-log hashes.
The native task owner enforced one attempt, a 90-second deadline, one CPU,
1 GiB RAM and bounded retained logs. Repeating model calls requires the operator's
client approval decision; see [client configuration](native-mcp-setup.md#codex-cli).

### Native outbound MCP observations — 2026-11-05

The [five native observations](../research/performance/forge-program-20261105-outbound-mcp.json)
bind release commit `5669fb33d3db9dac9b59fbec4680e8ac4df9efc8`, executable
`00dc249c5ae27d8fb441da34ca4e56f05d22eec1a5daefbb1aac12e71b722ccb` and source
inventory `53016f2e460a71facc56f77d74b6e7ee3824e922c990837231cf519deddc9be6`.
Each fresh service exercised the actual inbound stdio adapter, persistent native
outbound client and a separate Rust server reading an explicitly granted file.
All 160 selected-tool calls and 195 recorded phases completed, with zero failed,
censored or unstarted observations. This feature had no equivalent earlier
outbound implementation; these are current costs, not measured speedups.

| Observation | Range across five runs |
| --- | --- |
| Fixture preparation, service launch and readiness | 52.2–61.5 ms |
| Adapter startup and initialization | 1.18–1.79 ms |
| Inbound tool catalog discovery | 37.9–52.6 µs |
| First selected-tool call, including endpoint launch and remote discovery | 11.8–22.7 ms |
| Warm transport p50, 31 calls per run | 1.259–1.333 ms |
| Warm transport p95, 31 calls per run | 1.322–1.400 ms |

Transport timing includes framing and JSON conversion; the independent response
oracle is timed separately. Pin validation before and after each result is included.
Service CPU increased by 40–50 ms over the measured call sequence at 10 ms kernel
tick resolution. Driver, adapter and endpoint CPU deltas rounded to zero ticks;
this does not mean zero work. Final observed RSS was 25.4–25.6 MB for the service,
16.3–16.6 MB for the adapter and 2.13–2.20 MB for the endpoint. These are process
snapshots, not allocation counts or whole-tree peaks. The endpoint PID comes from
the cooperative fixture, with stable process start ticks; it is not an independent
producer attestation. No model usage, cache usage, task success or billed cost was
observed. A first call is not an OS-cache-cold measurement.

Every successful run checked unchanged observed inputs, adapter EOF/exit, explicit
service stop, the durable cleanup disposition and positive absence of the exact
owned scratch/cgroup paths. An independent review rechecked hashes, accounting and
path absence. The test's 25-second deadline is cooperative; an external 60-second
watchdog bounded these invocations. Failure or watchdog termination is not cleanup
evidence. Earlier functional-test failures are retained separately in the artifact.

The [matched ordinary CLI control](../research/performance/forge-program-20261105-outbound-cli-control.json)
used 101 samples and five warmups per binary with the same prompt workload and
environment. P50 changed from 1.602 to 1.595 ms, p95 from 1.909 to 1.825 ms, median
CPU from 1.166 to 1.149 ms, and maximum RSS from 14.410 to 14.524 MB. The existing
10% regression budget passed; these small differences do not establish an
optimization. The baseline binary was the preserved `411d168` build of the
pre-outbound production implementation. Subsequent documentation and Linux test
guard changes are not assigned these measured executable identities.

Reproduce on the documented Linux/systemd host using a private report destination:

```sh
cargo +1.98.0 build --release --locked --offline --jobs 2 \
  --manifest-path native/forge/Cargo.toml --bin forge --example outbound_mcp_server
sha256sum native/forge/target/release/examples/outbound_mcp_server
FORGE_TEST_BIN="$PWD/native/forge/target/release/forge" \
FORGE_TEST_MCP_SERVER="$PWD/native/forge/target/release/examples/outbound_mcp_server" \
FORGE_TEST_MCP_SERVER_SHA256="<digest from the independently captured build>" \
FORGE_TEST_OUTBOUND_REPORT="/absolute/private/new-observation.json" \
timeout --signal=TERM --kill-after=5s 60s \
  cargo +1.98.0 test --release --locked --offline --jobs 2 \
  --manifest-path native/forge/Cargo.toml --test native_outbound_mcp \
  -- --exact outbound_current_roundtrip_observation --ignored --test-threads=1
```

Build the test driver with `--no-run` before timing when using Cargo; the recorded
runs invoked the already-built test executable directly. Use a fresh report path
for each repetition. The six functional scenarios use `--include-ignored --skip
outbound_current_roundtrip_observation` and the same explicit binary handoffs.

### Automatic Cargo test value — 2026-11-05

The [automatic fixture observations](../research/performance/forge-program-20261105-cargo-value.json)
bind a clean release build at `015e66e5b5dc27b529a7a35803bcb15568e88b15`, executable
SHA-256 `f05ca4dc19477739266206951b07a93ee3f3667551e0a62e76abc4eb4e0fcc3d`.
The writer and cargo-mutants 27.1.0 produced two killed mutants, one per selected
source, two passing independent baselines, complete attribution, and one `CORE`
test. The test was absent on the fixture's `main` and added on its topic branch.
Live and commit corpus audits were `CLEAN`; candidate `mutation-evidence` passed
with one covered test, zero missing tests and the new node admitted without a waiver.

This closes the bounded automatic-evidence gap observed in the earlier identity
trial below. **Overall candidate review still did not pass:** the fixture lacks
Agent trailers and local tree attestations, and implicit test selection lacks its
required evidence. Twelve checks completed, with zero failed/censored command
lifecycles and no failed expectations; expected refusal exit codes are retained.
Two checks were explicitly unstarted: a malformed-matrix engine scenario (no
fabricated receipt) and the driver-labelled independent CI-attestation check.
Forge has no hosted CI. Malformed matrices remain covered by the native validator
tests. Wrong declaration hashes, new Rust inputs, changed node IDs and stale
contracts each produced the expected campaign-specific refusal.

Three supervised commands ran: two Cargo baselines and one cargo-mutants invocation.
The receipt's generic `subprocess_scaling` field describes scaling, not an exact
process count; internal engine subprocess count was not measured. Actor wall
observations were 91, 16 and 420 ms; the complete run command took 0.815 seconds
including driver capture/wait overhead. These single observations are not latency
benchmarks. CPU and RSS were not sampled for the campaign; configured cgroup limits
are not usage measurements. All three owned cgroups, the runtime and snapshot were
absent after cleanup, independently checked along with receipt, source and test hashes.

The trial exposed and fixed two real integration defects: an ignored generated
campaign need not be present in the Git snapshot, and ordinary `std::fs::copy`
supplies harmless file-type bits with permissions. The confined-copy regression
failed before the narrow broker fix and passed afterwards; the broker still rejects
mismatched types, special modes and writes outside owned scratch. Earlier refused
attempts and successful earlier trials remain separately identified in the artifact.
The final trial also exercises native raw snapshot preparation, replacing inherited
Git subprocesses that could execute configured filters or hooks. Capture remains
point-in-time, not atomic. Positive observations refer to held and committed inputs;
the final live declaration was intentionally changed by the stale-contract negative.

A fresh matched release comparison against the pre-snapshot-change `e153067` binary used
101 successful samples and five warmups per arm, the same literal commands, input
bytes and cleared environment. All 404 samples completed; both comparisons pass
the unchanged 10% regression budget.

| Workload | Wall p50 before / after, ms | Wall p95 before / after, ms | CPU p50 before / after, ms | Max RSS before / after, bytes | Sealed comparison |
| --- | ---: | ---: | ---: | ---: | --- |
| CLI help | 1.808048 / 1.820759 | 2.101699 / 2.085699 | 1.296 / 1.292 | 12,378,112 / 12,357,632 | [help](../research/performance/forge-program-20261105-cargo-value-help-comparison.json) |
| Symbol context | 2.323860 / 2.337020 | 2.547811 / 2.544191 | 1.820 / 1.819 | 17,281,024 / 17,235,968 | [symbol](../research/performance/forge-program-20261105-cargo-value-symbol-comparison.json) |

The shared host and warm filesystem limit these observations to these workloads.
The earlier `b82da144` versus `e153067` comparison also passed across 404 samples;
its sealed receipts remain in commit `f99737d`. Its driver initially could not
overwrite its read-only private subject after the baseline; no current samples
had started. It verified and replaced that exact subject before the current arm.
The final comparison above completed without that orchestration failure.
The performance runner's ambient Rust probe is not a build attestation; both selected
binaries were built with Rust 1.98.0. Source pins bind the executed binaries; the
comparison's ambient HEAD does not rebuild or reattribute the older executable.

Reproduce the [documented declaration and automatic commands](mutation.md#opt-in-cargo-per-test-value-evidence)
with the artifact's exact fixture, tool pins and operator configuration. Original
scenario logs were parsed before cleanup; retained hashes and normalized observations
do not authenticate a producer. Private Python orchestrated this experiment, while
production Forge remained native. No model tokens, dollars, actual agent task success,
client adoption or general speedup were measured. Publication validation is recorded
separately in the [engineering status](roadmap.md).

### Installed native identity — 2026-11-05

The [machine-generated observations](../research/performance/forge-program-20261105-installed-native-identity.json)
record a real automatically generated cargo-mutants 27.1.0 campaign on a tiny
external-host fixture with no vendored Forge sources or Python on the child PATH.
Clean build commit: `3f65b1ee03ed493c1402c5c1e663a30cdd20f831`; the linked artifact
records the executable SHA-256. Its identity command supplied the operator pin
before receipt generation.

The engine killed two mutants, one in each selected source, and removed its snapshot.
Both live and commit corpus audits were `CLEAN` without a Python bundle or `uv.lock`.
Candidate receipt verification was `PASS`, with one covered test and no missing
tests, after adding the separately required matching candidate lock. Fourteen
negative cases refused missing/mismatched pins and locks, partial inventories or
source drift. All 22 final command attempts completed; none was censored or unstarted.

**Full candidate review did not pass.** This earlier automatic Cargo receipt lacks
complete per-test attribution and admitted test-value evidence, tracked in
[#190](https://github.com/mcpirate17/llm-forge/issues/190) and addressed by the separate
automatic-value trial above. The fixture also lacks CI
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
identities are retained in the linked comparison receipts.
Both ELF compiler comments report Rust 1.98.0; the receipt's ambient `rustc`
probe reports 1.93.1 and is not a build attestation. The host remains shared,
the filesystem warm and the process newly launched. These observations support
no material regression in these two workloads, not a general speedup or agent
productivity claim. Reproduce using the literal commands and pinned binaries in
the embedded receipts; never edit receipt identities to retrofit a match.

### Deferred next-inspection advice — 2026-11-05

**Decision: do not ship this candidate.** A deterministic optional `next_action`
field produced one existing `code_inspect` call from caller-supplied location
facts. It passed behavior checks, but the stronger control completed the same
inspection with less latency and fewer bytes. Independent review recommended
deferral; the implementation and its added discovery schema were removed from
production. This is an evaluated, rejected candidate, not an accepted optimization.

The [native MCP observations](../research/performance/forge-program-20261105-next-action-mcp-evaluation.json)
retain five current runs, each with one first use, five warmups and 31 measured
workflows per arm, in rotating order on the same warm service. All 740 workflows
and 1,480 RPCs completed, including warmups; 620 workflows were measured samples.
Each inspection checked the actual source hash, exact signature, line bounds and
absence of truncation. These are fixture retrieval checks, not coding tasks.

| Workflow | Calls | Per-run wall p50 range, µs | Per-run wall p95 range, µs | Request / response bytes per sampled workflow |
| --- | ---: | ---: | ---: | ---: |
| Known inspection directly | 1 | 58.391–68.000 | 64.880–89.220 | 165 / 1,107 |
| Plain prompt preparation, then known inspection | 2 | 124.031–144.241 | 135.991–210.282 | 358 / 3,948 |
| Prompt with advice, then advised inspection | 2 | 146.351–171.002 | 163.512–202.692 | 495 / 4,754 |
| Plain prompt, repeated discovery, then inspection | 3 | 243.412–264.413 | 264.752–336.583 | 419 / 10,634 |

Advice added 22–27 µs to the within-run median versus prompt plus known inspection,
and 943 complete JSONL frame bytes. The last arm repeats discovery after setup;
it is a diagnostic comparison, not a stronger client baseline. Discovery itself
grew from 5,408 to 6,684 bytes; the prompt tool's schema grew from 472 to 1,748
compact JSON bytes. Every context-capability client would bear that schema cost.
No provider input/output/cache tokens, task success or dollar savings were measured.

Workflow timings include local validation and measurement bookkeeping. First use
occurs after other harness workloads; it is not cold process or cold filesystem
evidence. Separate setup observations span 33.7–42.4 ms for fixtures, 41.4–44.7 ms
for service launch/readiness, and 1.81–2.60 ms for adapter setup (including
0.336–0.348 ms discovery). During the whole four-arm interval, CPU deltas at
10 ms tick resolution were 0–10 ms each for driver and adapter, and 10–20 ms for
service. Observed RSS ranges were 6.34–7.28, 16.27–16.54 and 22.79–23.25 MB,
respectively. These are process snapshots, not per-arm costs or a summed tree
peak. Allocations and cold-host measurements are unavailable.

The initial attempt was refused before service launch: the fixture's 512 MiB
reservation could not admit the context worker. The corrected fixture reserves
1,024 MiB under the unchanged admission rule. Its five successful runs verified
adapter EOF/exit, service stop, fixture removal and unchanged observed inputs.
The failed prelaunch fixture remains at `/tmp/forge-hbench-1182259`. A separate
old-binary run correctly reports the fused arm unavailable (37 unstarted), not
PASS; its other arms and cleanup are retained. Per-process fixture paths differ,
so no cross-binary MCP latency improvement is claimed.

The [sealed CLI control](../research/performance/forge-program-20261105-next-action-cli-comparison.json)
compares the unchanged plan request with 101 samples and five warmups per binary.
Wall p50 was 1.637178 → 1.636747 ms, p95 1.832778 → 1.855489 ms; CPU p50 was
1.161 → 1.171 ms and maximum child RSS 14,372,864 → 14,655,488 bytes. The original
10% budget passed. All four absent-field CLI modes also returned byte-identical
outputs. This regression control does not justify the advice's additional cost.

Experiment implementation/tests are preserved at
[`ee571e3`](https://github.com/mcpirate17/llm-forge/commit/ee571e3919ebed808dcdc63bf8940f0ebc5eabb0),
with the benchmark reservation correction at
[`dd79a75`](https://github.com/mcpirate17/llm-forge/commit/dd79a758a68a2d7824030259b17cf2ec7b7800e6).
The measured executable is the clean `ee571e3` build, SHA-256
`cd5c6682d38aebc6404263003318179afcd8ab9c9aed503fcde13e386b3d5f24`;
the benchmark observes the `dd79a75` source tree. The projection binds executable,
driver, build-input, fixture and raw-report hashes. At that experiment revision,
build the release CLI and `cargo +1.98.0 bench --manifest-path native/forge/Cargo.toml
--locked --offline --bench harness --no-run`; execute the resulting harness with
`FORGE_BENCH_SERVICE=1` and an absolute `FORGE_TEST_BIN`. The source includes
cooperative deadlines and owned cleanup; terminating a wrapper is not cleanup
proof. These revisions are historical experiment code, not the current interface.

The candidate passed 26 focused tests and seven release adapter tests, including
three explicitly enabled MCP tests. Held-out inspection cases covered qualified
names, Unicode byte bounds, missing files, empty matches and truncation. Strict
Clippy, formatting and source-size checks passed. Future recommendation work needs
prospective tasks where guidance can change a real decision, with a capable
baseline client and complete task, token, latency and failure accounting.

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

Executable hashes are retained in the linked comparisons. Shared-host distributions
support no material regression in these two commands, not a general speedup.
The new operation's output is larger
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

### Ordinary Cargo receipt freshness — 2026-11-05

The bounded [before-fix observations](../research/performance/forge-program-20261105-supporting-input-freshness.json)
and [fixed release validation](../research/performance/forge-program-20261105-supporting-input-validation.json)
measure correctness of later receipt admission. Each fixture uses a real automatic
2-kill/PASS campaign. Selected mutation source/test, campaign and receipt bytes stay
fixed while authored supporting inputs change independently.

| Supporting input changed | Before, live / committed | Fixed, live / committed | Fixed after exact restoration |
| --- | --- | --- | --- |
| Unpinned library bytes | Accepted / accepted | Rejected / rejected | Accepted / accepted |
| Cargo edition | Accepted / accepted | Rejected / rejected | Accepted / accepted |
| Cargo.lock format version | Unmeasured | Rejected / rejected | Accepted / accepted |
| Added Rust binary | Accepted / accepted | Rejected / rejected | Accepted / accepted |

The fixed release is `f07bd1f`, binary SHA-256
`942f3fc9881d621a60e59be23c9f0b15ce5ef941e654556aa7dcdf8e965c56f9`.
The compact artifact references the earlier successful `9451943` trial as well.
All 43 phases and 114 commands completed, including expected nonzero refusals,
without failures or censored/unstarted work. Baseline and restoration require both CLEAN and receipt acceptance; all
eight drift checks require the specific supporting-input change refusal. Owned
snapshot/cgroup absence and evidence hashes were independently checked.

This is discovery-input freshness evidence. The unused helper and lock-format
probes do not demonstrate changed program behavior or dependency resolution.
Non-Rust data and full compiler/environment closure are unmeasured. The experiment
does not measure a latency distribution, incremental scan cost, CPU/RSS, model
usage or agent productivity. No speed or cost improvement is attributed to it.
The [engineering status](roadmap.md) tracks the separate release checks and their
failures; this successful experiment alone is not release validation.

### Ordinary Cargo confinement update — 2026-11-05

The earlier ordinary-engine measurements below used the then-current runner and
process-group supervision. They do not establish owned descendant cleanup or
current native receipt admission. The current implementation requires explicit
[execution grants](mutation.md#ordinary-cargo-execution-grants) and supports a
narrow stdlib-only root Cargo profile. Clean release `adec2a6` passed the normal
real-engine fixture (two kills/PASS), and the mixed-target fixture preserved both
mutants (two survivors/RATCHET_HELD, not PASS). The old control falsely killed
both via the unpinned test. [Recorded observations](../research/performance/forge-program-20261105-owned-mutation.json)
retain the prior permission refusal, missing-linker-helper baseline failure and
dirty-scope refusal; no failed attempt is discarded.

Mutation workflow timing remains unmeasured by a valid matched benchmark. Review
rejected an unexecuted proposed harness because killing its wrapper on timeout
could leave detached engine descendants. Individual receipt durations are
orchestration observations, not latency distributions or isolated confinement
cost. No token, allocation, model-cost, task-success or productivity claim follows
from these fixtures. The first full publication gate at `abebaa1` failed the
legacy CLI fixture tests (28 failures); the other 17 steps passed. That is not
release validation. See the [current status](roadmap.md) for the preserved receipt
and test migration. The migrated suite subsequently passed all 15 CLI cases
(including 10 explicitly delegated cases). The repeat full gate and local-verify
passed at `0674d34` (18 steps; 4,623 passing executions, zero failures and 172
ignored across 177 harnesses); 45 explicit release executions then passed with
zero ignored. Fresh ordinary and Cargo-value automatic trials reproduced the
recorded outcomes from that source. The compact evidence retains all 11 attempts.
These are correctness and execution-validation results, not measured workflow
speed or agent effectiveness.

Matched read-only release checks compare `015e66e` with `adec2a6`, using the same
binary pathname, command, input file, environment and unchanged 10% regression
budget. Each arm/workload has 101 measured samples after five warmups: 404 measured
executions, zero failures. These are process-startup measurements with warmed
filesystem caches; cold-cache behavior and concurrent workloads were not measured.

| Workload | p50 ms, before → after | p95 ms, before → after | CPU p50 ms, before → after | Maximum RSS bytes, before → after |
| --- | --- | --- | --- | --- |
| CLI help | 1.794 → 1.815 | 1.970 → 1.955 | 1.281 → 1.296 | 12,439,552 → 12,288,000 |
| Bounded symbol context | 2.340 → 2.358 | 2.519 → 2.737 | 1.821 → 1.851 | 17,174,528 → 17,313,792 |

Both comparisons passed the existing budget. The symbol p95 increased 8.64%; this
is a measured difference in one batch, not evidence of a speed improvement.
Sealed [help receipts](../research/performance/forge-program-20261105-owned-mutation-help-comparison.json)
and [symbol receipts](../research/performance/forge-program-20261105-owned-mutation-symbol-comparison.json)
retain individual samples and exact workload/source identities. These read-only
checks do not measure mutation-engine overhead.

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

## Confined Fest validation — active, 2026-11-06

The explicit Fest profile is under validation; no mutation PASS or speedup is
claimed. Two release attempts refused before payload launch: an incorrectly
ordered operator inventory, then a reserved sandbox environment override.
A debug attempt executed one pytest baseline command successfully (561 ms in
its command ledger), then refused when coverage cleanup removed its own config.
The snapshot and empty cache were retained, and the owned command cgroup was
confirmed absent. This is partial correctness evidence, not benchmark evidence.

The [evidence index](../research/performance/forge-program-20261106-confined-fest.json)
retains these failed attempts, build identities and unresolved validation work.
No agent task-success, token-cost, clean-host or installation benefit is inferred.
