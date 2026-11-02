# Real-code retrieval measurement at 6630b37

This CPU-only run completed all **144 requested retrievals**, with **0 failed,
0 censored and 0 unstarted**, within its 1800-second deadline. It measures the
native implementation at `6630b3705b2d2cfb9295cbee8e2375607cf4d96e`, not subsequent
changes. The [compact evidence](benchmarks/native-retrieval-6630b37.json) retains
the exact recipe, source inventory, labels, per-query observations and result
identities without duplicating retrieved source text. The
[benchmark contract](real-code-retrieval-benchmark.md) defines the metrics.

## Scope and provenance

The corpus contains 25 labeled files plus 100 deterministic distractor files:
333 chunks and 996,263 source bytes. All 24 query texts, IDs and labels were
frozen before retrieval. Luna authored the labels from source inspection; they
are agent-authored judgments, not independently adjudicated human ground truth.
Eight queries span multiple modules. Rankings were not used to revise labels.

An earlier preparation attempt timed out before any retrieval, leaving all 144
operations unstarted. Before any rankings existed, the retry adjusted batch
limits and rebound only the changed extraction source and q11's corresponding
line spans. It retained the queries, symbols, rationale and corpus paths. The
prior fixture remains in commit `674d258`; the successful fixture revision is
`674d258ad2dc9653b883a2923f40932178a49798+embedding-batch-controls-v2`.

The run used `qwen3-embed-cpu:latest`, artifact
`fdf7067d896175e871d925acacde4df653a1e6900a946710342ce972372dcf81`,
1024 dimensions, context 8192, two model threads and zero GPU layers. Batches
admitted at most two inputs and 8192 UTF-8 bytes. Each request retained its
60-second deadline. The host was an AMD Ryzen 9 9950X3D with 32 logical CPUs;
the server reported Ollama 0.32.14 when inspected after the run.

## Measured results

Each mode ran the same 24 queries with a newly opened reader and then with a
reused reader. Relevance counts were identical across those two passes.

| Mode | Path hit@10 | Span hit@10 | All label spans hit@10 | Cross-module span hits | Warm p50 | Warm p95 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| BM25 | 22/24 | 22/24 | 20/24 | 7/8 | 461.023 us | 546.103 us |
| Hybrid exact | 24/24 | 22/24 | 20/24 | 6/8 | 476.323 us | 545.843 us |
| Hybrid HNSW | 24/24 | 22/24 | 20/24 | 6/8 | 497.523 us | 567.713 us |

HNSW's raw vector top-ten recall against exact cosine search was 1.0 for every
query at ef=256. This vector agreement is separate from semantic relevance.
Hybrid search improved path hits on this fixture, tied aggregate span hits,
and lost one cross-module span hit. These mixed results do not establish a
general quality improvement.

Total elapsed time was 1417.110 seconds. Source embedding and index refresh
took 1413.823 seconds across 167 batches, with 239,087 model-reported input
tokens. The 24 query embeddings took 2.923 seconds across 12 batches and 454
input tokens. Embedding preparation dominates this run; the retrieval
measurements alone cannot establish end-to-end interactive latency.

Client peak RSS was 29,320 KiB, excluding the independently operated model
server. Samples observed the server at roughly 1.57 GB RSS, two CPU cores and
zero VRAM. Those samples are not exclusive whole-run resource accounting.

First-use query p50/p95 values were 466.683/569.084 us for BM25,
473.433/564.303 us for hybrid exact, and 494.943/589.534 us for hybrid HNSW.
First use means a new reader; OS page cache and model warmth were uncontrolled,
and the raw-vector comparison ran first. Query timings include production
search, source verification, relevance accounting and JSON serialization.
This is neither a cold-host study nor a before/after optimization comparison.
It supplies no measured case for a custom GPU kernel.

## Retained artifacts

The compact artifact contains 144 query observations, all 125 file bindings,
the 24-query workload and exact operator recipe. Hashes bind the full raw
report and journal retained under the operator's native-migration state
directory; those raw files contain repeated source chunks and are not copied
into this repository.

| Artifact | SHA-256 |
| --- | --- |
| Full report | `ef408adf696d8b914502260e47c1b1cbfb39c8f827dfa216a620b481f7107e57` |
| Journal | `92ec803b03f3eee3cdcaff1fe153d8ca86b0a5a6f012717d16280ed58d39c7de` |
| Benchmark executable | `0d8e0243325c10ff412cf1273bfec81e8f0fa38e7ee7705359300375fdd965bf` |
| Source manifest | `73c4ae00f6908cf7ec0c2a9d168b0aa32e38c60e4dd7037424f76b37b3e14974` |
| Workload | `f1b856af49a4af8cc1f9126448b07ddb58978d0e92d99edba6a253f8632a462b` |

Reproducing the measured tree requires the named commit, original model
artifact and frozen recipe/workload. Later source revisions need a new
prospective fixture binding and separate results.
