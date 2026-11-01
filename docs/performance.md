# Measuring performance

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
