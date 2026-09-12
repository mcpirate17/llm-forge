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

## Bootstrap a host project

Once `conductor-tooling` is installed into a host project's venv, one command scaffolds
everything that project needs to adopt the platform:

```sh
python -m conductor.bootstrap /path/to/host-project
# equivalently: python -m conductor bootstrap /path/to/host-project
```

It writes, idempotently and fail-loud (`--dry-run` to preview, `--check` for CI drift
detection): the dispatcher `hooks` block merged into `.claude/settings.json`, a
`.claude/hooks/dispatch.py` launcher pinned to the interpreter this package is installed
into (no dependency on an in-tree `tooling/` checkout), the code-review-graph server in
`.mcp.json`, a starter `conductor/candidate_policy.toml` and mutation registry, a
`.github/workflows/weekly-audit.yml` running this package's own audits, and a
`.claude/hooks/project/env.sh` stub for repo-specific hook defaults. A settings or MCP
key the host already has is kept; one wired differently is a conflict, refused with a
diff unless `--force`. Project-owned files (policy, preauthorizations, registry,
workflow, env.sh) are created once and never rewritten. Separately, it warns (never
writes) if the host's own `pyproject.toml` is missing the `[tool.conductor]` keys
(`candidate_policy`, `mutation_registry`, `package_root`) that `conductor.project_paths`
needs to find the host's own layout instead of assuming this repo's.

`conductor.bootstrap` is an alias onto `conductor init` (`conductor/project_init.py`),
not a separate implementation — see that module for the full contract.

## History

This repository was split out of the `LLM` monorepo on 2026-09-12 with
`git filter-repo`, keeping the commit history of `conductor/` and `tooling/`. The
monorepo's project data (its mutation campaigns, candidate policy, preauthorizations
and baselines) stayed behind; this repo carries its own dogfood campaigns and policy.
