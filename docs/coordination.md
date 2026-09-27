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
  -- cargo test --locked --test task_lifecycle -- --test-threads=1
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

Execution currently requires Linux for resource admission. Child processes run
in an isolated process group. Timeout, cancellation,
SIGINT, and SIGTERM stop that group; cancellation is observed at the next heartbeat.
On Linux, parent death also kills the direct child. This is process supervision,
not a filesystem/network sandbox. Descendants that deliberately detach can escape
the group, especially on abrupt supervisor death; use an OS service/cgroup or
sandbox when enforcing that boundary is required. The supervisor does not claim
authorization to run a command, launch training, or approve a gate. The CPU and
memory options below add a cgroup boundary for the process tree.

## Resource admission and limits

Forge 0.3 records a live CPU and RAM snapshot before execution. CPU availability
accounts for affinity and ancestor cgroup quotas; RAM availability accounts for
the host's available memory and ancestor cgroup headroom. Inspect admission
without creating task state:

```sh
forge task resources --host /path/to/project --cpus 2 --memory-mib 4096 --max-load 8
forge task run --host /path/to/project checks --owner agent-a \
  --cpus 2 --memory-mib 4096 --max-load 8 --timeout-seconds 120
```

`--cpus` is a process-tree CPU quota, in CPU equivalents (minimum 0.01).
`--memory-mib` caps process-tree memory and disables swap for that scope.
Both require a working systemd user manager, `systemctl`, `systemd-run` with
`--expand-environment=no` support, and delegated cgroup v2 controllers. Forge
refuses an unavailable backend. A native guard reads the kernel's applied limits
before starting the stored command; missing or weaker limits fail the attempt.
The runner stops its scope after success or failure, including descendants that
detach from the original process group. A systemd runtime deadline also bounds
the scope if the supervisor dies. These controls do not prevent a command with
the same user's privileges from deliberately escaping into another service.

`--max-load` rejects execution when the host's one-minute load average exceeds
the supplied value; it does not throttle an already running task. Resource
admission and backend availability checks precede retry/start, so rejection
preserves the task's state and attempt budget. The resource preview checks
admission only; it does not launch a scope or verify backend readiness. Snapshots
are observations, not reservations, so competing tasks can change availability
immediately afterward. CPU/RAM caps are opt-in; omitting them leaves those
resources uncapped by Forge.

Systemd places the command in a new user scope. Forge carries observed ancestor
CPU/memory caps into that scope when the corresponding flag is omitted. This
preserves those per-task ceilings, but the new scope does not share the caller's
original aggregate cgroup budget. Use a delegated child cgroup or container when
multiple tasks must remain inside one existing aggregate budget.

Commands default to CPU-only GPU visibility, even when the caller exposes GPUs.
Explicit NVIDIA selection optionally requires free VRAM:

```sh
forge task resources --gpu GPU-uuid --vram-mib 8192
forge task run train --owner agent-a --gpu GPU-uuid --vram-mib 8192 \
  --cpus 4 --memory-mib 16384 --timeout-seconds 300
```

GPU selection uses a bounded, read-only `nvidia-smi` query and pins the selected
device by UUID. It honors inherited visibility restrictions; ambiguous numeric
CUDA masks are rejected instead of widening access. CPU-only execution does not
query the GPU. Visibility variables are cooperative device selection, not a GPU
security boundary. Free VRAM is an admission check, with no reservation or hard
VRAM cap; Forge never stops other GPU jobs.

Execution receipts use schema version 2 and include `succeeded`, requested
resources, the availability snapshot, and kernel-verified CPU/RAM limits when
requested. The runtime deadline is recorded as requested, separately from those
verified limits. Failed launches and interrupted executions also receive failure
receipts when storage remains writable; unavailable output summaries are null.
The attempt's `resources.json` contains the native guard's limit readback.

The lifecycle and resource tests use native Rust process fixtures. After checking
CPU, RAM and VRAM availability, run the portable Linux checks serially:

```sh
CUDA_VISIBLE_DEVICES='' CARGO_BUILD_JOBS=2 cargo test --locked \
  --manifest-path native/forge/Cargo.toml \
  --test task_lifecycle --test task_resources --test task_preflight -- --test-threads=1
```

Real systemd enforcement checks are opt-in because ordinary CI may lack a user
manager or delegated controllers. On a suitable Linux host, this command also
checks detached-child cleanup, delayed finalization and a memory allocation
deliberately exceeding its 64 MiB scope. Each task is capped at 0.5 CPUs and
64 MiB; it uses no GPU:

```sh
CUDA_VISIBLE_DEVICES='' CARGO_BUILD_JOBS=2 timeout 120s cargo test --locked \
  --manifest-path native/forge/Cargo.toml --test task_resources --test task_preflight \
  -- --ignored --test-threads=1
```

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
