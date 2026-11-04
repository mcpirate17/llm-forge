# What Forge helps with, and what we have measured

Measurements below identify their source revisions and workloads. They do not
certify subsequent code or establish a general speedup. An implementation or test
count is not a performance result.

## Does moving the implementation to Rust improve performance?

It removes Python interpreter/environment requirements from the native runtime
path and enables shared native parsers, state, caches and process supervision.
Those are concrete packaging and implementation properties. Their net effect on
a particular tool call still requires measurement: file reads, fsync, subprocess
startup, model inference and additional correctness checks can dominate.

The evidence establishes improvements in some **native algorithms and
repeated-input caches**. A matched console-launcher migration benchmark found
slower native wall time. No general native-versus-Python speedup, agent-wide
token-savings percentage or faster verified task completion is established.
A port can cost more on a small workload if it adds safeguards; report that result
rather than comparing unequal work or dropping inconvenient samples.

## What functionality is available, and why use it?

| User job | Concrete shipped capability | Benefit to test in a real workflow |
| --- | --- | --- |
| Read unfamiliar code | Bounded AST facts/folds, caller/callee context and source-bound retrieval | Fewer irrelevant bytes returned; fewer whole-file rereads |
| Make a precise change | Expected-hash editing, exact match/diff controls, syntax validation, dry-run and guarded recovery | Less accidental replacement or stale-source editing |
| Continue a long-running command | Persistent PTYs, output cursors, stable session/write identifiers and durable task records | Fewer duplicate launches or lost outputs after reconnect |
| Stay within context limits | Pinned BPE counting, protected groups, deterministic compaction and recoverable originals | Smaller supplied context with fewer harmful omissions |
| Enforce project rules | Client-specific native hooks plus policy/source/resource checks | Consistent checks without requiring the model to remember each one |
| Inspect what actually happened | Scoped review/check/performance receipts, explicit missing/failed observations | More reviewable evidence and less ambiguous completion reporting |

These are current product capabilities, not a claim that every capability was
newly invented by the migration. The native migration changes ownership and
runtime dependencies. Persistent service orchestration, recovery and capability
boundaries also provide functionality beyond replacing a Python function.
The product's value depends on clients actually calling these paths.

## Measurements available now

| Evidence | Recorded observation | What it does and does not show |
| --- | --- | --- |
| Real-source signature lookup at `ef50dcb` | Full file 2,455 tokens → complete JSON projection 383 tokens: 2,072 fewer, or 84.4% | Pinned GPT-2 tokenizer, source hash and requested signature checked. Function bodies are omitted. This is useful for a signature question, not an equivalent full-file read or proof of task success. |
| Python/native relocation at `ef50dcb`, 16 scripts | Python median/p95 15.75/17.22 ms; native 21.68/22.60 ms | Native wall time was 1.38× as high. The same normal-case output was checked; native checks and durability work differ. Includes process startup. |
| Same comparison, 256 scripts | Python median/p95 30.01/30.91 ms; native 321.72/334.87 ms | Native wall time was 10.72× as high. This maintenance operation is a measured slowdown, not evidence for a general language-runtime comparison. Profiling is separate from the original timings. |
| Native optimization, `ef50dcb` → `0cb1100`, 256 scripts | CPU median 34.245 → 24.061 ms; wall median 321.040 → 315.810 ms | 29.74% lower CPU in this paired run; only 1.63% lower wall time, with no stable-speedup inference. Directory opens fell from 31,717 to 1,583; all 768 fsyncs remained. Maximum child RSS rose from 20.711 to 21.340 MiB. |
| Context projection fixture | 57,430 input → 3,277 rendered tokens, separate 512-token reserve; 29 old groups dropped | 54,153 fewer rendered tokens, 94.3%, for one pinned GPT-2 tokenizer fixture. Not a general savings rate, provider bill or task-quality result. |
| Repeated context compaction | Same repeated fixture 75.87 ms before count-cache change → 2.65 ms with verified recovery reuse; first final compaction 63.47 ms in one sample | Native cache improvement on repeated input. Neither cold-service nor Python/native comparison. |
| Real-code retrieval at `6630b37` | 144/144 completed; 0 failed/censored/unstarted. Warm p50 BM25 0.461 ms, exact hybrid 0.476 ms, HNSW hybrid 0.498 ms | Includes search, source verification, relevance accounting and JSON. Targeted 24-query agent-labelled fixture, not broad human-adjudicated relevance or agent success. |
| Retrieval preparation, same run | Index/embedding refresh 1,413.823 s (23.6 min), query embedding 2.923 s; 239,087 + 454 model-reported embedding input tokens | Preparation dominates that run. Embedding tokens are separate work, not prompt tokens saved. Cached reader timing alone cannot describe end-to-end search latency. |
| Dense native scan optimization | Secret scan median 30.597 → 2.111 ms; native scan 19.449 → 1.865 ms | Paired native component workloads producing 4,000 findings. Clean cases changed little; no whole-review or language-port speedup. |
| Casefold fast path | Reported component ratios 9.23×/3.00×/4.55× across three corpora; integrity workload stayed near 173–174 µs | Lower cost for that primitive under one unisolated before/after pair, not an application-wide speed result. |

