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
  --manifest-path native/forge/Cargo.toml --bin forge -- --help
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
