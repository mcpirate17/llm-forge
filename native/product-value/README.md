# Bounded product-value measurements

Linux-only native benchmark tools, separate from the shipping Forge CLI. They
reuse conductor-native sample summaries and the existing bounded fixture/process
runner. There is no production Python dependency: the original comparison uses
an explicitly selected Python executable solely as a reference guest, retrieving
one exact retired Git blob into an owned temporary file and removing it afterward.
No Python source is archived in this package. Git history containing blob
`4cd99821a4c6d9f9ddc4e9c3746f3dd0c4189edb` is a prerequisite; there is no download
or replacement baseline if it is missing.

Build from a clean source checkout after other compiler load has ended. Select an
external writable output directory `OUT`, absolute source checkout `REPO`, pinned
absolute native Forge binary `FORGE`, Python executable `PYTHON`, and native Git
`GIT`. Record their SHA-256 values as `FORGE_SHA` and `PYTHON_SHA`, along with build
receipts. The tools clear child environments and receive tool paths as arguments.
The package uses a relative conductor-native dependency and its own lockfile;
it is not added to shipping/default Forge workspace members.

```sh
CARGO_BUILD_JOBS=2 CUDA_VISIBLE_DEVICES='' CARGO_TARGET_DIR="$OUT/target" \
  cargo +1.98.0 build --release --locked --manifest-path native/product-value/Cargo.toml \
  --bins --jobs 2
```

Use `--offline` when the lockfile's dependencies are already cached. Build first,
then execute binaries separately: the measurement tools sample `/proc` for
competing compiler names at admission and between attempts. These checks refuse
observed compilers but cannot establish continuous exclusion of compiler load.
Historical results and their exact source/runtime identities are in
[the measurement report](../../docs/native-product-value-measurements.md).
This portable package has a different source identity from the original external
harness; do not assign historical measurements to its newly built executable.

## Python/native and real-source context

```sh
CUDA_VISIBLE_DEVICES='' "$OUT/target/release/forge-product-value" \
  --repo "$REPO" --git "$GIT" --forge "$FORGE" --forge-sha256 "$FORGE_SHA" \
  --python "$PYTHON" --python-sha256 "$PYTHON_SHA" \
  --scratch-parent "$OUT" --output "$OUT/paired.json" \
  --warmups 3 --samples 21 \
  --tokenizer-config "$TOKENIZER_CONFIG" \
  --tokenizer-name 'GPT2 artifact-pinned; no weights or billing'
```

The Forge version must identify the selected checkout HEAD. The default real
source is `native/forge/src/worktree_runtime.rs`, symbol
`relocate_console_scripts`; that workflow checks the signature's parameter names,
source hash and graph-disabled status. It compares complete emitted JSON with the
raw file. Token counts require an explicit tokenizer JSON config, for example:

```json
{
  "path": "/absolute/path/to/tokenizer.json",
  "tokenizer_sha256": "8414cab924d8b9b33013f0d221c5862f365ee9be39c5c2bfae8a5a9e970478a6",
  "model_sha256": "0707070707070707070707070707070707070707070707070707070707070707",
  "add_special_tokens": false
}
```

The 0707 association is a **synthetic fixture sentinel**, not a model-weight hash.
No weights are loaded; exact artifact-pinned GPT2 counts establish neither deployed
model identity nor billing. Without config, counts are unavailable; no estimates
replace them. Independent context reproduction also uses the shipped commands:

```sh
"$FORGE" code --root "$REPO" context --symbol relocate_console_scripts \
  --no-graph --json --expected-hash "$SOURCE_SHA" -- \
  native/forge/src/worktree_runtime.rs > "$OUT/projection.json"
"$FORGE" context tokens --tokenizer "$TOKENIZER_CONFIG" \
  --input "$REPO/native/forge/src/worktree_runtime.rs"
"$FORGE" context tokens --tokenizer "$TOKENIZER_CONFIG" --input "$OUT/projection.json"
```

Published original context corpus is ef50dcb, SHA
`7e4fefae55c8c48118c85f91c075ba156f462bdc69069ec8c94f58450cdd886e`.
Exact original replay needs that existing source checkout and matching Forge ELF;
a newer source produces a fresh, separately labelled corpus. Do not overwrite
original evidence or claim identical counts across source revisions. The existing
`native/forge/benches/context.rs` is the separate in-process compaction benchmark;
this package does not replace it.

