# Bootstrapping a host project

`python -m conductor bootstrap <project-dir>` — also reachable as
`python -m conductor init <project-dir>`; `conductor.bootstrap` is a thin alias
onto `conductor.project_init` — scaffolds the platform into a foreign checkout.
This page describes the current source checkout. Provider selection and explicit
native interpreter binding require that checkout's Python package and rebuilt
`forge`; the older published `d2d83d6` snapshot shown in the README does not contain
them. Follow the README's local editable install/build instructions until a reviewed
revision containing these features is published. Once installed into the selected
environment, one command writes:

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
| `--python PYTHON` | Interpreter bound into Python launchers, MCP, and native forge hooks |
| `--provider NAME` | Select claude (default), codex, qwen, grok; repeat or use all |
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

## Harness capabilities

Claude receives the complete dispatcher, using native `forge` when available.
Codex receives the existing Codex dispatcher output adapter and bounded A2A startup.
Qwen receives bounded A2A SessionStart; Grok receives it on the first UserPromptSubmit
because Grok discards SessionStart output. These latter two adapters do not install
tool guards or claim dispatcher parity. Existing unrelated hooks are preserved.

`--check` validates every selected provider and the explicitly configured interpreter.
Checks for messaging startup adapters do not execute the hook or start an endpoint.
Use the same `--provider` and `--python` options for initialization and subsequent
checks. The native interpreter order is mutation-snapshot override, configured
`CONDUCTOR_PYTHON`, host `.venv/bin/python`, then `python3` on PATH for older unbound
installations. An explicit binding is never replaced by a different interpreter.

## The `[tool.conductor]` warning

Separately, and never blocking: a host whose `pyproject.toml`
`[tool.conductor]` table is absent or missing `candidate_policy`,
`mutation_registry` or `package_root` gets a warning, never a write — those
three keys are how `conductor.project_paths` tells an installed-wheel host from
an in-tree checkout, and the monorepo-shaped defaults it falls back to are
wrong for anything else. A host may also set `notes_root` there to point the
knowledge-card retriever at its own notes tree (default `research/notes`).
