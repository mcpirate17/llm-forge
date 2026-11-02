# Native candidate review foundation

The native candidate CLI provides exact candidate metadata, bounded candidate
snapshots, policy classification and planning, isolated check execution, and
receipt digest verification. The `candidate review` coordinator also resolves
the complete active profile, captures native-owned evidence, runs scheduled
checks when explicitly requested, assesses only those observations, and can
publish a digest-sealed report. Planning and review preparation do not execute
checks, and a report is not an approval or authorization to land.

The coordinator currently requires one complete profile. It records
`review_complete`, `execution_verified`, `decision`, `incomplete`, `checks`,
`findings`, `stages`, and provenance bindings. Missing obligations or
incomplete observations fail closed. `approval` remains false. The checksum
proves report self-consistency only; it is not authenticated approval or proof
of an external analyzer's completeness. This page documents the current native
slice and does not claim overall migration completion, production readiness,
or final validation.

## Commands

Existing foundation commands remain available:

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
    [--surface pre-commit|post-commit|ci|manual]
    --grants ABSOLUTE_JSON_PATH [--expected-tree FULL_TREE_OID]
    [--check ID ...] [--execute]
forge candidate inspect [--repo PATH] --kind index|commit|range
    [--target-ref HEAD] [--base-ref REF]
forge candidate materialize [--repo PATH] --tree FULL_TREE_OID
    --output NEW_DIRECTORY [--dry-run]
    [--max-blob-bytes N] [--max-tree-bytes N]
forge candidate verify-receipt FILE [--repo PATH --ref REF] [--json]
```

The complete-profile coordinator is invoked separately:

```text
forge candidate review [--repo PATH] --kind index|commit|range
    [--target-ref HEAD] [--base-ref REF] [--policy CANDIDATE_RELATIVE]
    [--profile fast|full|nightly] [--today YYYY-MM-DD]
    --grants ABSOLUTE_JSON_PATH [--expected-tree FULL_TREE_OID]
    --surface pre-commit|post-commit|ci|manual [--execute] [--lock-timeout SECONDS]
    [--owner ID] [--receipt PATH] [--sarif-out PATH] [--junit-out PATH]
    [--registry PATH --registry-sha256 SHA256]
    [--mutation-runner-bundle PATH --mutation-runner-sha256 SHA256]
    [--equivalence-guest PATH --equivalence-guest-sha256 SHA256]
    [--equivalence-store PATH --equivalence-store-sha256 SHA256]
    [--equivalence-artifacts DIRECTORY]
forge candidate runner-bundle --repo PATH --package-root ABSOLUTE_PATH
    --output NEW_PATH [--lineage PATH]
    [--external-invariant-probe ELF_PATH] [--exclude-root DIRECTORY ...]
