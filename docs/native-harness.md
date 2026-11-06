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

## Optional outbound MCP tool

An operator may add one `outbound_mcp` object to the policy. This is separate
from `programs`: it does not grant `terminal_spawn` access to the endpoint or let
callers choose its executable or arguments. Replace every path and digest below
with an independently measured absolute path and lowercase SHA-256; the digest
shown is a placeholder. Add each required executable, runtime library and input
read path to the enclosing policy's `read_only` or use a path beneath its
`source_root`; apply the usual metadata, credential and Git-directory exclusions.
The endpoint's `read_only` entries must stay within those operator grants.
Admission rejects group/world-writable bound files, directories and non-sticky
ancestors. Use explicitly prepared private inputs; do not change permissions on
an unrelated shared checkout to make a policy pass. Directory read grants also
allow descendant execution and do not constitute a complete executable allowlist.

```json
"outbound_mcp": {
  "public_name": "outbound_lookup",
  "remote_name": "lookup",
  "executable": {"path": "/absolute/tools/server", "sha256": "0000000000000000000000000000000000000000000000000000000000000000"},
  "fixed_arguments": ["--stdio"],
  "pinned_inputs": [],
  "read_only": ["/absolute/tools/server"],
  "input_schema_json": "{\"type\":\"object\",\"properties\":{\"query\":{\"type\":\"string\",\"maxLength\":256}},\"required\":[\"query\"],\"additionalProperties\":false}",
  "output_schema_json": "{\"type\":\"object\",\"properties\":{\"answer\":{\"type\":\"string\",\"maxLength\":4096}},\"required\":[\"answer\"],\"additionalProperties\":false}",
  "resources": {"cpu_millis": 500, "memory_mib": 128, "processes": 16},
  "max_wall_seconds": 60
}
```

This fragment belongs inside the complete policy object; it is not a standalone
policy. `public_name` must begin `outbound_`. The input and optional output schema
fields are JSON **strings**, accepted by Forge's bounded schema profile; they
must structurally match the selected remote tool's advertised schemas. Omit
`output_schema_json` only if the selected tool advertises no output schema.
`pinned_inputs` may contain `{ "path": "...", "sha256": "..." }` entries for
operator-selected files; hashes are checked at admission and around calls. A
read-only directory grant is a capability, not a recursive content pin. Adjust
the aggregate resource budget to include the outbound reservation. Preflight
validates policy and admission without launching the endpoint.

The catalog exposes only the configured public tool through both `harness call`
and the existing MCP stdio adapter. The first call starts the fixed endpoint and
performs MCP 2025-11-25 initialization and bounded discovery over stdio; calls
permit one in-flight request and reject concurrent calls as busy. Each request
is checked against the pinned schema. Remote content is untrusted
and does not authorize operations. The sandbox denies networking, clears ambient
credentials/environment, and supplies a private scratch `HOME`, temporary and
cache directories. It limits child resources and owns process-tree cleanup.

`max_wall_seconds` bounds the endpoint lifetime, not each individual call;
transport and service deadlines also apply. Errors or cancellation can leave an
uncertain call result. There are no automatic retries or replacement launches;
start a new finite-lived harness to reset a failed or expired endpoint. Discovery
and replies are checked at bounded points, not authenticated producer identity or
atomic proof of when remote bytes were generated. This describes the native
interface, not proof that an external coding client uses it.

## Optional Python test report

Add `test_report` inside the complete harness policy to expose the native
[Python test import report](native-analysis.md#python-test-import-report).
Replace the paths and digest below with independently measured values. Include
the executable and every required runtime read path in the enclosing
`read_only` grants as well; this fragment is not a standalone policy.

```json
"test_report": {
  "executable": {"path": "/absolute/tools/forge", "sha256": "0000000000000000000000000000000000000000000000000000000000000000"},
  "read_only": ["/absolute/tools/forge", "/absolute/required/runtime/file"],
  "resources": {"cpu_millis": 500, "memory_mib": 512, "processes": 16},
  "max_wall_seconds": 20,
  "max_output_bytes": 262144
}
```

The enabled `python_test_report` tool requires `max_output_bytes`, from 512
through the configured maximum (at most 262144). It uses the exact policy
`source_root`; callers cannot select a root, executable, arguments, environment
or artifact path. The result is the same advisory envelope as the native CLI,
without artifact publication or test execution. Only one report runs at a time.
Reserve its CPU, memory and process limits in the aggregate policy in addition
to session and supervisor reservations.

Runtime grants retain strict ownership and ancestor checks. The final source
and in-root `.git` directories may be group-writable repository data; source
ancestors remain strict, and held directory identities are rechecked. Directory
grants are capabilities, not recursive content pins. Source observations remain
cooperative checks, not an atomic snapshot or hostile replace-and-restore proof.

Supply a trusted Forge executable: the digest binds the selected bytes and does
not attest their producer. The service invokes that executable with fixed worker
arguments inside the existing network-denying sandbox. The shipped worker uses
a cleared environment, private HOME/cache/temp and empty Git search directories.
The sandbox bounds its initial Git metadata/configuration reads; subsequent
semantic checks reject linked/external Git directories and unsupported Git
configuration. The worker does not launch Git or Python. Its child wall deadline
is 1–20 seconds. Pre/post filesystem capture and verification are cooperative
and can delay cancellation or shutdown; this is not a hard whole-call deadline.
Cancellation requests child termination and waits for owned cleanup. Uncertain
cleanup fails closed and retains scratch for inspection.

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
- `python_test_report` when `test_report` is configured, as described above.

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

For `python_test_report`, MCP cancellation closes the forwarded connection;
the service requests cancellation and awaits cleanup. That report connection
is one-shot: closing its write half or sending a pipelined frame requests
cancellation. Other calls retain their existing semantics: RPC cancellation
suppresses the response and cannot undo work already handed to the daemon;
use `terminal_close` to stop an owned terminal.
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
make build
FORGE_TEST_BIN="$PWD/native/forge/target/release/forge" \
  cargo +1.98.0 test --locked --manifest-path native/forge/Cargo.toml \
  --test native_harness --test managed_service -- --include-ignored --test-threads=1
```

They exercise actual manager cleanup, daemon crash, detached children, API
reconnects, idempotent writes, policy/schema rejection, transactional patches,
concurrent clients and durable shutdown. Test-only fixtures use no interpreter.
