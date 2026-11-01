# Native candidate review foundation

The native candidate foundation exposes exact Git candidate metadata, safe
materialization of a candidate tree, candidate policy classification, check
planning, isolated execution of one external check, and candidate receipt digest
verification. These commands provide inputs for review; they are not an
end-to-end review gate. Full check orchestration,
mutation evidence, attestation, claims, reporting, and approval workflows
remain in the Python `conductor.candidate_review` package until their later
native slices land.

## Commands

```text
forge candidate plan [--repo PATH] --kind index|commit|range
    [--target-ref HEAD] [--base-ref REF] [--policy CANDIDATE_RELATIVE]
    [--profile fast|full|nightly] [--today YYYY-MM-DD]
    [--scratch-parent DIRECTORY]
forge candidate assess [--repo PATH] --kind index|commit|range
    [--target-ref HEAD] [--base-ref REF] [--policy CANDIDATE_RELATIVE]
    [--profile fast|full|nightly] [--today YYYY-MM-DD]
    [--scratch-parent DIRECTORY] --results PATH
forge candidate execute [--repo PATH] --kind index|commit|range
    [--target-ref HEAD] [--base-ref REF] [--policy CANDIDATE_RELATIVE]
    [--profile fast|full|nightly] [--today YYYY-MM-DD]
    --check ID --grants ABSOLUTE_JSON_PATH [--expected-tree FULL_TREE_OID]
    [--execute]
forge candidate execute-batch [--repo PATH] --kind index|commit|range
    [--target-ref HEAD] [--base-ref REF] [--policy CANDIDATE_RELATIVE]
    [--profile fast|full|nightly] [--today YYYY-MM-DD]
    --grants ABSOLUTE_JSON_PATH [--expected-tree FULL_TREE_OID]
    [--check ID ...] [--execute]
forge candidate inspect [--repo PATH] --kind index|commit|range
    [--target-ref HEAD] [--base-ref REF]
forge candidate materialize [--repo PATH] --tree FULL_TREE_OID
    --output NEW_DIRECTORY [--dry-run]
    [--max-blob-bytes N] [--max-tree-bytes N]
forge candidate verify-receipt FILE [--repo PATH --ref REF] [--json]
```

Planning, inspection, and materialization default `--repo` to the current
directory. Receipt binding requires `--repo` and `--ref` together.

### Plan candidate checks

`plan` resolves the candidate tree and creates a private, automatically removed
snapshot under the system temporary directory, or under the existing directory
given by `--scratch-parent`. It loads the policy from that candidate snapshot,
classifies its changes, selects the active checks in policy order, and reports
each check's selected files and whether it is scheduled. It also reports
SHA-256 digests for candidate baseline files required by the selected profile
and touched classes. The JSON result has `schema_version: 1`,
`plan_only: true`, `profile`, `today`, `candidate`, `policy` (`path`, `sha256`,
and parsed `configuration`), `checks`, and `baselines`.

Policy selection is `--policy`, then `CONDUCTOR_POLICY`, then
`CONDUCTOR_CANDIDATE_POLICY`, then the policy path configured by the candidate's
`forge.toml` or legacy `pyproject.toml`, then the central project-path default.
An explicit or environment path skips configuration-file loading. A selected
environment override that is not UTF-8 is an error. A supplied policy path must
be candidate-relative; there is no
working-tree or installed-package fallback. Policy and baseline reads stay
inside the candidate snapshot. The policy is limited to 1 MiB; paths and
strings to 4096 bytes; structural admission to 20,000 nodes and depth 32.
Planning admits at most 10 million change/check pairs and 32 MiB of selected
file paths. Classification admits 100,000 changes, 64 MiB of path metadata and
100 million estimated pattern/path character operations. Required baseline files
are regular files, at most 64 MiB each,
with a 2 GiB aggregate read limit. The private planning snapshot uses the
standard 64 MiB per-blob and 2 GiB total-tree limits.

`--profile` defaults to `fast`; `--today` defaults to the current UTC calendar
date and accepts a valid `YYYY-MM-DD`. The baseline expiry date must be today
or later. Expired check exceptions and mutation waivers are excluded from the
active policy and retained in its expired-entry fields; active exceptions and
mutation waivers may expire no more than 90 days after `today`. The planner
returns inputs and schedule flags only: it does not execute checks, create
review findings, or approve a candidate.

