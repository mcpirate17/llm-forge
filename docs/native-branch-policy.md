# Native branch policy and exposure

`forge branch-policy` validates branch names, records branch/claim bindings,
audits local policy and reports workspace exposure. Bind/unbind update binding
metadata only. These commands never create, switch, delete or push a branch,
create/remove a worktree, or execute the exposure report's cleanup suggestions.
Repository policy still determines whether branch operations are permitted.

## Commands

Run these against an explicit existing repository:

```sh
forge branch-policy --repo /owned/repo check-branch agent/topic-20261102
forge branch-policy --repo /owned/repo --json status
forge branch-policy --repo /owned/repo audit
forge branch-policy --repo /owned/repo exposed
forge branch-policy --repo /owned/repo --json exposed
```

Names have the shape `<agent>/<topic>-<yyyymmdd>`. The agent starts with a
lowercase ASCII letter and continues with lowercase letters, digits or hyphens.
The topic starts with a lowercase letter or digit and uses the same alphabet.
The date must be a real calendar day. Refusals include a suggested spelling;
validation does not rename anything.

The shipped Make aliases route directly to native Forge:

```sh
make branch-policy-status
make branch-policy-audit
```

`branch-policy` is also an alias for status. The aliases use
`CONDUCTOR_HOST_ROOT` and the configured `FORGE` executable.

## Binding metadata

When the operator intends to record a mapping:

```sh
forge branch-policy --repo /owned/repo bind \
  --branch agent/topic-20261102 --claim gr-example --owner agent
forge branch-policy --repo /owned/repo unbind --branch agent/topic-20261102
```

The owner defaults to the branch's agent segment and must match it. A binding
records the supplied claim identifier; it does not create a governance claim,
authorize work, prove the claim is live, or create the named branch. A second
live local branch for the same recorded claim is refused. Binding the same
branch replaces its row with a new creation timestamp and unset push/PR fields.

The schema-versioned store is
`<common Git directory>/governance/branch-bindings.json`, shared by repositories
using that common Git directory. Native persistence uses an owned directory,
a permanent advisory lock, bounded regular-file reads, a temporary file and
atomic replacement with directory synchronization. Symlink/special-file stores,
hardlinks, unsafe ownership/permissions and malformed data refuse visibly.
Cooperating writers serialize; trusted parents and same-user writers remain an
operator boundary. A missing store reads as empty; unbinding a missing mapping
does not create one.

Native claim creation also creates new shared governance directories with mode
0700. Existing directory permissions are preserved; an existing writable-by-others
binding directory is refused rather than silently tightened.

`check-branch` returns 0 for valid names and 1 for a naming refusal. Bind returns
0 on success and 1 for a reported binding conflict; other malformed inputs or
persistence errors follow the CLI error path. Unbind returns 1 when no mapping
exists. Audit returns 1 for findings; status and exposure return 0 when their
reports were produced. Inspect report fields as well as exit status: an exposure
report may explicitly mark unavailable observations.

## Status and local audit

Status lists stored bindings, creation/push timestamps, PR number and age. The
age uses the last recorded push when available, otherwise creation time; more
than six hours is marked stale. Status does not fetch a remote or discover a PR.

Audit checks local branch names, multiple live branches for one recorded claim,
and whether a configured integration branch would rewrite the locally recorded
`origin/<branch>` line. It uses existing refs; it does not fetch or prove current
remote state. Integration names come from the shared project configuration:
`integration_branch` defaults to `main`, `CONDUCTOR_INTEGRATION_BRANCH` overrides
it, and `retired_integration_branches` are also treated as integration names.
See [project configuration](native-project-config.md).

## Read-only exposure report

`exposed` reports full rows and reasons for:

- Stale feature branches, their recorded claim/owner and push/PR observations.
- Commits on HEAD absent from recorded remote refs, associated with an active
  overlapping claim when one is found.
- Dirty or untracked files older than 24 hours, with age and claim association.
- A worktree cleanup preview with reasons; this command has no apply operation.

Branch push age uses recorded push metadata when present; a remote-tip commit
date is explicitly labeled an approximation. No push for six hours and no open
PR after 24 hours are separate observations. Integration branches and the live
integration ref are excluded from feature-branch reporting.

The report uses native Git/config/claim logic and bounded read-only Git probes.
`git` must be available on PATH for those probes. Optional `gh` on PATH enables
GitHub PR queries using the operator's existing authentication/network access.
Missing `gh` is labeled and PR staleness is skipped; query failures are retained
as report errors. No login, fetch, branch deletion or reaper execution occurs.
Remote-state probes can be unavailable; the report is an observation, not cleanup
authorization or proof that deletion is safe.

The preview holds the primary/current/locked/in-use worktree and active leases.
Malformed leases, including impossible calendar dates, are held rather than
treated as expired. Process inspection and other unavailable inputs can also
prevent a cleanup suggestion. Suggestions can include containment, merged PR,
gone remote branch, expired lease, idle files or stale registration; they do not
remove registrations, paths, refs or claims.

The full [native workspace hygiene report](native-workspace-hygiene.md) now reads
claims and all report inputs in process; `make workspace-hygiene` routes directly
to it. The Python module and old exposure transport still remain for the legacy
checkout-sync dependency and compatibility callers. Their complete retirement
requires the guarded checkout-sync actor; this report does not implement it.
The native branch-policy CLI and its Make targets do not invoke the deleted
Python branch-policy module.

## Evidence boundary

All eleven native CLI/persistence/exposure regression cases passed on the
integration host, including lease-date refusal, concurrent writers, native-claim
directory interoperability and optional-tool observations. The fixtures do not
establish live GitHub availability or permission to perform cleanup. Full release
validation is recorded separately by the committed-tree check receipts.