## Native/native

Select baseline and candidate ELFs with independent build/source receipts and
explicit SHA-256 pins. The source inventory compares every immediate
`native/forge/src/worktree_runtime*.rs` current file with ef50dcb, including added
and deleted paths. This is a selected family, not the whole shipping fingerprint.

```sh
"$OUT/target/release/compare_native_relocation" --repo "$REPO" --git "$GIT" \
  --baseline "$BASELINE_ELF" --baseline-sha256 "$BASELINE_SHA" \
  --current "$CANDIDATE_ELF" --current-sha256 "$CANDIDATE_SHA" \
  --scratch-parent "$OUT" --output "$OUT/native-paired.json"
```

Exactly three warmup and 21 measured pairs per size alternate baseline/candidate
order. Both variants must preserve the fsync/exchange/publication safety contract;
independent source review and refusal/race controls establish that, not timings.

## Four-case tracing

First create paired evidence on the same host/scratch parent. `STRACE` is an
explicit absolute native strace path; `PAIRED_SHA` hashes that fresh `paired.json`.

```sh
"$OUT/target/release/profile_relocation" --repo "$REPO" --git "$GIT" \
  --strace "$STRACE" --forge "$FORGE" --forge-sha256 "$FORGE_SHA" \
  --python "$PYTHON" --python-sha256 "$PYTHON_SHA" \
  --paired-results "$OUT/paired.json" --paired-sha256 "$PAIRED_SHA" \
  --parent "$OUT" --output "$OUT/profile.json"
```

Use a new empty output directory for fresh traces. The tracer matches fixture path
depth/length to its paired evidence and runs one native/Python job per 16/256 size.
It admits only complete paired evidence with those exact workloads and the same
Python and native executable hashes. To investigate a newer native executable,
add `--alternate-native-build-receipt /absolute/build-observation.json` and
`--alternate-native-build-sha256 SHA256`. The SHA-pinned
`forge-owned-build-observation-v1` document must identify the selected ELF in
`candidate.elf_sha256` and its full Git commit in `candidate.commit`. The report
retains that operator-supplied build observation and labels the trace as an
alternate-native diagnostic; it is neither a trace of the original native ELF
nor a new untraced timing comparison. A build observation is not execution
attestation. Python must still match the paired evidence.

The tracer samples compiler processes before admission and each case,
then rechecks executable, tracer, paired-data and optional build-observation pins
before completion, together with its own executable hash. `/proc` read failures
are explicit unless the inspected process has disappeared. Failed and censored cases remain distinct; later cases remain
unstarted. Each successful case includes a separate untimed idempotence replay.
Admission binds the retired reference blob, body hash, source entry inventory,
per-entry source bytes and script modes to the shared fixture generator. New
fixtures match original path depth and length, with newly allocated devices/inodes
verified within each run; those identities do not equal the historical run.
Skipped-file permissions follow the creation umask; script modes remain `0751`.
Raw read/write arguments prevent archiving temporary reference/body bytes.
Tracing perturbs timing: syscall durations are diagnostic, cannot be subtracted
from untraced timings, and cannot establish exact causal shares of the slowdown.

## Interpretation and bounds

Matched fixture correctness includes byte/count/mode/source-inode, copy-on-write,
skipped entries and separate zero-change replay with unchanged destination inodes.
Timed attempts retain failures/censoring and later unstarted cells. Wall includes
CLI spawn/startup and task, excludes reset/validation/replay. CPU and peak RSS use
Linux wait4. No inference or downstream answer quality is measured. Warm cache,
shared-host/load/affinity/cgroup evidence and compiler guards are explicit. The
300-second budget bounds child/admission time, not blocking filesystem operations
or whole-process duration. Normal cleanup is checked; fatal infrastructure cleanup
is not guaranteed. Original Python and native safety implementations differ.

Original measurements found native slower (1.376×/10.720× at 16/256 scripts), while
signature-only context payload used 84.4% fewer pinned GPT2 tokens. Omitted bodies
are not equivalent full-file semantics; token reduction alone proves no task-time
or answer-quality benefit. Fresh optimization evidence must retain old results.
