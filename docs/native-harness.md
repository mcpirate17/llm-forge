# Persistent native tool service

`forge harness` connects the compiled tool registry to native source tools,
confined PTYs and the existing SQLite task journal. It requires Linux Landlock
ABI 6 or newer, seccomp user notifications and a systemd user manager/client
version 254 or newer. Start fails if enforcement or controller readback fails.
There is no unconfined fallback. No Python or Node process is involved.

The operator supplies capabilities once, when starting the service. Model calls
can select a configured executable and arguments, inspect source, or apply an
edit to an exact permitted path. Calls cannot add executable/read/write
capabilities, change the resource ceiling or enable networking. Filesystem,
metadata and kernel limitations are documented in [native execution](native-execution.md).

## Operator policy and startup

Save a policy such as this, replacing the absolute source path:

```json
{
  "version": 1,
  "source_root": "/absolute/project",
  "write_paths": ["src/main.rs"],
  "read_only": ["/usr", "/lib", "/lib64", "/etc/ld.so.cache"],
  "programs": {
    "compiler": {
      "executable": "/usr/bin/cc",
      "fixed_arguments": [],
      "environment": {},
      "working_directory": "scratch"
    }
  },
  "aggregate": {"cpu_millis": 1000, "memory_mib": 640, "processes": 128},
  "session": {"cpu_millis": 500, "memory_mib": 64, "processes": 32},
  "max_sessions": 2,
  "max_retained_sessions": 8,
  "max_tasks": 128,
  "lifetime_seconds": 3600,
  "session_seconds": 300,
  "retention_seconds": 300,
  "replay_bytes": 65536,
  "log_bytes": 1048576
}
```

```sh
forge harness start --policy operator.json
forge harness catalog --socket /returned/state/socket
forge harness grammar --socket /returned/state/socket
forge harness stop --socket /returned/state/socket
```

Startup returns a verified ready socket and the unique transient service name.
State defaults to the native XDG state root under `llm-forge/h`; override it with
`--state-parent /short/private/path`. Linux Unix-socket path length must fit 107
bytes. Each run creates a new mode-0700 directory; its socket is mode 0600 and
both ends check peer UID. State must be disjoint from source and read/executable
capabilities. Existing sockets are never removed to take over a service.

`harness preflight --policy operator.json --state-parent /existing/state-parent`
checks policy, catalog, kernel ABI, manager availability and resource admission
without launching a payload. It does **not** certify launch-time enforcement;
the service and each child independently verify their actual controls.

The manager owns a finite-runtime service, with the supervisor in a delegated
subgroup and each terminal in an individually limited cgroup. Main-process death
causes manager cleanup of the whole subtree, including detached descendants.
The service verifies its manager PID, lifecycle policy and actual aggregate
controller values before accepting requests. Only its newly owned service root
has controllers enabled; shared parent slices are untouched.

## Calls and reconnects

The socket accepts one UTF-8 JSON object per newline-delimited request, with a
1 MiB frame cap, 64 concurrent connections and bounded output/wait deadlines.
The catalog and GBNF are compiled once. Disk, SQLite and source parsing run in
a bounded native worker pool; terminal I/O uses the shared Tokio reactor.

Save a request and use `forge harness call --socket SOCKET --request FILE`:

```json
{"method":"call","call":{"name":"terminal_spawn","arguments":{"id":"compiler-version","program":"compiler","arguments":["--version"]}}}
```

Replies contain `ok` and either `result` or `error`. A successful call result
contains the handler's `result` and a `repaired` flag. Each frame is validated
before any handler runs, preserving duplicate keys instead of silently accepting
the last value. `call_text` accepts an `input` string and explicit `repair:true`
for the sole supported repair: removing an outer Markdown fence and revalidating.

The catalog exposes:

- `code_inspect`, `code_fold`, and, when write capabilities exist,
  `code_replace`, `code_diff`, `code_restore`. Edits default to a dry run;
  `apply:true` still requires the operator's exact path capability, current hash,
  syntax checks and native Git recovery. Internal control paths are excluded.
