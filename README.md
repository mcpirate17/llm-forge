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

**Rehearsal.** `make tooling-standalone-smoke` assembles this package from the committed
tree in a scratch dir (`src/conductor`, `native/conductor-native`, `hooks/` without the
project extension, this manifest), installs it with `uv` into a fresh venv (building the
crate), proves the host packages are unimportable there, and runs `pytest src/conductor`
with `CONDUCTOR_PROJECT_TEST_PLUGIN=""`. Its failures are the hidden couplings; the
report (`research/reports/tooling_standalone_smoke.json`) groups them by first line.

## Last rehearsal

2026-09-02, tree `31767d45c6fc` (branch `tooling-boundary-contract`), install 19 s,
pytest 36 s: **865 passed, 47 failed, 46 errors, 5 skipped**, exit 1. Failure groups:

| count | first line | coupling |
|---|---|---|
| 31 + 1 | `PolicyError: required candidate policy is unreadable: conductor/candidate_policy.toml` | cwd-relative literal path; the package sits at `src/conductor` |
| 29 + 7 + 1 | `ImportError: slop_core is not built` / `No module named 'slop_core'` | host Rust crate (`host-native` extra); `repo_index`, `native_ablations`, `slop_ledger`, `slop_gate` |
| 9 | `FileNotFoundError: <dest>/src/.agent_hooks/crg_gate.py` | tests derive `.agent_hooks` from the package parent |
| 6 + 4 | `ModuleNotFoundError: No module named 'audit'` | repo-root `audit/` package imported by `mutation_testing` (`audit.orchestrator.snapshot_worktree`) |
| 1 | `FileNotFoundError: .github/CODEOWNERS` | repo file read by a test |
| 1 | `AssertionError: assert [] == ['vault_health.py']` | `test_iter_files_accepts_explicit_file_target` expects a repo file |

Collection errors (8 files): `test_candidate_review_structure_audit` (policy path),
`test_equivalence_probe` (`slop_core`), `test_mutation_coverage`, `test_mutation_dry_run`,
`test_mutation_testing`, `test_mutation_value`, `test_receipt_verify`, `test_runner_lineage`
(`audit`). slop-core has since become a path source (`native/slop-core`), so the
`slop_core` group closes at the next rehearsal. None of these is visible to the boundary contract: they are path and
host-package couplings, the split's remaining work.
