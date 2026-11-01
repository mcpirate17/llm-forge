# Native candidate review foundation

The native candidate foundation exposes exact Git candidate metadata, safe
materialization of a candidate tree, and candidate receipt digest verification.
These commands provide inputs for review; they are not an end-to-end review
gate. Full policy evaluation, checks and test execution, mutation evidence,
attestation, claims, reporting, and approval workflows remain in the Python
`conductor.candidate_review` package until their later native slices land.

## Commands

```text
forge candidate inspect [--repo PATH] --kind index|commit|range
    [--target-ref HEAD] [--base-ref REF]
forge candidate materialize [--repo PATH] --tree FULL_TREE_OID
    --output NEW_DIRECTORY [--dry-run]
    [--max-blob-bytes N] [--max-tree-bytes N]
forge candidate verify-receipt FILE [--repo PATH --ref REF] [--json]
```

All three commands default `--repo` to the current directory. Receipt binding
requires `--repo` and `--ref` together.

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

The native commands expose candidate source facts and a bounded standalone tree
snapshot. They do not choose or apply candidate policy, run analyzers or tests,
admit or authenticate mutation evidence, issue attestations, acquire claims, or
decide whether work may land. Continue to use the existing candidate-review
workflow for those decisions. The interdependent Python candidate-review
package remains in place while the native policy and execution slices are
completed.

This page records the native command contract only. It makes no build, test,
parity, approval, or release-readiness claim.
