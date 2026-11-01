# Native candidate review foundation

The native candidate foundation exposes exact Git candidate metadata, safe
materialization of a candidate tree, candidate policy classification, check
planning, and candidate receipt digest verification. These commands provide
inputs for review; they are not an end-to-end review gate. Check execution,
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

This remains an assessment of supplied data, not a complete review. Check
execution, cache provenance, evidence, attestation, and full review
orchestration remain in the existing workflow pending their native migrations.

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

The native commands can apply candidate policy to classify changes and plan
checks, but they do not run analyzers or tests, admit or authenticate mutation
evidence, issue attestations, acquire claims, produce a complete review, or
decide whether work may land. Continue to use the existing candidate-review
workflow for those decisions. The interdependent Python candidate-review
package remains in place while the native execution and review slices are
completed.

This page records the native command contract only. It makes no build, test,
parity, approval, or release-readiness claim.
