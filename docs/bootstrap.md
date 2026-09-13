# Bootstrapping a host project

`python -m conductor bootstrap <project-dir>` — also reachable as
`python -m conductor init <project-dir>`; `conductor.bootstrap` is a thin alias
onto `conductor.project_init` — scaffolds the platform into a foreign checkout.
Once `conductor-tooling` is installed into the host project's venv (pinned by
tag under `[tool.uv.sources]`, see the top-level README), one command writes
everything the governance and efficiency tooling needs:

- the dispatcher `hooks` block merged into `.claude/settings.json`,
- a `.claude/hooks/dispatch.py` launcher pinned to the interpreter this package
  is installed into (no dependency on an in-tree `tooling/` checkout),
- the code-review-graph server in `.mcp.json`,
- a minimal `conductor/candidate_policy.toml`,
- the `conductor/preauthorizations.md` skeleton,
- the mutation-campaign directories and registry,
- a `.gitignore` block,
- a `.github/workflows/weekly-audit.yml` running the platform's own audits,
- a `.claude/hooks/project/env.sh` stub the host fills in with repo-specific
  hook defaults (see `AGENTS.md`, "Hook extension convention").

## Options

| Flag | Behavior |
|---|---|
| `--python PYTHON` | Interpreter the launcher pins itself to |
| `--force` | Overwrite a settings/MCP key that is wired differently instead of refusing |
| `--dry-run` | Print the unified diff of every write, write nothing |
| `--check` | CI drift detection: exit 1 when a run would write anything or the hook doctor finds a dead hook |

## Merge semantics

Merging is by key, never by file: a settings or MCP file keeps every key it
already has, an event or server that is already wired identically is left
alone, and one that is wired *differently* is a conflict — refused with the
file and key named unless `--force`. Project-owned files (the policy, the
preauthorization ledger, the campaign registry, the workflow template, the
`env.sh` stub) are created once and never rewritten.

A run that writes ends with the hook doctor and fails loud when any declared
hook is dead.

## The `[tool.conductor]` warning

Separately, and never blocking: a host whose `pyproject.toml`
`[tool.conductor]` table is absent or missing `candidate_policy`,
`mutation_registry` or `package_root` gets a warning, never a write — those
three keys are how `conductor.project_paths` tells an installed-wheel host from
an in-tree checkout, and the monorepo-shaped defaults it falls back to are
wrong for anything else. A host may also set `notes_root` there to point the
knowledge-card retriever at its own notes tree (default `research/notes`).