```

`review` requires an explicit `--surface`, operator grants with a batch
budget, and a selected profile. Without `--execute`, it resolves the candidate
and reports the required checks and selected files without running them; the
report says `executed: false`, `review_complete: false`,
`execution_verified: false`, and `approval: false`. `--execute` opts into fresh
execution. `--receipt` is available only with `--execute` and atomically
publishes the completed report under an existing operator-controlled
parent. `--owner` supplies the local lane identity for ownership observations;
it grants no authority. An optional expected tree makes stale candidate
selection an input error. Profile/date/policy defaults and the shared operator
grants format are described in the existing sections below.

The complete-profile report contains coordinator-owned results only. It
includes reserved `engine-integrity` and `attestation` obligations alongside
every active policy check; unsupported or missing observations remain
incomplete and block a pass. The coordinator captures test selection, source
evidence, ownership, mutation evidence, equivalence inputs, regular checks,
and targeted checks where the selected policy schedules them. It rechecks
held inputs and observations before reporting and removes its private snapshot
before a successful report can be published. Cache hits are not accepted as
fresh review evidence. A `pass` means the selected local observations met
the configured policy for the named candidate/profile/surface; it does not
assert that an external tool examined every relevant input or authorize merge.

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

### Native file selection and cppcheck inputs

Checks may set `path_suffixes` to at most 64 distinct literal suffixes, each a
dot followed by 1–31 ASCII letters, digits, underscores, pluses or hyphens.
Normalization lowercases them and rejects duplicates without regard to case.
An absent or empty list leaves selection unrestricted. Suffix matching uses
the current basename after class, exclusion and deletion selection, allocates
nothing per path, and keeps the existing sorted, deduplicated result. A lone
dotfile such as `.c` has no suffix; `.hidden.c` does. Rename-side class unions
do not make a current `.rs` path match `.c`.

The shipped cppcheck check now uses this selector and invokes the C++ binary
directly. Its Python wrapper has been deleted. The packaged policy is a symlink
to the same root policy. The remaining legacy review bridge forwards dataclass
metadata and the snapshot path into the shared Rust implementation; it contains
no duplicate suffix or file-input algorithm.

`file_input = "cppcheck-list"` requires a command check with exactly one
`--file-list={changed_files_file}` argument and no `{files}` splice. The default
`"paths"` preserves other checks' existing behavior. A list avoids interpreting
leading-dash filenames as options. Cppcheck 2.13's
[input parser](https://github.com/cppcheck-opensource/cppcheck/blob/2.13.0/cli/cmdlineparser.cpp)
reads its entries as paths, but its
[path handling](https://github.com/cppcheck-opensource/cppcheck/blob/2.13.0/lib/path.cpp)
can reinterpret backslashes, so this protocol explicitly rejects them along
with NUL, CR/LF and noncanonical relative paths. Selected Git entries must be
regular files. Before either stage, native execution checks every entry through
the private snapshot directory without following parent or leaf symlinks;
missing files and special files fail. The legacy bridge uses the same native
preflight. Admission bounds are 100,000 files, 4,096 bytes per path and 8 MiB
of newline-delimited paths, checked before filesystem inspection.

Cppcheck keeps its existing flags, severity and resource limits. A native-only
change with no eligible C-family suffix now skips both version and analyzer
stages instead of launching a wrapper that does nothing. No executable grant is
needed for that skip. Selected checks still need an explicit cppcheck grant,
usable version evidence and sandbox resources. Input preflight proves those
files exist in the snapshot; it does not attest to an external analyzer's
behavior or completeness of analysis.

### Native Cargo audit

`execute-batch --check cargo-audit` also supports the native offline RustSec
builtin. It requires separately pinned operator evidence; see the
[Cargo audit contract](native-cargo-audit.md) for its bundle, grants, scope,
bounds and remaining legacy bridge.

### Native Cargo lint

`forge candidate cargo-lint --mode fmt|clippy -- FILE...` prints a bounded
plan without running a compiler; `--execute` runs sequentially and
`--tool-version` probes the explicit toolchain. Workspace and borrowed-target
scope are included in the plan. Direct execution is not sandboxed; for governed
use, call the CLI through an explicitly granted external stage. The native
runtime uses a canonical Cargo binary and private directories. See the
[native Cargo lint contract](native-cargo-lint.md) for roster resolution,
scope limits, exclusions, prerequisite status, and tool grants. Candidate policy
keeps the existing IDs and routes their commands through the uncached native
Forge stage.

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

This command remains an assessment of supplied data, not a complete review.
The separate `candidate review` command below assembles coordinator-owned
observations for the complete active profile.

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

```sh
make build
FORGE_TEST_BIN="$PWD/native/forge/target/release/forge" \
  cargo +1.98.0 test --manifest-path native/forge/Cargo.toml --test native_candidate_execute_cli
FORGE_TEST_BIN="$PWD/native/forge/target/release/forge" \
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
the batch incomplete. Native batch builtins currently cover `candidate-integrity`,
`dependency-integrity`, `secret-scan`, `native-source`, and `cargo-audit`; other scheduled
unsupported checks remain in `unexecuted` and as error rows, making
`success=false` and exit 1 even in dry-run.

The supported builtins run inside Forge without a shell, external process, or
per-check cgroup. They still require the candidate source grants and operator
`batch` budget; the grants file may contain an empty `programs` map. A scheduled
worker consumes a batch concurrency slot. Per-stage CPU and memory grants do
not impose kernel limits on these in-process evaluators; their input and work
ceilings bound admitted work, rather than host-process RSS or elapsed time. A scheduled
`candidate-integrity` check requires explicit `--surface`, including in dry-run.
Dry-run prepares the private capability-rooted candidate snapshot and validates
bounded inputs, but does not evaluate the builtin or return a result. Executed
checks use that actual snapshot; dependency integrity verifies the changed
manifest's corresponding lockfile there, so a deleted or absent changed lockfile
produces a blocking finding.

