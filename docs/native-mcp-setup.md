# Native MCP project setup

Start the harness with an explicit operator policy as described in
[native-harness.md](native-harness.md). The start result contains its socket.
Run setup using the installed `forge` binary you want the client to launch:

```sh
forge mcp --host /absolute/project bind --socket /returned/state/socket
forge mcp --host /absolute/project bind --socket /returned/state/socket --apply
forge mcp --host /absolute/project check --socket /returned/state/socket
forge mcp --host /absolute/project unbind --apply
forge mcp --host /absolute/project rollback --apply
```

Bind, unbind and rollback are read-only plans unless `--apply` is present.
Binding writes the `forge` entry in the host's `.mcp.json`, preserving unrelated
servers and settings. This is the project configuration format supported by
[Claude Code](https://code.claude.com/docs/en/mcp#project-scope); it does not claim
to configure other clients' distinct settings formats. Those clients can use
the emitted `desired_server` command and argument array in their own configuration.
No client approvals or managed settings are changed by Forge.

The command is the absolute current native executable, with arguments:

```text
harness stdio --socket /returned/state/socket --source-root /absolute/project
```

Bind checks an owned, private, singly linked Unix socket in a private parent
directory, verifies same-UID peer credentials, and compares the service's
canonical source root to the host. Status and catalog probes share a three-second
deadline. An incompatible protocol, stopping service, malformed tool schema or
changed socket prevents publication. Apply probes again while holding the
configuration lock. The adapter checks the source root again before exposing
tools when the client launches it. Paths containing `${` are rejected because
clients can expand those strings as environment references.

The proof is a live local binding, not process attestation. Same-UID unconfined
programs remain trusted. The service's policy and lifetime are unchanged. Setup
does not start a service, add execution capabilities, authorize new paths, or
keep a stopped service alive. After restarting the finite-lived service, bind
its newly returned socket explicitly. An old binding fails instead of silently
launching a replacement. Closing the MCP adapter leaves the authorized service
and its terminal sessions under their existing owner.

Configuration input is bounded to 1 MiB, with duplicate JSON keys rejected.
The owned host directory and files must not be writable by other groups/users;
symlinks, hard links and special files are rejected. New configurations are mode
0600; existing modes and unrelated values are preserved. `.forge-mcp.lock`
serializes cooperating writers. A private `.mcp.json.forge-native.bak.json`
records exact before/after bytes and modes before fsynced atomic publication.
Rollback refuses drift and restores the last operation, including prior absence.
Backup and target publication are separate durable operations, as documented
for [project initialization](native-project-config.md).

`--replace-legacy` removes only the exact historical `code-review-graph` entry
using Python `-m conductor.crg_server --repo HOST`, its matching `cwd`, stdio
type and `CRG_ROLE=review` environment. Modified entries are preserved. A custom
entry named `forge` causes a conflict instead of being overwritten. Plans expose
hashes, byte lengths, the desired command and live tool names, not unrelated
configuration values or credentials.

## Codex CLI

Codex uses TOML MCP configuration rather than Claude's project `.mcp.json`.
For an already-running service, use the emitted absolute command and arguments:

```toml
[mcp_servers.forge]
command = "/absolute/path/to/forge"
args = ["harness", "stdio", "--socket", "/returned/state/socket", "--source-root", "/absolute/project"]
required = true
enabled_tools = ["code_inspect", "code_fold"]
```

Tool exposure and client approval are separate decisions. Codex's documented
server-wide and per-tool approval settings remain operator-controlled; Forge
does not change them. MCP annotations describe behavior and confer no authority.
See the [official Codex MCP configuration reference](https://developers.openai.com/codex/mcp).

Forge marks `code_inspect` and `code_fold` read-only, non-destructive, idempotent
and limited to the local workspace. Idempotence describes their effects; source
bytes and parse results can still change. Other tools, including the generic
outbound capability, do not inherit those hints. The native catalog validator
accepts only the four optional boolean MCP hints and rejects unsupported or
malformed annotation fields. Hints do not alter argument validation or grants.

A bounded Codex CLI 0.166.0 diagnostic discovered and attempted Forge calls but
refused them under its effective approval policy. Its zero process exit code
did not mean successful tool execution. Validate actual tool events and results,
not just client startup, discovery or process status. The
[recorded observation](performance.md#actual-codex-approval-diagnostic--2026-11-05)
does not establish successful client adoption. Do not weaken a client policy to
turn a refused probe into a passing result.

## Operator-configured outbound tool

The optional outbound capability is configured on the **harness policy**, not
by adding an endpoint to a coding client's MCP configuration. See [the outbound
policy and isolation contract](native-harness.md#optional-outbound-mcp-tool).
When configured, the existing Forge adapter advertises its single public tool;
its remote output remains untrusted. `bind` does not create this capability or
prove that a client will select or use it.

## Hook interaction

When [native provider hooks](native-providers.md) are installed, only
`mcp__forge__code_inspect`, `code_fold` and `code_search` count as inspection calls.
`code_replace`, `code_diff` and `code_restore` use the existing edit/claim guard
and `.current_work.md` access guard, including their preview calls. Host claim
policy and harness path capabilities are independent checks.

The inspection marker is written at PreToolUse, matching the existing hook
contract; it records an attempted inspection, not successful inspection evidence
or approval. Terminal, context and edit calls cannot create this marker. Applied
native edits can queue graph refresh; previews and reported failures do not.
Native transactional edits are not passed to a second formatter that would
invalidate their returned source hashes.
