# llm-forge

**Local tools for coding agents: find the relevant code, prepare verifiable edits,
keep commands running, and control what goes back into the model's context.**

Forge works alongside your coding assistant. It helps the assistant fetch relevant
code, check edits against current files, and keep commands running between
messages. You choose which files it may read or change and which programs it may
run. Bring your own coding client and model.

## What you can do

| When your agent needs to… | Forge provides… |
| --- | --- |
| Understand a file before reading all of it | Definitions, signatures, references and folded bodies, with source hashes and bounded output |
| Find related code | A structural caller/import graph, lexical search and optional vector search with explicit source freshness |
| Change an exact file safely | Hash-checked replacements or diffs, syntax validation, dry-run preparation and guarded Git recovery |
| Work with a compiler or test process over several turns | Persistent terminal sessions, incremental output, reconnect/retry identifiers and resource controls |
| Fit a long conversation into a context budget | Offline tokenizer counts, protected message groups, deterministic reductions and recoverable originals |
| Check a task brief or compare a proposed rewrite | [Source-backed checklists and literal retention audits](docs/native-context.md#prompt-contracts-and-literal-retention), with explicit limits on semantic conclusions |
| Review what actually ran | Durable task records, scoped check results and source-bound receipts; missing evidence stays visible |

The benefit comes from using these tools in the workflow. Installing Forge does
not automatically shorten prompts, redirect an agent's existing tools, improve
its reasoning or lower its bill.

Use it when you work with an agent on a Linux project and spend time feeding it
files, recovering command output, or checking whether its edits still match the
source. For a small project where ordinary file and shell tools work well, Forge
may add more setup than value. It is a tool backend, not a replacement chat app.

## Try source inspection first

On Linux with Rust 1.98.0 and native compiler/linker prerequisites:

```sh
make install
export PATH="$PWD/target/install/bin:$PATH"

forge code --root "$PWD" context --symbol relocate_console_scripts \
  --no-graph --json -- native/forge/src/worktree_runtime.rs
```

This returns JSON containing the function signature, imports, source hash and
`graph_status: "disabled"`. The function body is represented by `{ ... }`:
enough to check its parameters without reading its implementation. The command
does not edit the file, start a service or call a model. This is also the source
used in the signature-token measurement below. The default installation stays inside
this checkout's `target/install`. Use `INSTALL_ROOT=/absolute/prefix` to choose
another location. The executable's default build requires no Python, Node or
Electron runtime; SQLite is bundled. See [installation](docs/install.md).

For caller/callee context, build the native graph, then ask about a file:

```sh
forge graph --host /path/to/project index
forge graph --host /path/to/project context src/module.rs --max-bytes 8192
```

Indexing writes `.forge/graph.db`. Queries report stale, missing or uncertain
relationships rather than presenting them as current facts. Read the
[graph contract](docs/graph-context.md) before relying on dependency coverage.

## Connect it to your agent

Use **MCP for tools the agent chooses**, **small hooks for automatic checks and
brief context**, and **the CLI for direct use and scripts**. They serve different
parts of the workflow; none should inject the entire repository on every turn.

For example, an agent requests a function signature through MCP when it needs
one; a hook checks an edit request when the client emits that event; you run the
CLI to inspect a file yourself. MCP does not make an agent select Forge's tools,
and its transport does not itself keep a backend running.

The persistent harness is the MCP backend. First define the files and commands Forge may access using
the [harness guide](docs/native-harness.md): select the source root, writable
paths, allowed programs and resource limits. Then:

```sh
forge harness preflight --policy /absolute/operator.json \
  --state-parent /existing/private/state
forge harness start --policy /absolute/operator.json --state-parent /existing/private/state
forge harness catalog --socket /returned/state/socket
```

The optional [outbound MCP capability](docs/native-harness.md#optional-outbound-mcp-tool)
adds one operator-pinned local stdio tool to this same catalog, with bounded
execution and schema validation.

The returned socket belongs to that running service. An MCP client launches the
absolute installed `forge` binary with these arguments:

```text
harness stdio --socket /returned/state/socket --source-root /absolute/project
```

[Native MCP setup](docs/native-mcp-setup.md) can preview and apply a project
`.mcp.json` entry. Other clients use their own configuration formats; copying a
Claude project file does not configure Codex. Hook installation is a separate
preview/apply operation through [provider setup](docs/native-providers.md).
Claude and Codex have guard/lifecycle adapters; Qwen and Grok currently have
bounded startup messaging integrations rather than equivalent guard coverage.

The harness requires documented Linux kernel/systemd capabilities. Unsupported
isolation fails explicitly. Start with inspection, add editing or execution only
when needed, and verify the advertised catalog in your actual client. Local
execution does not mean returned code stays out of the client's remote model
requests. Optional embeddings require a separately configured service.

## Are the gains measured?

There are useful measurements, with specific limits:

- A signature lookup on a real Rust file returned **383 tokens instead of 2,455**
  for the full file: 84.4% fewer tokens, including the complete JSON response,
  under a pinned GPT-2 tokenizer. It omits function bodies and does not establish
  task quality or time saved. [Measured benefits](docs/benefits.md)
- The first matched Python/native migration benchmark found a **slowdown**:
  relocating 256 console launchers took a median 321.7 ms natively versus
  Python's 30.0 ms. Both produced the checked outputs; the native path performs
  additional safety work. A native rewrite is not automatically a speedup.
  A follow-up native optimization reduced CPU use by about 30% at that size,
  while wall time changed little. [Matched runs and method](docs/native-product-value-measurements.md)
- One context fixture went from **57,430 input tokens to 3,277 rendered tokens**,
  with a separate 512-token reserve. That is 94.3% less rendered text under its
  pinned tokenizer, after dropping 29 old groups; downstream task quality was
  not measured. [Context evidence](docs/native-context.md#release-measurements)
- A source-bound retrieval fixture completed all 144 requested operations. Warm
  BM25 retrieval had a **0.461 ms median**, including source checks and JSON.
  Its embedding/index preparation took **23.6 minutes**. This historical run is
  not a native-versus-Python comparison. [Retrieval evidence](docs/native-code-retrieval-measurement.md)
- Specific native scan and cache optimizations have before/after component
  measurements. They do not establish a whole-agent speedup.
  [Performance evidence](docs/performance.md)

A general “Rust is X times faster,” prompt-cost saving or task-time saving is not
established. The [benefits and measurements](docs/benefits.md) separate evidence, estimates
that need your workload's inputs, and missing comparisons.

## Choose the next workflow

[Code tools](docs/native-code-tools.md) · [Search](docs/native-code-search.md) ·
[Context budgets](docs/native-context.md) · [Persistent execution](docs/native-execution.md) ·
[Candidate review](docs/native-candidate-review.md) · [Workspace hygiene](docs/native-workspace-hygiene.md) ·
[Checkout preservation](docs/native-checkout-sync.md) ·
[Committed equivalence gate](docs/equivalence-gate.md) · [Evidence backlog](docs/equivalence-backlog.md)

Forge's native runtime is usable while some interpreted maintenance adapters and
host integrations remain in migration. Native graph seeding is still prerequisite
work, not a shipped replacement for external schema-9 graph stores. Local models
are optional clerical helpers, not approval authorities. Review
[ARCHITECTURE.md](ARCHITECTURE.md) for component boundaries and
[product research](docs/product-research.md) for integration and positioning research.

For contributors, `make test` exercises native builds. Before publishing a clean,
committed change, use `make local-check` and `make local-verify` as required by
[local checks](docs/local-checks.md) and `AGENTS.md`. Installation, a passing test
selection and a publication receipt are different things.
