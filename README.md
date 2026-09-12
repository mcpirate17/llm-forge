# llm-forge

A development environment for building reliable LLM systems: agent coordination,
context and token efficiency, code quality, auditing, testing, and automated
governance.

The Python package is `conductor` (distribution `conductor-tooling`), backed by two
Rust crates under `native/`:

- `src/conductor/` — mutation campaigns and receipts, candidate review and the gate,
  agent-to-agent messaging, knowledge and memory retrieval, session hygiene, worktree
  and sandbox management.
- `src/tooling/hooks/` — the generic agent hooks (Claude, Codex, dispatch adapters) a
  host project wires into its own `.claude/hooks/` or `.agent_hooks/`.
- `native/conductor-native/` — the `conductor_native` PyO3 extension behind
  `conductor/_native.py`.
- `native/slop-core/` — the slop scanner.

## Install

```sh
uv venv .venv --python 3.12
uv sync --extra test
.venv/bin/python -m pytest src/conductor -q
```

Every host project pins this repo by tag in its own `pyproject.toml`:

```toml
[tool.uv.sources]
conductor-tooling = { git = "https://github.com/mcpirate17/llm-forge", tag = "v0.1.0" }
```

## Documentation

[`docs/`](docs/README.md) has one page per platform law (governance claims, the landing
gate, risk/approval tiers, mutation evidence, context budget, CI coverage), written for a
host project adopting this platform. `AGENTS.md` is the working contract for changes to
this repository itself.

## History

This repository was split out of the `LLM` monorepo on 2026-09-12 with
`git filter-repo`, keeping the commit history of `conductor/` and `tooling/`. The
monorepo's project data (its mutation campaigns, candidate policy, preauthorizations
and baselines) stayed behind; this repo carries its own dogfood campaigns and policy.
