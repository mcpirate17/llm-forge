# Native lifecycle hooks

For explicit task context and the default handoff brief, see
[native task briefs](native-session-brief.md).

`forge hook` dispatch runs without a Python interpreter, including all tool and
lifecycle handlers. Payload-consuming events accept at most 1 MiB of UTF-8 JSON, reject duplicate keys and
non-object payloads before doing work, and treat empty stdin as `{}`. Standalone
hook installation still owns only PreToolUse and SubagentStop; it does not add a
second lifecycle handler alongside a host's existing hooks.

PreToolUse and PostToolUse require an unambiguous nonempty tool name. Provider
shell aliases normalize to the same native guard; malformed PreToolUse payloads
produce a deny verdict. Every matching guard runs. The obsolete
`FORGE_NATIVE_HOOKS` partial-selection setting reports a diagnostic and cannot
disable guards or revive interpreter delegation. Budget warnings add context after
guards run; they cannot override a command denial. Defined events without a
registered action are quiet, while unknown event names are errors.

`forge hook EVENT --protocol claude|codex --host ROOT` selects the native output
adapter and binds the host before dispatch. `--project-dir` is an alias for
`--host`; omitting both retains environment-based root selection. Claude is the
default wire format. Codex PreToolUse output preserves denials and input rewrites,
projects `ask` to `deny`, and omits a bare allow verdict. PostToolUse retains stop
decisions and additional context while removing unsupported tool-result rewrite
fields. This ports Forge's existing adapter contract; it does not prove that an
installed client supports every hook or that its raw tool output is bounded.

Three adapter unit tests and four empty-PATH integration tests cover those
projections, malformed input, host binding, journal paths and invalid protocols.
The accompanying release selection passed 411 tests with one existing ignored
test, followed by strict all-target Clippy.

## Session startup

SessionStart initializes the graph gate, consumes background refresh notices,
refreshes the native preamble, activates a staged handoff, checks native binary
freshness when running against a Forge source checkout, and appends host extension
output. The preamble contains the exposure line once. Handoff state and approval
semantics are described in [native-context.md](native-context.md).

A2A identity comes from `A2A_AGENT_NAME` or the first line of
`.agents/a2a/default_identity`. Absent identity skips mailbox startup; a malformed
identity is reported and cannot activate another identity's handoff. The historical
`claude` handoff fallback applies only when no identity is configured. A configured
identity must already be registered; startup does not create registrations.

`forge mailbox start --identity NAME` starts an existing receiver when needed,
retries up to eight queued messages belonging to that sender, and produces a
bounded unread preview. Receiver startup uses a kernel readiness pipe and validates
the native child's PID/name/port plus its loopback Agent Card. An occupied invalid
endpoint is reported; it is never terminated or replaced. Startup writers use a
nonblocking per-identity lock. Startup locks and logs must be private, owned,
regular and singly linked. A log over 1 MiB requires operator rotation.

An automatically started receiver expires after 24 hours by default
(`--lifetime-seconds 1..86400`). Startup detaches it intentionally after readiness;
it continues independently of the short hook process. Expiry drops connections;
SQLite transactions remain atomic. A receiver started earlier is reused and its
lifetime is not extended. This is a local coordination endpoint, not a sandboxed
task executor. Unconfined same-UID writers and the operator-owned registry are
trusted. The Agent Card probe identifies the registered name; it is not remote
attestation.

The sender flush has a two-second deadline and bounded output. A failed flush is
reported alongside the preview; queued or crash-left pending messages remain
eligible for a later retry. Startup never acknowledges inbound messages or injects
full bodies. Preview limits are eight messages, 140 characters per summary and
1,200 characters for the compact JSON envelope. Message transport and retention
remain the existing native mailbox operations.

`forge mailbox startup-hook --provider NAME` supplies the dedicated provider
envelope, including bounded once-per-session state for Grok. Native provider
configuration, rollback and emission semantics are described in
[native-providers.md](native-providers.md).

The aggregate hook bounds the mailbox child to eight seconds, preamble and
handoff children to two seconds each, and the host extension to two seconds.
Each native child has a 64 KiB output cap. File-system work performed by the hook
itself is not a hard real-time guarantee against a stalled filesystem.

## Host extension and freshness

The hook prefers the executable `.claude/hooks/project/session-start`, falling
back to the established host filename `session-start.sh`. `PROJECT_HOOK_DIR` may
select another operator-controlled directory. The extension receives the original
stdin and host path environment. Forge executes it directly, with no shell wrapper
or interpreted dependency of its own; an operator-owned script still requires its
own interpreter. No extension is required on a clean native installation.

Successful UTF-8 stdout is appended as text. A nonzero exit, invalid text, more
than 16 KiB of combined output, or timeout produces a visible diagnostic while
preserving the generic context. Extension output cannot replace the hook JSON or
weaken a verdict. Child groups are cleaned up after execution; persistent work
must be started through an explicit managed service rather than an incidental
background child of the extension.

