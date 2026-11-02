# Native mutation corpus audit

`forge candidate mutation-corpus` checks mutation-corpus reproducibility against the recorded baseline. With no `--kind`, it audits the live repository. Candidate mode is explicit and resolves immutable Git input; it does not change live-audit semantics.

The command requires independently pinned executable inputs: `--git` with `--git-sha256`, `--git-build-profile` with `--git-build-profile-sha256`, and `--mutation-runner-bundle` with `--mutation-runner-sha256`. The build profile is an operator-owned assertion for that exact Git binary, including its declared system-attributes mode/path; Forge does not independently discover or prove the Git build's compiled-in attributes path. Missing, unsafe, mismatched or malformed pins and unsupported Git environment overrides refuse. Effective Git config, index and applicable attribute sources are held/rechecked; input drift, unsafe paths/types, unsupported patch targets and resource-limit violations fail closed. Some stricter refusals (including unsafe symlink parents and binary/non-UTF-8 patch targets) are compatibility limits.

Example live audit (values are operator-provisioned):

```sh
forge candidate mutation-corpus --repo . \
  --git "$CONDUCTOR_MUTATION_GIT" --git-sha256 "$CONDUCTOR_MUTATION_GIT_SHA256" \
  --git-build-profile "$CONDUCTOR_MUTATION_GIT_BUILD_PROFILE" \
  --git-build-profile-sha256 "$CONDUCTOR_MUTATION_GIT_BUILD_PROFILE_SHA256" \
  --mutation-runner-bundle "$CONDUCTOR_MUTATION_RUNNER_BUNDLE" \
  --mutation-runner-sha256 "$CONDUCTOR_MUTATION_RUNNER_SHA256" --summary
```

The live audit reads and revalidates registry fragments, manifests, referenced files, receipt inventories, baseline, changed-file inputs, runner bundle and Git identity. It does not run mutation tests or manufacture campaign receipts. `RATCHET_HELD` is only the legacy corpus reproducibility status; it is not a campaign PASS, mutation-kill result or approval. Baseline recording is explicit with `--write-baseline` (often paired with `--summary`) and takes the exclusive review lease; ordinary audit does not write Git objects, refs or index.

Candidate mode uses `--kind`, `--target-ref` (default `HEAD`), optional `--base-ref`, and required external `--scratch-parent`; candidate policy and scratch are validated against the captured tree. Index resolution may create Git tree objects through the candidate resolver. This is an audit of corpus state, not a review run or gate PASS. Repository state/configuration drift, timeout, signal, malformed data and failed rechecks refuse rather than yield a clean verdict. The bounded worker option is `--max-workers` (1..64); execution is currently serialized.

The default baseline remains beside the project's configured mutation registry.
An explicit `--registry` changes the audited inventory; use `--baseline` to select
a different baseline. Baseline output parents must already exist and satisfy the
ownership checks.

Both CLI modes own interrupt handling through Git probes and final disposition.
Cancellation stops and reaps owned probe groups, interrupts writer-lease waits,
and checks again before baseline replacement. Candidate snapshots are discarded
before reporting, including on failure; cancellation is checked after cleanup.
Baseline replacement is atomic, but a later cleanup or cancellation failure can
occur after a baseline has already been published. A nonzero result therefore
does not imply that an explicitly requested baseline write never happened.

Integration cases exercise real Git attribute/configuration drift, all seven
baseline dimensions, and a native blocked-Git fixture with a descendant. Separate
CLI cases verify cancellation during that probe and while a real writer lease is
held. These are correctness checks, not automatic-engine mutation PASS receipts.
