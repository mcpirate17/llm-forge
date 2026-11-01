# llm-forge

A native local-LLM tool runtime for source inspection, deterministic edits,
confined execution, code retrieval and bounded context. The `forge` executable
and its default library dependencies are Rust, with native C/C++ dependencies;
the default build does not discover or link a Python interpreter and requires
no Node or Electron runtime.

The repository is undergoing a native migration. The runtime described below
works from the current source tree. Remaining interpreted governance/analysis
adapters and optional bindings are recorded in [ARCHITECTURE.md](ARCHITECTURE.md);
their presence means the repository-wide migration is not yet complete.

## Build and install

On Linux, install Rust 1.98.0 and the native compiler/linker prerequisites, then:

```sh
make build
make install
export PATH="$PWD/target/install/bin:$PATH"
forge --help
```

`make install INSTALL_ROOT=/absolute/prefix` installs `bin/forge` under another
explicit prefix. The default prefix is local to this checkout and does not
modify a shared Python environment. SQLite is bundled. Installed runtime
operations do not need Cargo or Python; commands that build or profile user
code still require their explicitly selected external tools. See
[installation](docs/install.md) for provider setup and verification.

The three native crates are:

- `native/forge`: the executable, tool harness, Linux process isolation,
  provider hooks, lifecycle, messaging and task coordination.
- `native/conductor-native`: shared syntax, search, planning, configuration and
  evidence algorithms. Default features enable native source and vector search.
- `native/slop-core`: native source checks and checked ablation proposals.

## Tool surfaces

| Surface | Native implementation and guide |
| --- | --- |
| Structured calls | Compiled closed-schema validation, bounded repair, GBNF and tool-signature validation; [structured output](docs/structured-output.md) |
| Source intelligence | Tree-sitter definitions, signatures, references, scopes and imports for seven languages; [code tools](docs/native-code-tools.md) |
| Editing | Deterministic replacement/unified diff, unique fuzzy matching, syntax validation, dry runs and hash-bound Git undo; [code tools](docs/native-code-tools.md) |
| Retrieval | SQLite FTS5/BM25, exact or HNSW vector search, immutable mapped indexes, source freshness and optional pinned local embedding endpoints; [code search](docs/native-code-search.md) |
| Execution | Persistent PTYs, readiness-driven I/O, pidfds, bounded output and Linux Landlock/seccomp/cgroup isolation; [execution](docs/native-execution.md) |
| Context | Offline BPE tokenization, protected message groups, AST folding, diagnostic projection and durable recovery before compaction; [context](docs/native-context.md) |
| Orchestration | Persistent local service, capability policy, request/output limits, receipts and MCP stdio; [harness](docs/native-harness.md) |
| Source quality | Native Rust/Python source inspection and read-only checked ablation proposals; [analysis](docs/native-analysis.md) |

Linux isolation requires the documented kernel and delegated-resource features;
unsupported isolation fails explicitly. Embedding generation remains an optional
configured service. There is no embedded GPU tensor workload or claim of GPU
acceleration. Benchmarks distinguish library dispatch, service calls, process
startup and full retrieval costs; see [performance](docs/performance.md) and the
individual surface guides.

## Provider and host setup

Initialize host settings and preview provider changes before applying them:

```sh
forge project --host /path/to/host init
forge project --host /path/to/host init --apply
forge providers --host /path/to/host --provider claude install
forge providers --host /path/to/host --provider claude install --apply
forge providers --host /path/to/host --provider claude check
```

Claude and Codex use the native lifecycle dispatcher. Qwen and Grok support the
bounded mailbox startup integration. `--replace-legacy` explicitly migrates
recognized Forge Python entries; other commands are preserved. Installation
records a durable, drift-checked rollback snapshot. It does not start a service
or send peer messages. See [provider contracts](docs/native-providers.md).

Host path settings and optional SessionStart text can live in `forge.toml`.
The shared native reader accepts existing `pyproject.toml` host settings only
when that native manifest is absent; it never executes an interpreter. See
[native project configuration](docs/native-project-config.md).

The harness uses an operator-selected capability policy and socket. Follow
[its setup guide](docs/native-harness.md) to start the service, then use
[`forge mcp bind`](docs/native-mcp-setup.md) to plan and install a connection to
that exact host and socket. Filesystem roots, command capabilities and embedding
endpoints are
explicit configuration, not inferred grants from model output.

## Verification and development

```sh
make test
make local-preview
# On a clean committed checkout:
make local-check
make local-verify
```

The default tests use native release builds. Local verification installs an
isolated artifact and runs formatting, strict Clippy, native tests and runtime
smoke tests. A preview is advisory and cannot produce a publication receipt.
The committed gate binds HEAD, the target branch's policy and check logs; see
[local checks](docs/local-checks.md). Policy changes do not install or start a
scheduled sweep. Optional binding contracts remain outside the native gate while
those adapters are retired.

[ARCHITECTURE.md](ARCHITECTURE.md) records milestones, measurements, limits and
remaining migration work. [Messaging](docs/messaging.md),
[coordination](docs/coordination.md), [lifecycle](docs/native-lifecycle.md) and
[graph context](docs/graph-context.md) describe the existing native operational
interfaces. `AGENTS.md` is the repository working contract.

This repository was split from the `LLM` monorepo on 2026-09-12, retaining the
history of its conductor and tooling components.