### Assess supplied candidate results

`assess` uses the same candidate selection, policy, profile, date, and scratch
options as `plan`; `--results` names a JSON array of supplied check results.
For example, an empty supplied set is:

```text
forge candidate assess --repo . --kind index --results results.json
```

```json
[]
```

The results file must be a regular UTF-8 file no larger than 16 MiB. Its final
path cannot be a symlink or FIFO. Duplicate JSON keys and unknown result fields
are rejected. The command plans the candidate using its private snapshot,
assesses the supplied findings against that candidate's policy, and removes
the snapshot before returning. Output has `schema_version: 1`,
`assessment_only: true`, the complete `plan` report, and `assessment` with
`decision`, `findings`, `stale_exceptions`, `scope: "supplied_results"`,
`review_complete: false`, and `execution_verified: false`.

Exit 0 means the supplied scope evaluated to `pass`; exit 1 means it evaluated
to `fail`. Malformed or unsafe inputs are command errors. An empty results
array can pass because the command evaluates only supplied results. It does
not execute checks, verify cache provenance or result completeness, or issue a
receipt or approval.

Exception matching and inherited-finding detection are recomputed separately.
The current policy's exception matching the finding's check and rule determines
the `exception_id`; a configured fingerprint match takes precedence, otherwise
the exception path pattern is matched against the finding path. Inherited status
is recomputed from finding attribution and the candidate's changed old and new
paths, including both paths for a rename. A supplied nonempty finding
fingerprint is preserved; if empty, it is computed from the check, rule, path,
line, and message.

The `error` check result status (`Error` in the native enum) always blocks; it
is a status, not finding severity. An `engine-integrity` result containing
findings and fixed native infrastructure-failure rule IDs also block without
inheritance or exception suppression. These failure rules are native semantics
and cannot be changed by candidate policy. This prevents the legacy
crash-suppression case.

Policy cannot redefine the reserved `engine-integrity` or `attestation` checks.
A result's finding IDs must match its check ID; failed results need findings,
skipped results cannot contain them, and high or critical findings cannot carry
a passed status. Diagnostic identifiers cannot contain control characters.

Admission limits include 20,000 result rows; 100,000 findings, files, command
arguments, and candidate changes each; 1 MiB per string field; 256-byte IDs;
and safe candidate-relative paths up to 4096 bytes. Parsed assessment data is
limited to depth 32, 1 million nodes, and 64 MiB including accounting overhead.
Normalized policy is limited to 4 MiB, grouped exception matching to 2 million
pairs, and pattern/path matching to 100 million estimated work units.

This remains an assessment of supplied data, not a complete review. Full check
orchestration, cache provenance, evidence, and attestation remain in the existing
workflow pending their native migrations.

### Prepare or execute one external check

`execute` selects one active external command check from candidate policy using
the same policy precedence, classification, profile, date, and Git candidate
selection as planning. It requires a separate operator-owned version-1 grants
file outside the source repository. Candidate policy cannot grant executables,
environment variables, filesystem access, or resource ceilings. The default is
a dry-run: prepare both stages, validate sandbox admission and argument
expansion, remove their private workspaces, and return their plans without
launching a child or creating a cgroup. `--execute` explicitly launches them.

For example, adapt these absolute paths and limits to the host. The source and
its common Git directory must be readable; executable and runtime-library
directories need explicit read grants. An executable alias such as `analyzer`
must match the first argument of the policy's command and version command.
Existing scratch and delegated cgroup parents are operator-provided; Forge
does not enable controllers or modify the parent's limits.

```json
{
  "version": 1,
  "source_root": "/home/operator/project",
  "read_only": [
    "/home/operator/project", "/opt/analyzers/bin", "/usr", "/lib", "/lib64"
  ],
  "programs": {"analyzer": "/opt/analyzers/bin/analyzer"},
  "environment": {},
  "scratch_parent": "/home/operator/private-candidate-scratch",
  "cgroup_parent": "/sys/fs/cgroup/operator-delegated",
  "resources": {"cpu_millis": 1000, "memory_mib": 256, "processes": 16},
  "max_wall_seconds": 30,
  "max_cpu_seconds": 30,
  "max_address_space_mib": 512,
  "python": null
}
```

