# Durable local task coordination

`forge task` supplies a Rust/SQLite task lifecycle for manual agent work and
explicit command execution. It is independent of a particular model or harness.
Build the current checkout with `cargo build --manifest-path native/forge/Cargo.toml`
and use `native/forge/target/debug/forge` (or install that reviewed build locally).

All commands accept `--host /path/to/project`. State is stored under that host's
`.agents/forge/tasks.sqlite3`, using WAL, full synchronous writes, and transactions
that record the task update and its history together. Add `.agents/forge/` to a
host's ignore rules if it does not already ignore `.agents/`.

## Assign and run

```sh
forge task create --host /path/to/project --id checks --title "Run focused checks" \
  --owner agent-a --session session-id --max-attempts 2 \
  -- .venv/bin/python -m pytest tests/test_feature.py -q
forge task run --host /path/to/project checks --owner agent-a --timeout-seconds 60
forge task show --host /path/to/project checks
```

The executable receives literal argv, with no implicit shell. Execution is an
explicit action: creating, assigning, listing, or inspecting a task never starts
an agent or a command. A task without `--owner` starts queued; `task assign ID
--owner NAME` assigns it. `--depends-on ID` is repeatable; each dependency must
already exist and succeed before execution can begin.

States are `queued`, `assigned`, `running`, `succeeded`, `failed`, and `cancelled`.
Every start increments an attempt number, consumes the original retry budget,
and takes a lease. Atomic acquisition prevents two workers starting the same
attempt. Stale owners/attempts cannot heartbeat or finish a replacement attempt.

`run` maintains its own lease (default 30 seconds), enforces a wall-clock deadline
(default 300 seconds), and retains at most 1 MiB of each output stream by default.
Use `--log-bytes` to select a smaller bound; excess bytes are drained and counted.
Logs and an execution receipt live under `.agents/forge/runs/ID/ATTEMPT/`.
The journal binds the receipt's path and SHA-256. A successful command exits zero;
nonzero exit, timeout, interruption, or lost lease fails the attempt.

On Unix, child processes run in an isolated process group. Timeout, cancellation,
SIGINT, and SIGTERM stop that group; cancellation is observed at the next heartbeat.
On Linux, parent death also kills the direct child. This is process supervision,
not a filesystem/network sandbox. Descendants that deliberately detach can escape
the group, especially on abrupt supervisor death; use an OS service/cgroup or
sandbox when enforcing that boundary is required. The supervisor does not claim
authorization to run a command, launch training, or approve a gate.

## External agents and recovery

An agent executing through its own harness uses the same lifecycle:

```sh
forge task start work --owner agent-a --lease-seconds 60
# Use the attempt number returned by start:
forge task heartbeat work --owner agent-a --attempt 1 --lease-seconds 60
forge task finish work --owner agent-a --attempt 1 --receipt results/checks.json
# Or: forge task fail work --owner agent-a --attempt 1 --reason "checks failed"
```

`--claim`, `--session`, and `--message` on creation link existing governance,
harness, and messaging evidence. A linked claim must be active for the owner
when starting or heartbeating. A task does not create or expand that claim.
An external `finish` records the worker's report; attaching a receipt binds its
bytes, but does not validate its verdict or grant approval.

`task retry ID --owner NAME` recovers a failed task or an expired running lease.
It refuses active leases, successful/cancelled tasks, and exhausted budgets.
`task run ID --owner NAME --resume` explicitly performs recovery and reruns the
stored argv from its beginning. Commands must therefore tolerate repetition;
this is not a process checkpoint or exactly-once side-effect guarantee.
`task cancel ID --owner NAME` prevents further completion by that attempt.

If SQLite is unavailable during completion, the worker stops its child and
reports the error; a running record may remain until its lease expires. Inspect
the attempt's files and history before choosing recovery.

## Platform health

```sh
forge status --host /path/to/project --json
forge status --host /path/to/project --json --check --verify-receipts
```

Status reads tasks, active claims, configured harness events, mailbox delivery
counts, messaging supervisor state, and linked ledger session totals. It never starts a
service, marks messages read, or initializes a missing task/mailbox database.
Registry credentials and message bodies are excluded. `--state-dir` and
`--ledger-root` select existing alternative stores.

`--verify-receipts` hashes task receipts and identifies missing/changed bytes.
Without it, existing receipt content is explicitly unchecked. Expired leases,
inactive linked claims, changed receipts, and pending messages without a live
supervisor produce warnings. Broken state produces errors rather than a clean
zero. `--check` returns nonzero for warnings as well as errors.

Task output is bounded (`--limit`, default 100) and includes total counts and a
truncation flag. Hook configuration is reported as unverified; use the selected
provider's bootstrap `--check` and hook doctor to verify its execution contract.
Task `session_id` joins the existing ledger by both session ID and host project
path. The read is bounded to the latest 31 day files and 8 MiB, with truncation
and unmatched sessions reported. Shared sessions appear once; their token totals
are session-level usage, not per-task cost attribution. Status does not imply the
ledger has been freshly rolled up or convert missing records into zero usage.

For durable delivery, retry supervision and delivery history, see
[messaging](messaging.md). For portable correctness audits, see
[guardrail auditing](guardrail_audit.md).
