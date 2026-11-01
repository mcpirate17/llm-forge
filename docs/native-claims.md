# Native ownership claims

`forge claim` manages structured ownership records in the repository's common
Git directory. It resolves linked checkout metadata without launching Git.

```console
forge claim create --repo /path/to/host --owner lane-a \
  --justification "Fix the parser" src/parser.rs
forge claim list --repo /path/to/host --compact --paths
forge claim touch --repo /path/to/host --owner lane-a claim-ID
forge claim release --repo /path/to/host --owner lane-a claim-ID
```

Without `--compact`, `list` emits JSON with the stored claims and their digest.
The digest excludes activity timestamps. `--path PATH` filters by overlap.
The `governance-claim`, `governance-release-claim`, `governance-claims` and
`governance-claims-json` Make targets invoke these native commands.

Creation requires exact relative paths, a named lane and a justification.
Traversal, globs, broad roots and overlaps with active claims are refused.
The default estimate is 45 minutes and the default maximum is 60 minutes;
`--expected-minutes` and `--max-minutes` must satisfy
`0 < expected <= maximum <= 120`. The maximum lifetime is unchanged by a touch.
Idle expiry and the shorter idle window after the estimate continue to apply.

Native create, release, list and explicit touch operations acquire the shared
`governance/commit-review.lock` before loading state. The native write hook uses
the same lock for activity updates, with a 200 ms contention bound; failure is
reported and the write gate refuses the operation. CLI transactions wait at most
30 seconds. Lock files must be owned regular files with one link, without write
permission for other users. Legacy readable lock files are accepted. Symlinks
and hard-linked lock files are refused.

Publication uses private staging files and durable atomic rename. The claims
and activity stores are separate files: their publication is not a single atomic
transaction. Activity for an absent claim is ignored and pruned during lifecycle
updates. Native overlapping creations and concurrent activity updates serialize
under the same lock.

Remaining Python governance and session callers have not all adopted this
transaction protocol. Mixed native and unlocked legacy writers do not yet have
a global concurrency guarantee. Native commands also refuse inherited
`LLM_GOVERNANCE_COMMIT_LOCK_FD` or `LLM_GOVERNANCE_COMMIT_LOCK_TOKEN` leases;
they do not assume that an environment variable proves lock ownership.

The lifecycle and CLI tests cover competing creations, concurrent touches,
owner checks, activity cleanup, linked common-directory metadata, lock timeout,
unsafe paths and lock files, and execution with an empty `PATH`.