Keep the operator grants file mode `0600`. It must be a regular, single-link
file owned by the current user, without group/world write permission, at most
1 MiB; its final path cannot be a symlink. Unknown fields and duplicate
JSON keys are refused. All granted paths are absolute and must exist. For a
repository with its Git common directory elsewhere, add that directory to
`read_only`. Runtime paths above are examples, not implicit grants. The
`resources` fields are cgroup millicpus, MiB of memory, and process count;
these are separate from per-process CPU-time and address-space limits.
`max_*` fields cap policy wall seconds, CPU seconds, and address-space MiB.
Policy limits exceeding these ceilings fail preparation. Reserved workspace
environment names, including `PATH`, `HOME`, `TMPDIR`, `GIT_*`, `XDG_*`, and
`PYTHON*`, cannot be supplied through `environment`. `python`, when non-null,
names an explicitly granted program alias for `{python}`; the native core
never discovers or depends on an interpreter.

```text
PATH= /absolute/path/to/forge candidate execute --repo /home/operator/project --kind index \
  --check lint --grants /home/operator/candidate-grants.json
PATH= /absolute/path/to/forge candidate execute --repo /home/operator/project --kind index \
  --check lint --grants /home/operator/candidate-grants.json \
  --expected-tree FULL_TREE_OID --execute
```

Replace `/absolute/path/to/forge` with the native executable. Child `PATH` is
always empty. Argument expansion uses vectors, never a shell. A whole
`{files}` argument expands to separate selected paths; embedded `{files}` stays
literal. Version probes receive no changed-file arguments. Other replacements
are `{repo}`, `{snapshot}`, `{base}`, `{tree}`, `{python}`, and
`{changed_files_file}`; unknown placeholders remain literal.

Each version or analyzer stage gets a separate candidate snapshot, private
HOME/TMP/XDG cache, index, and detached Git metadata built with libgit2. This is
not a linked worktree or clone; source refs and source index are not rewritten.
The original source object directory is explicitly readable via private Git
alternates. `{repo}` intentionally still names the original source repository:
operator read permission permits an analyzer to inspect it, so sandboxing and
successful execution do not prove that the analyzer covered only the snapshot
or examined every selected file. Version success is required before the main
command; a failed or potentially truncated version capture blocks that launch.
Version probes have a maximum 30-second wall timeout, the same cgroup and
address-space limits, and a CPU-time limit capped at 30 seconds. Both stages
remain within the operator's ceilings; neither can run with unlimited rlimits.

The supervisor uses the shipped filesystem/process/network sandbox and an
explicit delegated cgroup. It owns child observation, bounded Unicode output
tails, cancellation, resource cleanup, and final reaping. Resource evidence,
cleanup failures, timeout, and infrastructure failures cannot map to a pass.
SIGINT/SIGTERM request cancellation through the existing interrupt handler; the
CLI waits for the resource-owning actor before exiting its runtime.
Cleanup guarantees apply while the runtime can execute its cleanup path; they
do not cover supervisor SIGKILL, machine failure, or uninterruptible kernel I/O.
When cleanup cannot safely complete, the private workspace is retained for
recovery instead of deleting potentially live process resources.

Output has `schema_version: 1`, `executed`, `skipped`, candidate and policy
identity, `grants_sha256`, normalized `check`, selected `files`, ordered
`stages`, and `result`. Each stage contains its expanded `command`, sandbox and
resource admission, limits, and observed `report` (null during dry-run).
Dry-run and no-matching-file skips leave `result` null; skips have no stages.
A failed version produces only its version stage. Actual execution returns a
`CheckResult`, not an assessment or receipt. Exit 0 means dry-run, skip, or
passed execution; exit 1 means a failed execution result. Invalid input or
preparation failures are command errors. Neither result means approval, full
review completion, or permission to land.

Native integration validation commands (not a claim of results):

```text
cargo +1.98.0 test --manifest-path native/forge/Cargo.toml --test native_candidate_execute_cli
FORGE_TEST_CGROUP_PARENT=/sys/fs/cgroup/operator-delegated \
  cargo +1.98.0 test --manifest-path native/forge/Cargo.toml --test native_candidate_execute_cli \
  -- --ignored --test-threads=1
```

### Execute a selected batch

`execute-batch` uses the same candidate selection and operator grants as
`execute`. It defaults to preparation without launching children; `--execute`
requests fresh execution. Repeated `--check ID` selects exact active IDs;
omitting it selects all active checks for the profile. Duplicate, absent, and
inactive requested IDs are errors. Report order always follows policy
declaration order, regardless of request order or completion order.

