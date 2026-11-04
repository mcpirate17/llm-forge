# Forge integration and positioning research

Accessed **2026-11-02**. This is bounded primary-source desk research plus current
repository inspection, not customer validation, market-share research, a
competitor benchmark or a recommendation to install/change anything. Product
positioning below is an inference unless a row explicitly reports documentation.

## Recommended positioning

**Forge gives your existing coding agent bounded local tools for understanding
code, editing exact files, managing commands and controlling context.**

Primary audience: developers and small teams running coding agents against Linux
workspaces, especially those needing consistent tooling across clients and
reviewable execution. Adjacent audience: agent-runtime builders who want native
libraries and an MCP/CLI boundary rather than another conversational coding UI.
This is not yet a one-click general-consumer app or a complete cloud-agent
platform. The current setup cost should be visible before people commit to it.

Lead with a user job and a demonstrable result: “inspect the relevant code before
loading whole files,” “continue a test process after reconnect,” or “prepare a
hash-checked edit and review the plan.” Treat “implemented in Rust” as a deployment
and engineering property, not the headline proof of user benefit.

## MCP, hooks or CLI?

**Assessment: use a hybrid.** MCP is the main explicit tool interface; small native
hooks automate essential checks/briefs/telemetry; the CLI remains valuable for
manual trials, scripts and clients without a configured MCP connection. This is
an architectural recommendation, not a measured latency or token-saving result.

| Route | Best job | Cost/limitation to disclose |
| --- | --- | --- |
| Persistent native harness + MCP adapter | Agent-selected inspection/search/edit/context operations and stateful terminals | Operator must start a service and configure the client; discovery, transport, policy and service startup have costs. The agent may not select the tool. |
| Small automatic hooks | Policy checks and brief context at known lifecycle/tool events, plus telemetry | Runs on the event path; large automatic context and repeated heavy scans can add latency/tokens even if the agent needs none of that material. Support/failure behavior differs by client. |
| Direct native CLI | Fast trial, shell automation, troubleshooting and explicit operator workflows | A new process may reload state; the agent must know the command. No automatic client-tool integration follows from installation. |

MCP specifies discoverable/callable tools and does not mandate a particular user
interaction pattern. Its normal tool model allows the model to discover and
invoke them. The stdio binding starts a client-launched subprocess; the protocol
alone does not guarantee a long-lived backend. Forge's separately owned harness,
not “MCP” by itself, provides retained sessions across adapter disconnects.
[Official MCP tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools),
[official stdio binding](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio).