- `terminal_spawn`, `terminal_read`, `terminal_write`, `terminal_resize`,
  `terminal_interrupt`, `terminal_close`, `terminal_info` when executables exist.
- `task_get` and `task_list` for durable task records, leases and receipt references.
- `code_search` when `code_indexes` capabilities exist; see
  [native code search](native-code-search.md#persistent-harness-search).
- `context_count`, `context_compact`, and `context_recover` when a pinned
  `context` capability exists; see [native context](native-context.md).
- `session_brief` when a `workspace` capability exists, and `workspace_recall`
  for named `memory_indexes`; see [workspace context](native-session-brief.md#persistent-service).

After a lost spawn response, reuse the same `id` and identical arguments. A live
or retained session returns its existing PID; different arguments are rejected.
After replay expiry the durable task ID prevents relaunch. Never invent a new ID
to retry an uncertain launch. Sessions do not automatically resume after daemon
death; an expired journal lease is unresolved execution evidence, not success.

Terminal input/output uses base64 to preserve arbitrary bytes. Reads have independent
absolute cursors and explicit eviction. Writes use a sequence starting at one;
the last 64 submissions retain a payload hash and accepted prefix. Retrying the
same sequence does not deliver bytes again or restart its deadline. An expired
sequence is rejected. A timed-out partial write requires the caller to account
for its accepted prefix before submitting the remaining bytes as a new sequence.

The journal records one attempt per session and renews its lease during execution.
Completion persists the resource report, bounded-log accounting and a SHA-256
receipt. Graceful shutdown closes all session cgroups and waits for receipt
publication. A manager-enforced kill can prevent final publication; retained logs
and expired leases preserve that uncertainty.

## MCP clients

After starting an authorized service, configure the client's command as the
absolute `forge` binary with arguments
`harness stdio --socket SOCKET --source-root /absolute/project`. The optional
source-root argument requires a matching live service before discovery. Use
[`forge mcp bind`](native-mcp-setup.md) for bounded, reversible project setup. This
native adapter communicates through bounded newline-delimited JSON-RPC on pipes;
only protocol messages go to stdout. It supports
[MCP 2026-07-28 per-request metadata](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning)
and legacy `initialize`/`notifications/initialized` for 2025-11-25, 2025-06-18
and 2025-03-26. Modern discovery/catalog results have private cache scope.

The adapter exposes the operator's exact catalog through `tools/list` and forwards
validated `tools/call` requests. It handles at most 16 in-flight calls while
remaining responsive to other requests during long terminal reads. Repeated
in-flight RPC IDs fail; malformed parameters cannot become valid through JSON
key collapse. Tool failures are visible content with `isError:true`.

RPC cancellation suppresses the eventual response. It cannot undo a tool already
handed to the persistent daemon; use `terminal_close` to stop an owned process.
Closing or restarting the adapter leaves terminal ownership with the finite-lived
service. Retain session IDs, write sequences and cursors when reconnecting.

## Bounds and limits

Aggregate memory must cover session reservations plus native supervisor buffers
and retained replay. CPU quota is shared at the service root as well as constrained
per session. Task and replay capacities reject new work when exhausted; state and
scratch artifacts are retained for explicit recovery/cleanup. `log_bytes` bounds
captured output, **not** scratch filesystem usage. The current backend does not
provide a scratch disk quota or namespace-based metadata privacy.

Same-UID unconfined host programs remain trusted. A source edit can conflict with
an unrelated editor within the documented final rename window. An operator should
grant only the source files and read capabilities needed for the work. This policy
is an execution capability declaration, not a scientific approval or gate receipt.

The live integration tests are opt-in because they start transient services:

```sh
cargo +1.98.0 test --locked --manifest-path native/forge/Cargo.toml \
  --test native_harness --test managed_service -- --include-ignored --test-threads=1
```

They exercise actual manager cleanup, daemon crash, detached children, API
reconnects, idempotent writes, policy/schema rejection, transactional patches,
concurrent clients and durable shutdown. Test-only fixtures use no interpreter.