Supported rows include `builtin.implementation: "forge-builtin/1"`; the
`candidate-integrity` row also reports `unicode_version: "15.0.0"`. That Unicode
version matches Python 3.12 for valid UTF-8 case folding and representation; it
does not normalize text, accept lone surrogates, or claim Unicode versions newer
than 15.0.0. The compatibility boundary is UTF-8 candidate paths.

`secret-scan` considers all live changes independently of the scheduling row's
file filter. It preserves deliberate skip behavior for materialized symlinks,
nonregular files, files above the policy size limit, invalid UTF-8, and explicit
Git gitlinks. A skipped file is not evidence that its contents were scanned.
Valid UTF-8 containing NUL remains scan text. Reads use strict UTF-8 decoding and
normalize CRLF and lone CR to LF. An unexpectedly missing regular candidate file
is an explicit read error, not a clean skip.

`native-source` includes every live native-class candidate path in its result
scope, even if the policy-selected row scope is narrower. It follows only
capability-confined internal symlinks and records missing or unreadable expected
files, nonregular endpoints, and invalid UTF-8 as critical incomplete-ledger
findings, including unmaterialized changed gitlinks. Core regex findings keep
their file/rule/match order; sanitized ledger findings append afterward in sorted
path order. Its `files_expected`, `files_read`, `files_skipped`, and `skipped_files`
metrics are always present, including zero counts for an empty native source
scope. The scan core preserves regex messages, help, redaction, and finding
semantics, but incomplete-read diagnostics intentionally
do not reproduce Python exception prose.

Each scan file read is capped at 1 MiB, and an attempt consumes aggregate read
credit up to 32 MiB, including the extra byte used to detect growth; failed UTF-8
or I/O attempts still consume credit. The reader polls cancellation between
64 KiB chunks, verifies descriptor and current confined endpoint identity, and
uses nonblocking regular-file reads. Secret reads reject final symlinks; native
source may follow only safe links inside the snapshot. Limits are 100,000
candidate changes/files and 4,096-byte paths. The prepared payload is capped at
32 MiB. The scan core admits at most 100,000 findings and 32 MiB of exact
serialized findings; after the first match in a file, it indexes LF offsets
once and uses binary search for finding line numbers. Resource refusals return
an error row with no partial result. These are bounded-admission and
cooperative-cancellation controls, not
hard host RSS, wall-time, or uninterruptible-I/O limits.

Builtin preparation and results are each capped at 32 MiB. Candidate integrity
admits at most 100,000 tree entries and changes, 4,096-byte paths, 4,096
protected globs of at most 4,096 characters each, and 100,000,000 units of
estimated glob-match work. Deserialization borrows input metadata and globs are
prepared once. `checks[].files` remains the policy-selected file list;
`checks[].result.files` reports each handler's full scope and can differ.
Builtin workers are joined through cancellation and snapshot cleanup; a running
bounded evaluator may finish its current CPU work before cancellation returns.

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
`execution_scope: "selected_regular_checks"`, `review_complete: false`,
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

### Export a recorded receipt

```sh
forge candidate report receipt.json
forge candidate report receipt.json --format json --output report.json
forge candidate report receipt.json --format sarif --output report.sarif
forge candidate report receipt.json --format junit --output report.xml
```

`report` verifies the digest and admits the schema-1 rendering fields before
producing output. It never runs checks, recomputes an assessment, or approves a
candidate. Exit zero means the export succeeded, even for a recorded failing
decision. Human, SARIF and JUnit reports explicitly mark execution unverified;
JSON preserves the recorded payload and digest. The display identifier remains
unbound by the legacy digest. JSON uses indented UTF-8, with a final newline;
export whitespace and key order are not the canonical bytes used by the digest.

The human report uses the recorded `policy.block_at` threshold, separates
waived, excepted and inherited findings, shows every waiver, then prioritizes
blocking findings within the next twenty displayed entries. All dynamic terminal
controls and directional overrides are escaped. Empty policy data is accepted
only for failure receipts and uses the high threshold. Surface/profile labels,
tree identifiers, status/severity enums and report field types are checked.