Sources: [paired relocation, signature lookup and tracing](native-product-value-measurements.md),
[context measurements](native-context.md#release-measurements),
[real-code retrieval](native-code-retrieval-measurement.md) and its
[compact artifact](benchmarks/native-retrieval-6630b37.json),
[component measurement contracts](performance.md). Relevance was mixed:
hybrid improved path hits on that fixture, tied aggregate span hits and lost one
cross-module span hit. HNSW was not faster at this size. No new result should
quietly inherit these historical binaries' performance claims.

The relocation run used three warmups and 21 alternating pairs for each size.
All 96 planned calls, including warmups, and 96 separate correctness/idempotence
replays completed; none failed, were censored or remained unstarted. It checked
hardlink copy-on-write, source bytes/inodes, destination contents/modes, skipped
entries and idempotence. The retired Python source existed only in an owned
temporary benchmark fixture and was removed after the run. CPU/RSS and raw samples
are retained with source, executable and fixture identities. This was a shared
host, not an isolated performance laboratory.

The signature query used `native/forge/src/worktree_runtime.rs` at `ef50dcb`,
with graph lookup disabled. The 1,140-byte JSON response was counted in full,
against the 5,712-byte file. Only the tokenizer artifact was verified: no model
weights were loaded, and the tokenizer test configuration's model association
was a synthetic fixture marker, not a weight hash or deployed-model identity.

## Comparisons still needed

Additional comparisons must record source/binary identities, workload, host/load,
warmups, samples and complete outcome totals. Missing controls remain visible.

| Comparison | Required reporting | Current audit status |
| --- | --- | --- |
| Old Python vs native console-launcher relocation, first application | Same synthetic existing-env/script bytes and mode/COW outputs; subprocess wall p50/p95, CPU, RSS, failures; include interpreter startup and extra native guard/fsync work | Original slowdown retained; subsequent native/native run reduced CPU, with small wall-time changes. No post-optimization Python comparison was run. |
| Same APIs, idempotent rerun | Existing relocated bytes, zero rewrites; keep setup outside timed scope and distinguish cold/new process from persistent code | Correctness replays completed; not the primary latency comparison |
| Prompt/context fixture projection | Exact supplied input/output and tokenizer identity, retained/dropped groups, recovery round-trip, runtime | Historical compaction and current real-source signature counts above; task-quality controls remain missing |
| Persistent MCP vs direct CLI | Same tool/outputs, cold service+adapter setup separately from warm calls, catalog/policy/transport included as declared | **NOT YET MEASURED** |
| Hooks enabled vs disabled | Matched event sequence, per-event p50/p95, injected provider tokens, denied/failed events and tool-task outcomes | **NOT YET MEASURED** |
| Agent task with vs without Forge | Same model/budget/repo/task, verified patch outcomes, wall time, actual usage, repeated trials and failed/censored cases | **NOT YET MEASURED** |

A relocation comparison is narrow but useful. Its correctness checks must retain
hardlink COW, source preservation, destination mode, CRLF/body bytes and
idempotence. Native-only link/locking/exchange safeguards are additional work;
the normal-case output match does not imply identical security semantics.

## Responsible token, cost and time estimates

For an exact pinned tokenizer and supplied text:

```text
gross rendered-token reduction = input_count - output_count
projection reduction fraction = 1 - output_count / input_count
```

Include separators, markers and any actually rendered framing. Report reserved
output budget separately. Do not substitute graph CLI's conservative byte upper
bound for a model-tokenizer count, or treat provider cached-input tokens as
uncached billed input.

For a proposed deployment, two first-order estimates are possible only after
measuring that deployment's inputs:

```text
estimated prompt-cost delta = changed billed tokens × applicable billed rate
estimated prefill-time delta ≈ changed prefill tokens / measured prefill rate
```

These are conditional estimates, not current Forge results. Cache pricing,
output tokens, extra retrieval/embedding work, model batching, nonlinear context
costs and rereads can change or reverse the net benefit. Never convert a 94.3%
fixture projection into “94.3% cheaper” or “94.3% faster.” Measure actual provider
usage and time to a verified result to answer those questions.

For example, the measured signature projection omitted 2,072 tokens. At an
**assumed** uncached prefill rate of 1,000 tokens/second, that represents about
2.1 seconds of input processing; at an assumed 10,000 tokens/second, about
0.21 seconds. These illustrate the formula, not a measured model or expected
hardware range. Cached prompts, extra retrieval and later rereads can reduce
or erase the net saving.

## Adoption gaps that constrain the product claim

- A binary install does not configure MCP, start a harness, trust hooks or teach
  an agent when to discover a tool. Client-visible catalog and real calls must
  be verified after setup.
- The strict persistent harness needs compatible Linux/systemd/cgroup controls,
  an explicit policy and selected executables. A source-inspection-only trial is
  easier than full execution deployment.
- Hook support is client/surface-specific. Claude/Codex guard adapters and
  Qwen/Grok startup messaging are not equivalent coverage; local command hooks
  are not automatically supported by cloud orchestration.
- Tokenizer/model identity and optional embedding endpoints require operator
  setup. Local tools can still return source to a remote agent provider.
- External schema-9 graph consumers and direct host Make callers remain a
  separate cutover. Graph seed/shared-publication and reuse-analysis drafts are
  not released capabilities and must not appear as completed value.
- Historical correctness/gate receipts are scoped evidence. They do not prove
  active client adoption or downstream model efficacy.

The most credible product promise today is **bounded, inspectable local tools
that an existing coding agent can actually use**. Better task outcomes and lower
net spend remain hypotheses until matched workflow measurements establish them.
