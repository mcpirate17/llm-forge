# Native provider hooks

`forge providers` installs native hook commands into one host configuration at a
time. It does not install a provider client, register identities, start receivers
or modify a Python environment. Claude is the default provider.

| Provider | Configuration | Installed capability |
| --- | --- | --- |
| Claude | `.claude/settings.json` | Five native guard and lifecycle events |
| Codex | `.codex/hooks.json` | Five events with native Codex output projection |
| Qwen | `.qwen/settings.json` | Bounded A2A SessionStart only |
| Grok | `.grok/hooks/workspace.json` | Bounded A2A first UserPromptSubmit per session |

These mappings preserve Forge's existing provider contracts. They are tested
against local configuration and wire fixtures, not live installations of every
client. Qwen and Grok installation does not provide tool guards. See
[native-lifecycle.md](native-lifecycle.md) for output projection limits.

```sh
forge providers --host /path/to/project --provider codex install --mode enforce
forge providers --host /path/to/project --provider codex install --mode enforce --apply
forge providers --host /path/to/project --provider codex check --mode enforce
forge providers --host /path/to/project --provider codex uninstall --apply
forge providers --host /path/to/project --provider codex rollback --apply
```

Install, uninstall and rollback are read-only plans without `--apply`. Plans show
before/after hashes, sizes, modes and desired commands, without printing unrelated
configuration. Install defaults to this running executable; `--binary` selects an
executable named `forge` with a native ELF header and matching version probe.
`check` verifies the requested mode, identity and binary, and runs a bounded
two-second/4-KiB version probe. It never starts messaging.

Generated commands pin absolute host and binary paths with shell quoting.
`--identity` adds a validated `A2A_AGENT_NAME` binding. Otherwise startup resolves
the environment or host default. Tool hooks use `warn` by default; select
`enforce` explicitly. The aggregate SessionStart has a 20-second client timeout.
Qwen's A2A timeout is 15,000 milliseconds; Grok's is 15 seconds.

Use `install --replace-legacy` to migrate recognized Forge Python dispatcher or
A2A startup commands. Without it, recognized legacy entries cause install to
fail before configuration writes. Recognition accepts canonical generated
commands and the exact historical Claude launcher form. Arbitrary shell
snippets, unknown options, other modules and other providers' entries survive.
Uninstall removes recognized native commands only. Other commands in mixed
groups, custom group fields, unrelated events and top-level settings survive.
Hand-edited commands outside recognized forms require operator review; a module
mention alone does not establish ownership.

## Configuration integrity

The selected host and configuration parents must be owned, nonsymlink directories
without group/other write permission. The explicit host resolves once; descendant
symlinks are refused. Existing files must be owned, regular, singly linked and
not writable by others. Files are capped at 1 MiB; strict JSON rejects duplicate
keys. Malformed or unreadable configurations are errors, never empty defaults.
Existing directories are never chmodded. New configuration directories use 0700.

Applied changes hold a permanent nonblocking lock and recheck the planned file's
exact bytes and mode. Publication uses an exclusive temporary file, file fsync,
rename and directory fsync. Existing target permissions survive; new targets,
locks and backups are private. Unconfined same-UID writers and operator-controlled
ancestors remain trusted. This is cooperative serialization.

Each changed operation durably writes `<config>.forge-native.bak.json` before
publishing the target. This bounded snapshot records exact before/after text,
mode and original absence. Rollback restores the previous state only if the
current file matches a recorded state; later edits fail closed. Repeated rollback
is a no-op. Rollback retains the snapshot and does not implement redo. Interrupted
publication can leave either recorded state, recognizable on retry. Configuration
and backup are separate durable files, not a multi-file atomic transaction.

## Native A2A startup adapter

```sh
forge mailbox --host /path/to/project startup-hook --provider grok
```

This command consumes a JSON object on stdin, capped at 1 MiB, and emits a provider
hook envelope containing the bounded unread preview. `--identity`,
`A2A_AGENT_NAME`, then the host's `.agents/a2a/default_identity` select an already
registered identity. The provider name never chooses one. Invalid identity or
malformed/duplicate JSON fails before receiver work. Missing identity is an
error for this dedicated A2A command; aggregate SessionStart may omit A2A when no
identity is configured.

Grok requires a nonblank `session_id` of at most 512 UTF-8 bytes without control
characters. `--once-per-session` enables the same behavior for another provider.
Successful sessions use a SHA-256 digest of provider and session ID; raw IDs are
not stored. A private success set under the identity's `.session-start` retains
up to 4,096 entries for 24 hours, capped at 512 KiB. Live entries are never evicted
to make room. Clock rollback, corruption, capacity exhaustion, unsafe files and
lock contention are explicit errors.

One permanent nonblocking lock serializes startup and success publication. No
live lock file is pruned. Startup failure leaves the session retryable. Success
is recorded durably **before** stdout: a crash or broken output pipe after that
point can lose the first context message, but retry will not duplicate it during
retention. This is at-most-once emission, not exactly-once delivery. A receiver
may already have started before a later error; retry reuses it.

Lock guards explicitly unlock on destruction. Closing a descriptor alone is
insufficient when another descriptor references the same open-file description;
a deterministic duplicate-descriptor regression covers that release boundary.

The preview remains limited to eight messages, 140 characters per summary and
1,200 characters for its JSON envelope. Startup does not acknowledge messages.
Flush has a two-second bound; queued delivery is permitted, while a failed flush
leaves success uncommitted. Existing native startup/lifetime limits still apply.
Registry reads reject final symlinks, FIFOs and duplicate JSON keys; registry
parents remain operator-trusted.

## Validation

The release selection passed 511 tests with one existing ignored test, covering
Forge's binary and library units, provider adapters/installation, native startup,
and mailbox transport, queue, server and retention. Seven shared hook-transform
tests passed separately. Strict all-target Clippy passed. Provider and startup
fixtures run with an empty PATH and private disposable stores; they do not send
messages to real peers or alter installed client configuration.

Coverage includes all four provider mappings, twelve simultaneous Grok hooks,
failure/retry, source identity selection, mixed-group preservation, shell quoting,
exact rollback and later-edit refusal, unsafe files and bounded malformed input.
An initial release failure exposed descriptor-lifetime lock retention; the fix
explicitly unlocks and the added duplicate-descriptor regression passed. These
working-tree results are not a clean-host installation or committed-tree receipt.
