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

## Last rehearsal

2026-09-03, tree `35b584d52dd6` (branch `tooling-conductor-init`), editable install 20 s,
pytest: **1280 passed, 48 failed, 9 errors, 1 skipped**, exit 1. Foreign half: wheel
install 16.5 s, `hook-doctor | PASS dead=0 warn=0 total=4`, force-push **deny**, both
modules import from `.venv-foreign/lib/python3.12/site-packages`. Two blockers the
rehearsal found and this branch fixed: the launcher shebang and the dispatcher's PATH
prepend resolved the venv interpreter's symlink out to `/usr/bin/python3.12`.

Remaining pytest-phase couplings (host paths and host packages, none an import from the
monorepo by the package itself):

| count | first line | coupling |
|---|---|---|
| 18 | `ModuleNotFoundError: No module named 'torch'` | `test_equivalence_probe` imports torch; a host extra, not a wheel dependency |
| 17 + 2 | `CampaignError: mutation runner component is missing or unsafe: tooling/native/conductor-native/src/mutation_evidence.rs` / `cannot inventory ... research/tests/...` | `test_mutation_testing`, `test_mutation_value` pin host-repo paths |
| 9 | `FileNotFoundError: <dest>/src/.agent_hooks/...` | `test_local_ai_policy`, `test_workspace_eval`, `test_workspace_runtime_matrix` derive `.agent_hooks` from the package parent |
| 3 + 1 | vulture whitelist / `research/tools/vulture_whitelist.py` | `test_vulture_audit`, `test_mutation_testing` read host files |
| 1 + 1 + 1 | `.github/CODEOWNERS`, `vault_health.py`, `assert 2 == 1` | `test_candidate_review_cli_policy`, `test_guardrail_audit` expect repo files |
| 1 + 1 + 1 | `only 674 imports checked`, `assert 91 > 200`, `assert 3 == 0` | `test_repo_index`, `test_mutation_coverage` measure the host tree |
| 1 | `test_launcher_is_tracked_and_executable` | `tooling/hooks/dispatch/test_registry.py` expects the launcher at `<root>/.claude/hooks/dispatch.py`; the standalone layout keeps it at `hooks/dispatch.py` |
