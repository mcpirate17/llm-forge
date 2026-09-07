# conductor-tooling

The agent governance tooling of the LLM workspace, on its way to a standalone package:
`conductor/` (mutation campaigns and receipts, candidate review and the gate, A2A,
knowledge and memory retrieval, session hygiene), the generic agent hooks, and the
`conductor_native` Rust crate behind `conductor/_native.py`.

This directory holds the future package's manifest. The code has not moved: receipts
bind to literal manifest paths, so `conductor/` stays where it is until split time,
when it relocates to `src/conductor`. What is enforced now, every PR, is the boundary
(`conductor/tooling_boundary.py`, an always-on gate check): no import edge from the
tooling to the host packages, no host path literal, one native seam.

**Rehearsal.** `make tooling-standalone-smoke` runs the Rust harness in
`tooling/native/tooling-standalone-smoke`. It assembles this package from the committed
tree in a scratch dir (`src/conductor`, `src/tooling/hooks`, `native/conductor-native`,
`native/slop-core`, `hooks/` without the project extension, this manifest), installs it
with `uv` into a fresh venv (building the crates), proves the host packages are
unimportable there, and runs `pytest src/conductor src/tooling` with
`CONDUCTOR_PROJECT_TEST_PLUGIN=""`. Its failures are the hidden couplings; the report
(`research/reports/tooling_standalone_smoke.json`) groups them by first line. Then the
foreign half: `uv build --wheel`, a second fresh venv with only the crates and that
wheel, `python -m conductor init` on a throwaway `git init` repository (the hook doctor
runs inside it), a force-push denied through the scaffolded `.claude/hooks/dispatch.py`
with a clean environment, and `tooling.hooks.dispatch` / `conductor.project_init` proven
to import from that venv alone. Any infrastructure failure aborts with a refusal report.

## Scaffolding a project

```
uv pip install <wheel> [conductor-tooling[graph]]   # graph: the code-review-graph MCP server
python -m conductor init <project-dir> [--python PY] [--force] [--dry-run | --check]
```

Writes, idempotently: the dispatcher `hooks` block in `.claude/settings.json` (merged by
event; a differently wired event is refused unless `--force`, every other key is kept),
`.claude/hooks/dispatch.py` (shebang = the interpreter carrying the tooling), the
`code-review-graph` server in `.mcp.json` (merged by server key), a minimal
`conductor/candidate_policy.toml`, the `conductor/preauthorizations.md` ledger, the
mutation-campaign registry and directories, and a marked `.gitignore` block. Files the
project owns (policy, ledger, registry) are created once and never rewritten.
`--dry-run` prints the unified diff; `--check` exits 1 when a run would write or the
doctor finds a dead hook; a run that writes ends with the doctor and fails loud on a
dead hook.

## Host seams

Two environment variables carry what only a project can know. Both default to the host
repository's layout, so nothing changes for this tree; a standalone install overrides or
ignores them.

| Variable | Meaning |
|---|---|
| `CONDUCTOR_PROJECT_TEST_PLUGIN=""` | This install has no host project behind it. The suite's host-coupled tests skip; `conductor/_project_hooks.py` defines the signal and `conductor/conftest.py` holds the inventory of what it excuses. |
| `CONDUCTOR_VULTURE_WHITELIST` | The project's vulture whitelist, default `research/tools/vulture_whitelist.py`. A whitelist names the symbols a *project* intentionally keeps alive, so conductor cannot own the list. When the configured path is absent the argument is dropped rather than passed: naming a missing file makes vulture exit 1 with no findings, and every finding it would have reported is then untrusted. Dropping it is not a silent weakening — an unfiltered run reports strictly more, never less. |

## Last rehearsal

2026-09-07, tree `c78b238e47d5` (branch `llm-31/standalone-smoke-20260907`), install
31.0 s, pytest: **1858 passed, 12 skipped, 1 failed**, exit 1. Foreign half: wheel
install 27.6 s, `hook-doctor | PASS dead=0 warn=0 total=4`, force-push **deny**, both
modules import from `.venv-foreign/lib/python3.12/site-packages`.

The single failure is not a host coupling:
`test_mutation_coverage.py::test_mutation_testing_cli_inspect_verify_and_refuse`
(`assert 3 == 0`) is `mutation_testing inspect` refusing on `source_hash_drift` against
`conductor/mutation_scope.py`. It fails identically inside this repository and on
master, and is one of the two contract tests PR #360 repairs.

The 12 skips are the host-coupling surface that remains, and they are an inventory
rather than a scattering of decorators: eight in `conductor/conftest.py`, one in
`tooling/hooks/dispatch/conftest.py`, one module-level `importorskip("torch")` in
`test_equivalence_probe.py`, and two that predate this work. Each entry names the host
artifact it needs — `.github/CODEOWNERS`, a crate under `tooling/native/`, an
`aria_core` source, a `research/`-scoped fixture campaign, host-tree scale thresholds,
the launcher `conductor init` writes. **That list is debt, not architecture**; the goal
is for it to reach zero, and shrinking it is what a later rehearsal should show.

For comparison, the 2026-09-03 rehearsal on `tooling-conductor-init` (tree `35b584d52dd6`)
was **1280 passed, 48 failed, 9 errors, 1 skipped**. The errors and 47 of the failures
were host couplings in the package itself: host path literals in `run_audit` and
`_vulture_issues`, `.agent_hooks` derived from the package parent, runner components
resolved out of the monorepo, and a `research/tools/vault_health.py` path standing in
for a synthesized fixture.
