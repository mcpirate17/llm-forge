# Full native workspace hygiene

`forge workspace-hygiene` is the explicit full read-only report. Session-start
hooks retain the separate cheap offline exposure surface; full import, PR,
registry and worktree observations do not run implicitly on that hot path.

```sh
forge workspace-hygiene --repo /owned/repo
forge workspace-hygiene --repo /owned/repo --json
forge workspace-hygiene --repo /owned/repo --live-ref origin/main --json
forge workspace-hygiene --repo /owned/repo --exit-nonzero-on-exposure
make workspace-hygiene
```

The default live ref is the current HEAD branch; detached HEAD requires
`--live-ref`. Landed containment uses the resolved integration line, preferring
its origin tracking ref to a local branch. Configured integration names and
operator environment override use the shared project-path resolver. Reports may
query a remote HEAD advertisement only as a last resort; the cheap session path
never does. Git is required; optional gh enables PR observations and unavailable
or failed PR queries remain visible.

The JSON retains the existing full report fields: live/landed refs, redundant
branches, stale/dirty/landed worktrees, expired/idle claims, unpushed branches,
orphan/dangling manifests, untracked import closure, stale feature branches,
GitHub availability, local-only commits, stale dirty files, worktree leases and
checkout drift. Native `gh_errors` additionally preserves optional PR failures.
The human report also includes the read-only reaper preview.

Claims are read directly in the native process from the shared validated store.
`FORGE_BIN` or a repository-selected executable cannot substitute claim rows.
Report expiry uses declared expiry; report idle uses the later of claim creation
and claimed-path mtimes with the existing 90-minute threshold. It is distinct
from the claim lifecycle's activity/deadline rules. Missing claimed paths do not
become invented idle observations.

The report never deletes a branch, worktree, lease or claim. Primary `.git`
directory layouts are excluded from landed-removal observations. A missing
upstream ref is not proof of merging: such rows say merge is unverified and are
marked review-only. The retained `checkout_drift.fast_forwardable` field tests
tracked overlap only; it is not actor admission for untracked/ignored collisions.
No report row authorizes a checkout mutation or proves a later removal safe.

Native Git probes have deadlines/output bounds, clear inherited Git overrides,
disable optional index writes and external diff/textconv/fsmonitor hooks, and
supply explicit configuration/helper controls. They use operator-installed Git
and optional SSH/GitHub tooling, not a kernel sandbox or a publisher-authenticity
proof. Global/system Git configuration is disabled for these probes; unsupported
credential-helper-dependent remote discovery fails rather than launching an
unbounded repository helper. Existing local integration refs normally resolve
offline.

Malformed claims/leases/registry input, impossible calendar dates, import parse
or read failure, unsupported path encodings and resource overruns refuse visibly.
This is stronger than the old import scanner's partial findings on parse failure.
JSON documents are regular/no-final-symlink and at most 4 MiB, with 32 MiB per
registry/lease lane and 100,000-row inventories. Existing bounded import closure
limits apply (16 MiB/source, 1 GiB total, 100,000 source/lookup paths and lookup
byte bounds). Git probes retain at most 1 MiB combined output within 15 seconds;
serialized report output is capped at 64 MiB. These are input/output limits,
not an OS memory reservation or an atomic whole-repository snapshot.

A produced report exits 0 by default. `--exit-nonzero-on-exposure` preserves the
legacy predicate: stale feature branches, local-only commits or stale dirty files
produce 1. Other cleanup/import/claim/drift findings do not implicitly broaden
that predicate. Native input/setup errors use exit 2, replacing the former
unhandled Python exception path. A successful report is not a gate PASS.

The Python workspace module and checkout-sync actor have been deleted. Checkout
mutation is provided separately by [native checkout sync](native-checkout-sync.md).
Reporting does not invoke that actor. The broader package migration remains open.