Batch execution additionally requires the grants file's `batch` object. For
example, replace `resources` and add `batch` in the complete grants above with
this fragment; it does not constitute a complete grants file:

```json
{
  "resources": {"cpu_millis": 1000, "memory_mib": 128, "processes": 16},
  "batch": {"max_workers": 2, "cpu_millis": 2000, "memory_mib": 256, "processes": 32}
}
```

`resources` is the per-stage cgroup ceiling. `batch` is a separate aggregate
operator admission budget; its CPU, memory, and process units are identical.
Admitted concurrency is the minimum of policy `max_workers`, operator
`batch.max_workers`, and each aggregate budget divided by its per-stage
ceiling, rounding down. Worker counts must be 1–32. A missing budget or a
budget unable to admit even one stage fails before launching. These aggregate
fields admit concurrent stages; they do not enable controllers or modify the
borrowed cgroup parent. Individual stages still have their own cgroups,
CPU-time/address-space limits, wall deadlines, and Unicode capture limits.

```text
PATH= /absolute/path/to/forge candidate execute-batch --repo /home/operator/project \
  --kind index --grants /home/operator/candidate-grants.json
PATH= /absolute/path/to/forge candidate execute-batch --repo /home/operator/project \
  --kind index --grants /home/operator/candidate-grants.json \
  --check lint --check format --execute
```

A check without selected files and with `always=false` is an explicit skipped
row, including a builtin check. It has no stages or result and does not make
the batch incomplete. A scheduled builtin has no native implementation in
this command: it appears in `unexecuted` and as an error row, making
`success=false` and exit 1 even in dry-run. Explicitly selecting supported
external checks avoids implying that unselected builtins ran.

Each command check requires usable version evidence before its analyzer
stage, in separate private workspaces as described above. A failed version
blocks that check's analyzer, while other checks still return their own
results. Ordinary analyzer failures produce failed `CheckResult` rows; queued
cancellation and per-job infrastructure errors produce an error row with a
null result. Owner-level failures remain command errors. SIGINT/SIGTERM cancel
queued work and active stages; the independent batch owner awaits registered
resource owners before the CLI runtime exits. Dropping a client does not drop
cleanup ownership. The same SIGKILL/runtime-destruction/uninterruptible-I/O
limits and unsafe-workspace retention rules apply as for single execution.

The report has `schema_version: 1`,
`execution_scope: "selected_regular_commands"`, `review_complete: false`,
`cache: "disabled_fresh_execution"`, `executed`, `success`, `cancelled`, common
candidate/policy/grants identities, and `budget` with `admitted_parallelism`.
`checks` contains policy-ordered rows with `check_id`, normalized `check`,
`files`, `skipped`, `stages`, `result`, and `error`; `unexecuted` names scheduled
unsupported checks. Dry-run retains null results and stage reports. Exit 0
means the selected scope prepared or executed successfully; exit 1 means
failed or incomplete selected scope. Cancellation cannot yield success.

Admission caps selection at 256 checks, 10 million change/check pairs, 8 MiB of
paths per check, and 32 MiB of aggregate selected paths. Serialized report
retention is capped at 128 MiB, with reserved bounded error rows. Every
execution is fresh: this command neither reads nor writes a result cache. A
successful batch is not a complete review, an assessment, attestation, receipt,
approval, or evidence that an analyzer examined every selected file. The
legacy full-review callers remain in place.

The batch integration target is `native_candidate_execute_batch_cli`;
delegated-cgroup cases are explicitly ignored by default and require
`FORGE_TEST_CGROUP_PARENT` when run with `--ignored --test-threads=1`, using the
manifest-path command pattern above. Those fixtures create their own child
delegation and enable controllers only there, never on the borrowed parent.
This describes validation commands and boundaries, not test or performance
results.

### Inspect

`inspect` reads Git object and index data through the native Git library and
emits a JSON `Candidate`: candidate kind, candidate and base tree IDs, optional
base/target commit IDs and target ref, changed paths and modes, old/new blob
IDs, rename/copy source path where relevant, and integration-base identity and
explanation. Change `classes` and `risk` fields are placeholders at this layer
(`[]` and `normal`); no candidate policy has classified the changes.

