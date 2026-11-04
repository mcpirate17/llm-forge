# Native product value: bounded measurements

This report records three narrow observations: a Python/native relocation
comparison at `ef50dcb`, a native/native optimization comparison from
`ef50dcb` to `0cb1100`, and a real-source signature projection at `ef50dcb`.
They measure isolated operations and tokenized payload size. They do not measure
agent task completion, answer quality, project-wide speed, or net token/cost
savings.

## Evidence and executable identities

The retained result JSON files in
[`benchmarks/product-value/`](benchmarks/product-value/) are the primary
machine-readable observations. The portable measurement programs and their
current source hashes are in [`../native/product-value/`](../native/product-value/);
their invocation and build instructions are in that directory's
[README](../native/product-value/README.md). The portable package is a new,
separately versioned reproduction tool. Its source or executable identity must
not be substituted for the original harness that generated the historical
results.

| Run | Source and executable identity |
| --- | --- |
| Python/native and signature lookup | Forge checkout `ef50dcb5629bcffab02683319c45b7a7683e9bc3`; Forge ELF SHA-256 `b31e01e7e67cc1e58d383cec2811f18b7c113c6b8c1bb4ad3ee60e4afa685e1f` (`forge 0.8.21 (git ef50dcb)`). Python 3.12.3 ELF SHA-256 `e50d468e8b0adfb05733f5b87b3cff34829c4a8c1aea50c865aa8bdfe4bb150f`. The retired Python reference was blob `4cd99821a4c6d9f9ddc4e9c3746f3dd0c4189edb`, source SHA-256 `9015359e74c9fcd17a7bd898665ec6acbb610e088aa56b9e72defefebdf888cb`; it lived only in a temporary owned fixture and was removed after measurement. Original harness ELF SHA-256: `a7d71bae632a1f9c91d86a3ea85eb2275c667412df463b7c529f63cb44262e3d`. |
| Native optimization | Baseline Forge ELF is the `ef50dcb` binary above. Candidate checkout `0cb1100f7eec9e1c3a2f5b8ef62f73054023582f` (tree `5f7bea9c917efafe4ceb4a3e6051e9e8f64e536c`) produced Forge ELF SHA-256 `15726d5689d19c41d3f0a63db59e9c6778a6841c2ad532125cf3a22e83df8d8b`; its source fingerprint is `5b95e72d5643b97537daa0491eeb33691e5709a4112d78cc439e69dbb32edccc`. The build record is an observation, not cryptographic execution attestation. The comparison binds the selected `native/forge/src/worktree_runtime*.rs` family, not the whole shipping tree. |

The original harness source inventory is retained in
[`original-harness-sources.json`](benchmarks/product-value/original-harness-sources.json).
The portable runner's present sources have different hashes and live under
`native/product-value/`. Archived `profile-ef50dcb.json` and
`profile-0cb1100.json` are summaries that refer to external trace/build context;
they are not self-contained raw traces or a fresh executable build. Treat them
as diagnostic records only.

## Method and results

The relocation fixture contained exact existing-environment/script bytes and
modes, including hardlinks, mixed LF/CRLF shebangs and a symlink. Each size used
three warmups and 21 alternating measured pairs per implementation. Wall time
includes child CLI startup and task execution; fixture setup, validation and
separate idempotence replay are outside the timed interval. CPU and maximum RSS
are child measurements via Linux `wait4`. Correctness checked source
preservation/inodes, copy-on-write, destination bytes/modes, skipped entries and
idempotence. All 96 timed attempts (including warmups) and all 96 separate
untimed idempotence attempts completed; none failed, were censored or remained
unstarted. This was a shared, unisolated host. The native path performs
additional pinning, locking, bounded reads and safe publication work, so this
is a comparison of accepted tasks with differing safeguards, not a pure
language-runtime test.

| Comparison | Workload | Median wall (p95), ms | Median CPU, ms | Maximum child RSS, MiB | Reading |
| --- | ---: | ---: | ---: | ---: | --- |
| Python vs native at `ef50dcb` | 16 scripts | Python 15.749 (17.215); native 21.677 (22.603) | 15.684; 3.372 | 12.90; 14.29 | Native wall median was 1.38× Python. |
| Python vs native at `ef50dcb` | 256 scripts | Python 30.011 (30.910); native 321.717 (334.870) | 29.932; 33.614 | 13.42; 20.60 | Native wall median was 10.72× Python. This maintenance workload was slower in the native implementation. |
| Native `ef50dcb` vs `0cb1100` | 16 scripts | 21.397 (21.870); 20.815 (22.512) | 3.214; 2.617 | 14.54; 15.02 | Small wall change in this run. |
| Native `ef50dcb` vs `0cb1100` | 256 scripts | 321.040 (329.365); 315.810 (322.695) | 34.245; 24.061 | 20.71; 21.34 | CPU median fell 29.74%; wall median fell 1.63%. Do not infer a stable wall speedup. |

In the native/native 256-script run, directory opens fell from 31,717 to 1,583;
all 768 fsyncs remained. These counts are consistent with the optimization's
reduced directory probing, but traces are diagnostic and timing perturbation
prevents attributing an exact share of latency to individual calls. Peak RSS
rose from 20.711 to 21.340 MiB. No post-optimization Python comparison was run.

For the real-source context observation, the file
`native/forge/src/worktree_runtime.rs` at `ef50dcb` was 5,712 bytes and 2,455
tokens under the pinned GPT-2 tokenizer; the complete JSON projection for
`relocate_console_scripts` was 1,140 bytes and 383 tokens. That is 2,072 fewer
tokens (84.4%) for this signature question. The response's source hash was
`7e4fefae55c8c48118c85f91c075ba156f462bdc69069ec8c94f58450cdd886e`, and graph
lookup was disabled. Tokenizer SHA-256 was
`8414cab924d8b9b33013f0d221c5862f365ee9be39c5c2bfae8a5a9e970478a6`; the
configured model hash was the synthetic `0707…0707` sentinel. No model weights
were loaded and no deployed-model identity, billing, task quality, or full-file
semantic equivalence was tested. The projection omits function bodies.

## Reproduction and limits

Use the commands in [`../native/product-value/README.md`](../native/product-value/README.md).
The Python/native replay requires Git history containing the exact retired
reference blob; it does not download or reconstruct a substitute baseline. A
fresh run must pin its own Forge and Python executable hashes, source checkout,
tokenizer artifact and fixture/output paths. Fresh binaries and source hashes
are new evidence and must not be assigned the historical results above. The
native/native comparison similarly requires independently pinned baseline and
candidate binaries and retained build receipts.

These measurements support only the stated fixture-level findings: this native
relocation path was slower than its Python reference at both tested sizes, a
later native revision reduced CPU work with little observed wall-time change at
256 scripts, and signature-only projection reduced one pinned token count.
They do not establish global speedup, faster verified tasks, answer quality,
provider savings, or general token savings. The archived traces are summaries,
and the shared-host observations are sensitive to host load and environment.