SARIF retains all findings and recorded waiver/exception/inheritance annotations
as properties, without authorizing standard suppressions or baseline states. It
percent-encodes artifact paths and preserves first-occurrence rule order. Legacy receipts do not specify
column units, so the raw zero-based column is retained in `properties.recordedColumn`
instead of inventing a SARIF character offset. JUnit preserves each check's
recorded status, escapes XML text and attributes, and refuses XML-invalid
characters. A synthetic failing testcase prevents a recorded failure without
failed/error checks, or blocking findings not represented by those checks, from
becoming a green suite. Recorded policy and annotations remain unverified; the
export cannot serve as independent gate evidence.

Receipt input and each rendered output are capped at 16 MiB. Rendering also
limits nesting to 32 levels, nodes to one million, checks to 20,000 and combined
top-level/check findings to 100,000. The full output is prepared before writing.
An output file's parent must already exist. Publication walks and pins each
parent without following symlinks, rejects traversal and unsafe existing leaves,
uses a random exclusive mode-0600 staging inode, and synchronizes the file and
directory around atomic replacement. Symlinks, special files, hardlinks and
foreign-owned destinations are refused. Cooperating concurrent writers leave
one complete report. Same-UID hostile directory moves or entry swaps are outside
the trusted-parent contract; rename does not follow or truncate a target inode.
A directory-sync error after replacement explicitly says publication occurred.

The Rust `candidate_receipt::seal` API produces the same canonical checksum and
derived `gr-` identifier for bounded JSON without an interpreter. Sealed values
are immutable through the public API. Sealing is an integrity operation and
adds no execution, authorship, authentication or approval evidence.

## Current boundary

The native CLI now includes a complete-active-profile review coordinator as
documented above, alongside the foundation and single/batch execution paths.
The coordinator uses native-owned evidence and supervised stages, but this
does not establish authenticated execution of every installed compatibility
source or completeness of external analyzer behavior. The legacy Python
candidate-review workflow remains in place; native review does not silently
replace its callers. Review reports record their scope and limitations, set
`approval: false`, and do not decide whether work may land.

This page records the current native command contract only. It makes no build,
test, parity, approval, migration-completion, or release-readiness claim.

### Complete-profile review, runner bundles, and mutation invariants

The review coordinator composes candidate selection, policy scheduling,
operator grants, native evidence collectors, supervised stages, assessment,
and report sealing. Its review path differs from `assess`, which evaluates
caller-supplied rows, and `execute-batch`, which executes checks without
assembling complete review evidence. Review always uses the complete active
profile and rejects duplicate or reserved check identities. Its required set
includes `engine-integrity` and `attestation`, even if the profile has no
corresponding policy rows. Missing results, cancellation, check errors,
baseline drift, cleanup failures, or failed rechecks prevent a complete
passing decision. A dry-run is a plan only and cannot produce a review receipt.

The receipt's `execution_verified` field means the coordinator completed its
own required observation/execution protocol for this report. It is not a
cryptographic attestation of arbitrary child behavior. The sealed digest
supports corruption detection and optional exact-tree binding via
`verify-receipt`; it does not authenticate who approved the candidate. The
report always records `approval: false`. Review has no approval or promotion
operation.

#### Installed mutation runner bundle

The optional `--mutation-runner-bundle` plus `--mutation-runner-sha256`
provides a separately operator-selected compatibility-source manifest for
mutation receipt and value verification. Generate a manifest with
`candidate runner-bundle`; the package root must be a canonical absolute
installed package directory outside the source checkout, Git common directory,
snapshot, and scratch roots. The output must be new and inside an existing
owned mode-0700 parent. The manifest pins the exact shipped inventory:
eleven compatibility components, five core/scope/engine bindings, and
optional lineage and external-invariant probe files. Each file is bound by
SHA-256; inputs are held and rechecked while the bundle is captured. The
schema uses the CPython 3.12 symbol-hash protocol. Bundle inputs are bounded
to 32 MiB total (with per-source and probe limits).

For example, after choosing the installed package and protected output parent:

```text
forge candidate runner-bundle --repo /path/to/source \
  --package-root /canonical/installed/package \
  --output /operator/private/bundle/runner.json \
  --lineage /canonical/installed/package/lineage.json \
  --external-invariant-probe /canonical/installed/package/probe/conductor_native.so
```