Official Claude Code docs describe automatic lifecycle/tool hooks and show that
pre-tool checks and post-change notifications have different decision powers.
They also document MCP discovery/deferred tool search, with client/model/platform
fallbacks. Recommendation: give Forge a small, descriptive tool catalog and clear
server instructions; return bounded results on demand rather than rely on large
startup dumps. Deferred discovery is not evidence that any specific Forge
catalog has already lowered a user's billed context.
[Claude hooks](https://code.claude.com/docs/en/hooks),
[Claude MCP/tool search](https://code.claude.com/docs/en/mcp#scale-with-mcp-tool-search).

Official Codex docs support local stdio MCP connections in `config.toml`, including
trusted project configuration. Its hook system separately supports trusted
command and MCP-tool handlers. It warns that accumulated hook/plugin context can
harm model performance, and handles oversized output by spilling/previewing.
Recommendation: keep automatic hook content small, measure it, and use explicit
retrieval for details. No universal optimal token cap follows from that guidance.
[Codex MCP](https://learn.chatgpt.com/docs/extend/mcp#connect-codex-to-an-mcp-server),
[Codex hooks](https://learn.chatgpt.com/docs/hooks#large-hook-output).

Hooks and MCP are not mutually exclusive transports: both clients now document
MCP-tool hook handlers. That does not make Forge's existing command hooks a
ready-made replacement service. Codex cloud orchestration also has different
managed-hook support from local execution, and callback failure need not block a
tool. Do not promise client-wide enforcement without a tested allowed case,
denied case and failure case on the actual execution surface.
[Codex managed-hook scope](https://learn.chatgpt.com/docs/hooks#managed-hooks-from-requirementstoml).

## Nearby products: evidence and implications

| Primary-source observation | Positioning inference for Forge |
| --- | --- |
| Aider describes a repository map of key symbols/signatures and selects relevant portions within a token budget. [Aider repo map](https://aider.chat/docs/repomap.html) | Code-context reduction is an established job, not a unique claim. Differentiate Forge through callable cross-client tools, source identity, deterministic edit controls and explicit recovery; do not imply Aider lacks them without a fuller comparison. |
| Context7 positions itself around current, version-specific library documentation delivered to coding agents. Its setup page emphasizes client-specific connection choices and checking that the connection is available. [Context7 overview](https://context7.com/docs/overview), [installation](https://context7.com/docs/installation) | A narrow job and obvious first successful call are easier to understand than a long infrastructure inventory. Forge is chiefly about the selected local workspace/runtime, not a replacement for an external documentation service. |
| Claude and Codex already expose tool integration and automatic extension points through their own clients. [Claude MCP](https://code.claude.com/docs/en/mcp), [Codex MCP](https://learn.chatgpt.com/docs/extend/mcp) | Sell the useful local backend and consistent contracts, not the mere existence of an MCP connection. Demonstrate why the agent would choose Forge over its existing file/shell tools. |

These sources establish documented offerings, not their measured effectiveness,
user satisfaction, adoption counts or Forge superiority. Vendor quality claims
are not independent evidence. Broader audience/installation observations are in
[audience research](audience-research.md).

## What a non-specialist should know before configuring Forge

1. **Keep your existing agent.** Forge supplies tools; you still choose the client
   and model. Some features need a tokenizer or an optional embedding service.
2. **Choose the workspace and permissions.** Specify what can be read, which exact
   paths can change and which programs can run. A tool request cannot enlarge
   that operator policy. Start with a read-only source inspection.
3. **Understand the connection.** A service owns the work; an MCP adapter connects
   the client to it. Hooks are separately installed automatic actions. For Forge,
   copying a Claude `.mcp.json` entry does not configure Codex's TOML settings.
4. **Understand where data goes.** Local parsing/execution does not imply offline
   model use. Tool results may be sent by the agent client to its remote model;
   configured embedding endpoints are another data path.
5. **Confirm a real call.** Inspect the client-visible tools, ask for one specific
   read, verify its source hash/status, then try a dry-run edit before granting
   execution. Catalog presence is not proof that the agent actually uses it.
6. **Know the operating constraints.** The strict harness requires supported Linux
   kernel/systemd controls and fails if unavailable. Other tools may work through
   the CLI without starting that harness. Avoid implying all machines/clients
   have equal feature coverage.
7. **Evaluate value on your own work.** Compare complete tasks, actual usage and
   verified outputs. A smaller projected prompt can omit useful material; a fast
   microbenchmark can be irrelevant to the task's slowest step.

These are onboarding recommendations derived from the documented client setup
and Forge's current contracts, not results of a tested layperson onboarding study.

## Current Forge support versus remaining product gaps

Repository evidence: `harness_mcp.rs` forwards to an already authorized service;
`harness_mcp_protocol.rs` advertises the actual catalog and concise server
instructions. `harness_catalog.rs` selects tools from operator capabilities.
`mcp_setup.rs` manages one project `.mcp.json` format with root/socket checks;
`hook_protocol.rs` projects native handler results into Claude/Codex wire shapes.
Provider configuration contracts are in [native providers](native-providers.md).

| Area | Current support | Credible next step, not a completed claim |
| --- | --- | --- |
| First useful experience | CLI source inspection works without a model service | A copyable tiny repository example; a ready-made read-only MCP profile and one readiness/doctor flow are proposed onboarding work, not an existing one-click setup |
| Persistent agent access | Native finite-lifetime service + stdio adapter + verified catalog; socket creation/binding is explicit | Per-client setup/status/undo recipes and easier service lifecycle handling, tested end-to-end with a real client connection |
| Automatic checks | Native command hooks, bounded output and protocol adapters | Measure event overhead and actual injected tokens; avoid duplicate hooks and unnecessary autoinjection |
| Context value | Exact supplied-text counts and one documented projection fixture | Real-task comparison with recovery/reread accounting and provider usage |
| Discovery | Source-defined tool descriptions and MCP instructions | Observe actual tool selection; tune names/examples before increasing catalog visibility |
| Runtime prerequisites | Explicit enforcement preflight and documented Linux requirements | A plain-language readiness checklist and actionable unsupported-platform path |
| Performance | Scoped component/retrieval evidence and a matched Python/native relocation slowdown | Profile the slowdown and test any optimization; add persistent/CLI controls and report safety-scope differences |
| Migration | Native core plus remaining interpreted host boundaries | Keep graph schema-9 cutover and reuse drafts out of shipped marketing promises |

No messages were posted, products installed, accounts connected, settings changed
or services started for this research. All external factual statements above are
supported by opened primary documentation; recommendations are explicitly
inferences and need workflow validation.
