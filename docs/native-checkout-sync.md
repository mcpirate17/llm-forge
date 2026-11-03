# Native guarded checkout sync

`forge checkout-sync` preserves the staged and unstaged layers of a dirty checkout
while fast-forwarding its branch to an existing integration ref. Resolution is
offline: the command never fetches or queries remote advertisements. An explicit
`--target` names a local ref; the default uses the workspace integration policy.
Symbolic/short ref names resolve to their full direct ref before admission.

```sh
forge checkout-sync --repo /path/to/checkout --dry-run --json
forge checkout-sync --repo /path/to/checkout --recovery-dir /trusted/private/recovery-parent --json
forge checkout-sync --recover /trusted/private/recovery-parent/checkout-sync-recovery-ID --json
```

An applying transaction requires an explicit pre-existing recovery parent, owned by
the effective user and mode 0700, outside the checkout and Git directories and
not their ancestor. The operator chooses that trusted directory;
Forge does not create an implicit recovery parent. Already-even reports need no
recovery parent. Each transaction retains its
own private journal, recovery blobs and snapshot ref. `--recover` accepts the
exact existing transaction directory containing `journal.jsonl`, not a parent,
ref, arbitrary JSONL file or glob. It cannot be combined with forward-sync flags.
Completed or already-recovered journals are reported as such; they are not
instructions to undo a successful forward transaction.

`make checkout-sync CHECKOUT_SYNC_RECOVERY_DIR=/trusted/private/recovery-parent`
routes to the native actor. For preview, use
`make checkout-sync CHECKOUT_SYNC_ARGS='--dry-run --json'` without a recovery parent.
The older Python module and its compatibility targets have been deleted. This
native interface uses the versioned result contract below.

## Preview and transaction boundaries

Dry-run reads the original Git index through a sealed in-memory parser and reads
existing blobs and no-follow worktree observations. It uses the same path
reconciliation, EOL clean/smudge conversion, collision checks and operation
scheduler as execution. It does not call the snapshot creator, write Git objects,
create a snapshot ref/journal, publish an index, or change worktree entries.
The cooperative lease can create or retain `.git/governance/commit-review.lock`
and its parent directory (under the Git common directory for linked layouts).
That lock metadata is the sole intended dry-run persistence exception.

Capture is bounded to 100,000 visited filesystem entries, 256 directory levels,
64 MiB per regular/blob entry, 4,096-byte symlink targets, and a cumulative 2 GiB
of captured base, staged, incoming and worktree bytes. Intermediate maps and
conversion buffers can require more memory than the input-byte budget. Ignored
leaf names and empty directories remain collision barriers; ignored content is
not read. Non-UTF-8 paths, nested Git metadata, unsupported file types, hardlinked
admitted leaves, unsupported index flags/extensions, custom filters and unsupported
encoding/ident transforms fail closed.

The preview rechecks branch/target/index authority, lease, inventory, admitted
leaf content/identity, directory identity and effective filter policies around
capture. It reports fast-forwardability only after reconciliation succeeds.
Ordinary execution repeats authoritative admission under its lease and Git locks,
creates a snapshot, journals every publication, and conditionally restores owned
preimages on failure. A preview is an observation, not a lease reservation for a
later execution. Writers of affected paths, attributes, config and ignore policy
must honor the cooperative lease. Neither preview nor execution promises atomicity
against arbitrary writers that ignore it; conditional recovery refuses to
overwrite a changed foreign inode and retains evidence when the outcome is uncertain.

SIGINT/SIGTERM use the native interrupt supervisor and cancellation callbacks.
Forward failure cleanup may finish conditional recovery with cancellation
suppressed to preserve safety; the original signal is propagated after leases and
actor state have dropped. Explicit recovery uses the same native journal parser,
source verification and guarded conditional rollback.

## Result contract

JSON objects include `schema_version: 1` and `interface: "forge.checkout-sync"`.
Forward reports contain `branch`, `target` (full direct ref), `old`, `new`, `ahead`,
`behind`, `dry_run`, `fast_forwardable`, `blocked_by`, `snapshot`,
`recovery_directory`, `planned_index_paths` and `planned_worktree_operations`.
An even checkout has no captured plan, so its two planned counts are null.
`fast_forwardable` describes reconciled dirty state, not ancestry alone.

`outcome` is `even`, `would-fast-forward`, `fast-forwarded` or `blocked`.
Recovery outcomes are `journal-marked-complete`, `journal-marked-recovered` or
`recovered`; recovery adds `retained_depot` and `recorded_slots` for retained
artifacts. Errors emit a JSON error envelope when `--json` is set and a diagnostic
on stderr. They do not return a success-shaped drift report.

Exit 0 means a successful preview, even checkout, completed transaction or native
recovery result. Exit 3 is an expected semantic refusal: ahead/diverged commits,
path collision, reconciliation conflict or an unsupported effective attributes
change. Exit 2 is argument, capture, authority, policy/runtime or recovery failure.
Native signal termination is propagated as a signal after safety cleanup.

Unlike legacy Python results, there is no `remote` alias or per-path
`classification` dictionary; `target`, explicit planned counts and the versioned
outcome contract replace those surfaces. Snapshot names use
`refs/snapshots/forge/checkout-sync-*`, not the legacy Python namespace.
