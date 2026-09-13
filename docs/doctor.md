# `conductor doctor --harness`: proving the settings that dominate agent cost

The hook doctor (`tooling.hooks.dispatch.doctor`) proves declared hooks are
alive. The harness doctor proves the *values* around them are the cheap
ones: it reads the project's `.claude/settings.json` (under
`$CLAUDE_PROJECT_DIR`, or `--project-dir`) and the user's
`~/.claude/settings.json` (under `$HOME`, or `--home`) and grades four
items per file, PASS/FAIL each. Settings drift here is silent — nothing
errors, sessions just cost more or run on a model the hooks were never
tested against.

## What it grades

| Check | Wants | Why |
|---|---|---|
| `subagentPromptCacheTtl` | `"1h"` | A five-minute TTL rewarms a ~180K-token prefix at write price for every subagent spawn. |
| `hooks-dispatcher` | every wired event routes through the one dispatcher command | A stray per-hook command under a dispatcher event is a hook the doctor cannot vouch for and the dispatcher cannot bound. |
| `BASH_QUIET_LIMIT_BYTES` | set in the settings `env` block, a positive integer string | The bound `_bash_quiet` enforces exists either way (module default 8000), but policy belongs where everyone reads it, not in a source-file literal. |
| `model` | absent, or a known Claude model id (a `[...]` thinking-budget suffix is fine) | A `glm-*` or other foreign id left there sends every session to a model this platform's hooks were never tested against. |

The two scopes differ on purpose:

- **project file** must exist and must *wire* all four dispatcher events
  (`PreToolUse`, `PostToolUse`, `SessionStart`, `SessionEnd`), each through
  `registry.dispatcher_command(event)`'s command or `forge hook <Event>`.
  A missing project file is itself a FAIL — the dispatcher wiring lives
  there.
- **user file** only has to not bypass: no dispatcher-event hooks declared
  at all is a PASS (wiring lives in project settings). A missing user file
  is a SKIP row, not a failure — the harness default applies.

The known-model set is the fleet this platform is tested against
(`claude-opus-5`, `claude-sonnet-5`, `claude-fable-5-1`,
`claude-haiku-4-5-20251001`, the `opus`/`sonnet`/`haiku`/`fable`/`default`
aliases). Extending it when the fleet changes is the review, not an
inconvenience.

## What `--fix` changes

Exactly the failed items' edits, nothing else:

- `subagentPromptCacheTtl` → `"1h"`
- `hooks-dispatcher` → the hooks block rewritten to
  `registry.settings_block()["hooks"]` (every event through the single
  dispatcher)
- `BASH_QUIET_LIMIT_BYTES` → `env` block gains `"8000"`
- `model` → the key removed, so the harness default applies
- a missing project file → the canonical settings written:
  the dispatcher block, the `env` bound and the `"1h"` TTL

`--fix` then reports the **post-fix** state — the findings it prints are
re-diagnosed after the edits — and is idempotent: a second run finds
nothing to do. It never touches the user file beyond failed items in it.

## Running it

```
uv run python -m conductor.doctor --harness        # diagnose
uv run python -m conductor.doctor --harness --fix  # diagnose, then repair
uv run python -m conductor.doctor --harness --json # {"findings": [...], "fixed": N}
```

Exit codes: `0` all checks pass (or, with `--fix`, all pass after the
edits); `1` any check FAILs; `2` the invocation is wrong (no `--harness`,
no resolvable home) or a settings file exists but is not readable JSON —
malformed JSON fails loud, never silently skipped.