Build-time SHA-256 covers the Forge, conductor-native and slop-core manifests, lockfiles,
source trees, build scripts, routing policy and compiled ledger calibration data.
The build watches those inputs, including source-directory membership. SessionStart
compares the running binary's fingerprint with a local Forge checkout and reports
source drift. The inventory is capped at 4,096 files, individual files at 2 MiB,
and combined contents at 64 MiB. This detects stale local builds; it does not prove
reproducible builds or authenticate dependencies. Other host projects continue
to use `forge hooks status` and `forge doctor` for installation diagnostics.

## Session completion and journal

SessionEnd runs its existing ledger rollup in a native child with a two-second
deadline, then the native journal with a three-second deadline and 64 KiB output
cap. `FORGE_LEDGER_DISABLE=1` skips only ledger rollup. Failures are visible on
stderr and do not deny session shutdown. SubagentStop uses its existing bounded
native task/ledger completion and emits no dispatcher output.

`forge session journal --host REPO` also accepts a SessionEnd payload directly.
PostToolUse retains the legacy timestamp/kind/path TSV accumulator format in
`FORGE_JOURNAL_DIR`, defaulting to `/tmp/claude-session-journal`. Relative overrides
resolve against the host. The accumulator directory is owned and private; records
and lock files are regular, owned and singly linked. Session IDs are bounded ASCII
identifiers. Embedded TSV separators, malformed timestamps, unknown record kinds,
more than 4,096 unique paths, or accumulators over 1 MiB are rejected.

The journal writes under `OBSIDIAN_VAULT_ROOT/claude`, defaulting to
`~/Documents/CodexVault/claude`. Dates are UTC. Native libgit2 supplies a bounded
status projection: at most 256 entries and approximately 32 KiB. A daily file is
capped at 8 MiB and must be an owned, singly linked regular file; final symlinks
are rejected. Parent directories are operator-trusted.

Under a nonblocking daily lock, publication creates an exclusive 0600 temporary
file, writes the complete day, syncs it, renames it, and syncs the directory. The
source accumulator is consumed only after durable publication. A content-derived
marker suppresses an identical retry in the same daily file. This is not an
exactly-once protocol across date changes, changed end reasons, or external
same-UID edits. Failure preserves the source records for explicit retry.

## Validation and measurements

The initial working-tree validation passed five session-start tests, four mailbox-startup
tests, seven journal tests, 23 hook integration checks, six process-I/O tests and
five bounded-probe tests, plus strict all-target Clippy. Fixtures use native
executables, private temporary stores and synthetic loopback receivers; no live
peer messages or user vault entries are involved. Full migration and clean-host
validation are separate remaining gates.

After that validation, the delegation code and its fake-interpreter executable
fixture were deleted. Seven native dispatch tests replace the obsolete forwarding
tests, covering guards under obsolete environment overrides, malformed input,
provider aliases, budget-warning/deny composition, telemetry, journal/ledger
completion and event selection. They passed before the selected release sweep.
That sweep subsequently passed 592 tests with two existing ignored tests. It
included six startup tests, adding malformed-identity isolation, and both frozen
hook corpora with the intentional native graph-refresh recovery command updated.

The lifecycle benchmark includes child process startup and durable I/O, with 31
warm samples after three warmups on the shared development host. The measured
release binary SHA-256 was
`2964e791df40c59b791b63f3819192bfe9ff9b56289da2456fe00b03fd0206d8`, before the
aggregate startup/journal changes. The measurements therefore describe that
artifact, not the final migration build.

| Operation | Median | p95 |
| --- | ---: | ---: |
| CLI version startup | 0.881 ms | 1.078 ms |
| Durable refresh queue, worker lock already held | 1.257 ms | 1.398 ms |
| Handoff stage with lineage and fsync | 2.272 ms | 2.554 ms |
| Unchanged 128-file structural index | 1.672 ms | 1.877 ms |
| Already-active handoff hook | 0.931 ms | 1.132 ms |

The first 128-file index took 15.666 ms including process startup, producing 256
definitions and 128 resolved calls. These are current-only lifecycle measurements,
not a Python comparison, cold-filesystem study, or model-quality result. Run
`cargo +1.98.0 bench --locked --manifest-path native/forge/Cargo.toml --bench lifecycle`.

Native hook installation no longer accepts `--python` or embeds interpreter
bindings. Reinstalling strips obsolete bindings from owned entries and preserves
unrelated hooks. `forge doctor` checks settings, the native binary, ledger, policy
and a live hook roundtrip through bounded native process supervision. The
installation/doctor cleanup passed 404 selected tests and strict all-target
Clippy, including empty-PATH integration tests. Root package installation and
provider configuration installation remain separate migration work.
