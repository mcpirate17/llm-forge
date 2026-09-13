# conductor.mk targets

`conductor.mk` is the platform's user-facing command surface: every governance,
mutation, hygiene and audit entry point a host project runs. A host includes it
from its own Makefile (`include /path/to/llm-forge/conductor.mk`) and overrides
the variables below to match its own layout instead of editing the recipes —
every recipe invokes `$(PYTHON) -m conductor.<module>`, so it runs under
whatever interpreter installed `conductor-tooling`.

This page is generated from the `##` help annotations in `conductor.mk`
(47 targets); regenerate it rather than editing by hand when a
target changes.

## Variables a host overrides

| Variable | Default |
|---|---|
| `UV` | `env -u VIRTUAL_ENV uv` |
| `PYTHON` | `$(UV) run python` |
| `CONDUCTOR_HOST_ROOT` | `$(CURDIR)` |
| `CONDUCTOR_PACKAGE_ROOT` | `src/conductor` |
| `CONDUCTOR_CANDIDATE_POLICY` | `candidate_policy.toml` |
| `CONDUCTOR_MUTATION_REGISTRY` | `campaigns/registry.json` |
| `CONDUCTOR_AUDIT_DIR` | `tasks/audit` |
| `CONDUCTOR_REPORTS_DIR` | `reports` |
| `CONDUCTOR_GATE_FINDINGS_DIR` | `$(CONDUCTOR_REPORTS_DIR)/gate_findings` |
| `PREFLIGHT_REF` | `HEAD` |
| `PREFLIGHT_BASE` | `origin/main` |
| `SLOP_BASE` | `origin/main` |
| `CRG_REPO` | `$(CONDUCTOR_HOST_ROOT)` |
| `CODEX_JOURNAL_MAX_STATUS` | `80` |
| `CODEX_JOURNAL_OUT_DIR` | `tasks/codex_journal` |
| `NOTEBOOKLM_OUT` | `tasks/notebooklm/codex_context_bundle.md` |
| `NOTEBOOKLM_RESEARCH_OUT` | `tasks/notebooklm/research_briefing_bundle.md` |

## Targets

| Command | What it does |
|---|---|
| `make governance-check` | Run the exact candidate-index fast review |
| `make governance-audit` | Run the exact candidate-index full review and emit CI-format artifacts |
| `make governance-preflight` | Reproduce the CI review locally for PREFLIGHT_REF (default HEAD) |
| `make governance-fix` | Explicitly fix only FIX_PATHS='path ...' outside commit hooks |
| `make governance-commit` | Hold the commit mutex; pass COMMIT_ARGS='-m ... path ...' |
| `make governance-claim` | Claim exact paths; set CLAIM_PATHS and CLAIM_JUSTIFICATION (OWNER defaults to this lane) |
| `make governance-release-claim` | Release CLAIM_ID as OWNER |
| `make governance-claims` | Show active claims, one line each (CLAIM_PATH="a b" filters by overlap) |
| `make governance-claims-json` | Show the full machine-readable claim store (active and expired) |
| `make governance-close-session` | Atomically close session: release claims, append handoff, update state |
| `make slop-gate` | Tier-1 equivalence probe over changed modules; reports only (add SLOP_ENFORCE=--enforce to fail) |
| `make slop-backlog` | Sweep SLOP_MODULES (default: changed) into the ranked backlog + ledger |
| `make slop-imports` | Ablate the silenced (noqa F401) imports of SLOP_MODULE against its drivers |
| `make slop-probe` | Probe one module against its driver tests: SLOP_MODULE=path.py SLOP_TESTS='tests...' |
| `make complexity-report` | Report production Python cyclomatic complexity |
| `make complexity-check` | Fail on new production Python D-F complexity blocks |
| `make complexity-refresh-baseline` | Refresh the legacy D-F complexity baseline |
| `make dupes` | Duplicate code detector (jscpd + pmd-python) |
| `make dupes-jscpd` | JSCPD duplicate detector |
| `make dupes-jscpd-check` | JSCPD duplicate detector as a failing gate |
| `make dupes-pmd` | PMD CPD duplicate detector for Python |
| `make dupes-pmd-check` | PMD CPD duplicate detector as a failing gate |
| `make dupes-pylint` | Pylint duplicate-code detector |
| `make dupes-nicad` | NiCad near-miss clone detector |
| `make dupes-deep` | Run JSCPD and PMD CPD duplicate audits |
| `make dupes-deep-check` | Run duplicate detectors as a failing gate |
| `make mutation-retention` | Report (or with MUTATION_RETENTION_APPLY=1, delete) uncitable receipts |
| `make mutation-patch-audit` | Report campaigns that cannot be applied, re-run, or vouched for |
| `make mutation-patch-audit-record` | Re-record the reproducibility baseline after repairs |
| `make graph-seed-worktree` | Seed W=<worktree>'s code-review-graph store from this checkout (W required) |
| `make worktree-reap` | Preview finished worktrees; REAP_ARGS=--apply explicitly removes eligible trees |
| `make crg-probe` | Handshake with the code-review-graph MCP server declared in CRG_REPO's .mcp.json |
| `make crg-sync` | Install this tree's native crates into the code-review-graph MCP interpreter |
| `make crg-check` | Report (never repair) drift between this tree and the MCP interpreter |
| `make workspace-hygiene` | Read-only: what is safe to delete (branches, worktrees, claims) and what breaks a clean clone |
| `make branch-policy` | Branch binding status: live claim/branch pairs, age, PR number, staleness |
| `make branch-policy-audit` | Check every branch-policy rule decidable without a push; exits 1 on violations |
| `make checkout-sync` | Snapshot the dirty tree, then fast-forward this checkout onto the remote line |
| `make dead-tests` | Import-closure dead-test finder: broken tests, test-only targets, unreferenced sources |
| `make test-graph` | Run pytest on tests selected via code-review-graph for changed code |
| `make codex-journal` | Append an Obsidian-compatible Codex journal entry |
| `make notebooklm-bundle` | Build a curated Markdown bundle for manual NotebookLM upload |
| `make notebooklm-research-bundle` | Build a research-focused NotebookLM upload bundle (includes CodexVault notes when present) |
| `make test-conductor-native` | cargo test the conductor-native crate |
| `make test-slop-core` | cargo test the slop-core crate |
| `make test-forge` | cargo test the forge crate (native hook launcher, step 1 of the Rust hook port) |
| `make forge-build` | Build forge and install it to .tools/bin |