The command prints the manifest SHA-256 and path. Pass both path and digest
to `candidate review`; the digest must match the manifest bytes. The pins
prove which bytes were captured and detect drift. They do **not** prove those
installed bytes were executed, loaded, or used to create a historical
receipt. Review records this limitation explicitly; consistency and source
binding are not execution authentication.

#### Mutation invariant observations

When mutation-evidence verification derives external invariant requests, a
review needs the bundle's pinned native probe. The coordinator executes those
requests through its granted and supervised stage, then binds the observation
to the candidate tree, policy, request set, and held probe. The probe must be
a pinned native ELF shared library named `conductor_native.so`. The external
measurement observes requested runtime and coverage state; it is not proof
of historical loading or a general guarantee that every invariant holds.

The standalone `candidate mutation-invariant` subcommand is the worker
protocol and requires an explicit `--execute`; normal use is through review's
owned stage, which supplies its arguments internally. It binds execution to
the candidate base/tree and original source, requires explicit grants for
Forge, Python, pytest, coverage, and read-only `/proc`, and verifies the probe
pin. Requests are bounded and each identifies a candidate test node, pinned
Torch value, phase, and justification. Keep this command under the parent
review supervisor; invoking it directly does not create complete candidate
review evidence or issue approval.

The probe directory precedes the captured project on `PYTHONPATH`; use a
dedicated directory containing the probe so other installed modules cannot
shadow project imports. The worker preserves the operator's pytest autoload
setting. At each sample, the probe checks the loaded Torch module's version,
canonical origin, and identity against the separately observed installation.
Torch must originate outside the source checkout and captured snapshot.

### Native Make review callers and report exports

`--receipt FILE`, `--sarif-out FILE` and `--junit-out FILE` export the newly sealed
in-memory receipt, including failed reviews. They require `--execute`, existing
real parent directories, and distinct destinations. All requested formats render
before publication. Each file is replaced atomically through the existing report
store; multiple outputs are not a transaction. An export error fails the command
and never falls back to an existing receipt. A failure before a new receipt is
built does not overwrite an older artifact or label that artifact as fresh.

The Make review front doors call this native coordinator directly:

```sh
make candidate-review CONDUCTOR_REVIEW_GRANTS=/operator/review-grants.json
make governance-audit CONDUCTOR_REVIEW_GRANTS=/operator/review-grants.json
make governance-preflight CONDUCTOR_REVIEW_GRANTS=/operator/review-grants.json
```

`FORGE` selects the executable. `REVIEW_ARGS` supplies additional native options
for `candidate-review`; `CONDUCTOR_REVIEW_ARGS` supplies them for the three
`governance-*` review targets. These options carry explicit owner, runtime bundle,
registry and equivalence pins when required. Make never generates grants or
silently runs the Python review if native admission fails. These manual/CI
commands do not replace a host's pre-commit hook or locked commit protocol.

## Remaining compatibility boundary

The mutation collector currently preserves the legacy grandfather inventory:
commit `61343f575215dd222a74fc2c060d0328692ded5e`, its fixed tree, and a
SHA-bound inventory from the LLM repository. Changed-test review on a host
without those objects fails this check. Portable baseline configuration remains
unfinished. External-invariant waivers also require an actual campaign receipt
bound to a manifest declaring the invariant; a successful probe alone does not
satisfy the collector. No complete positive collector run is claimed from the
leaf probe tests.

The Python candidate-review package remains shipped for live compatibility APIs.
`conductor.gate` and the legacy review benchmark still invoke its review CLI.
`governance-commit` and `attest-message` still use its shared governance mutex,
inherited lock token/descriptor, canonical pre-commit receipt lookup and commit
message mutation. Those callers must switch with equivalent native lock and
receipt-binding behavior before their engine/CLI code can be removed. Existing
PyO compatibility drivers also call that code; retire them only with an explicit
native coverage map, not by making imports fail.

Executing native review holds the Git-common governance lock through selection,
cleanup, and publication. `--lock-timeout` defaults to 30 seconds and accepts
0–3600. Normal reviews hold a shared lease; a review invoked by a locked commit
can retain an inherited exclusive descriptor. The parent must hold that lease
until review returns. Token-only inheritance is refused because the token
cannot retain the underlying lock. Dry runs acquire no governance lease.