- `index` captures the staged index tree, excluding unstaged and untracked
  working-tree content. It compares against `HEAD` by default, or the explicit
  `--base-ref`; a repository without a commit uses the empty tree.
- `commit` inspects `--target-ref` (default `HEAD`) against its first parent.
  Root commits compare with the empty tree. Merge commits require an explicit
  `--base-ref`.
- `range` requires `--base-ref` and compares the merge base of that ref and the
  target commit with the target tree.

Ref ambiguity is refused; use an exact `refs/...` name when multiple ref
namespaces resolve the same short name. Candidate change metadata and tree
traversal are bounded to 100,000 entries and 64 MiB of metadata. Index input is
bounded to 64 MiB, paths to 4096 bytes, and similarity inputs to 64 MiB per blob
and 2 GiB total. Non-UTF-8 or unsafe Git paths are refused.
Index admission reads bounded bytes once and seals a private Linux memory file
before libgit2 parses them. Replacement of the original index cannot substitute
unchecked bytes. `GIT_INDEX_FILE` is honored; a relative path is resolved from
the selected repository root. Neither index file is rewritten by inspection.

### Materialize

`materialize` writes the exact blobs named by a full tree object ID to a new
private directory. It creates a standalone snapshot, not a Git worktree; it
does not create `.git` metadata. Safe relative symlinks are preserved only when
their resolved targets remain inside the snapshot. Absolute, escaping, or
otherwise unsafe symlinks and reserved `.git` paths are refused. Validation
limits each path to 256 components, each symlink chain to 40 links (including
the initial link), aggregate expansion to 1 million components and 64 MiB of
resolved path bytes, and a single expanded path to 4096 bytes. Gitlinks are
skipped and reported; foreign submodule trees are not fetched. Non-UTF-8 paths
are refused, existing destination paths are never overwritten, and blobs are
read one at a time. The standalone snapshot root is created with mode `0700`;
files start private and receive their Git executable mode, while created
subdirectories use mode `0755`.

The default and maximum blob size is 64 MiB; the default and maximum total tree
size is 2 GiB. Operators can lower either limit. `--dry-run` checks destination
admission and reports planned files, links, skipped gitlinks, and byte budgets
without creating output. A library-created temporary snapshot is cleaned up
automatically; an explicitly published CLI snapshot persists at `--output`.
The existing output parent is selected by the operator and must be trusted.
Publication rechecks its identity and uses atomic no-replace renaming on Linux.

### Verify a candidate receipt

`verify-receipt` accepts at most 16 MiB of UTF-8 JSON and checks that a candidate
receipt is self-consistent with its canonical SHA-256 digest. Canonicalization
sorts object keys and normalizes finite JSON numbers; the digest is recomputed
with `receipt_id` and `receipt_digest` set to empty strings. Supplying `--repo`
and `--ref` together additionally binds `candidate.tree_oid` to the selected
commit's tree. JSON output includes `digest_valid`, `receipt_id`,
`receipt_digest`, `decision`, and `tree_binding` (either `null` or an object
containing `commit_oid` and `tree_oid`). A matching digest is an integrity
check over the receipt content; it does not establish who produced or approved
that content. The `decision` value is reported as data and is not an approval.
Duplicate keys, nonfinite numbers and excessive nesting are refused. Integers
are limited to 4,300 digits, float tokens to 16 KiB, and the shared JSON nesting
limit is 72 levels. This finite JSON profile intentionally excludes legacy
Python's nonstandard `NaN` and infinity encodings.

This command is separate from mutation-receipt authentication and from
verification of existing local-check receipts. Those formats and trust paths
have distinct owners and contracts; candidate receipt digest verification
does not replace them.

## Current boundary

The native commands can apply candidate policy, plan checks, and explicitly run
one granted external check or a bounded selected batch. They do not orchestrate
the complete review pipeline,
admit or authenticate mutation
evidence, issue attestations, acquire claims, produce a complete review, or
decide whether work may land. Continue to use the existing candidate-review
workflow for those decisions. The interdependent Python candidate-review
package remains in place: its CLI and engine still call the Python command
runner for the legacy complete-review workflow. The native execution paths
do not silently replace those callers. Full orchestration, mutation evidence,
attestation, claims, reporting, and approval remain separate migration work.

This page records the native command contract only. It makes no build, test,
parity, approval, or release-readiness claim.
