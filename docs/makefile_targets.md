# conductor.mk targets

`conductor.mk` is the platform's user-facing command surface: every governance,
mutation, hygiene and audit entry point a host project runs. A host includes it
from its own Makefile (`include /path/to/llm-forge/conductor.mk`) and overrides
the variables below to match its own layout instead of editing the recipes —
native commands invoke `$(FORGE)`; remaining compatibility commands use
`$(PYTHON) -m conductor.<module>` under the installed interpreter.
See [native backlog usage](equivalence-backlog.md) for its preview and publication flags.

This page is generated from the `##` help annotations in `conductor.mk`
(50 targets); regenerate it rather than editing by hand when a
target changes.

## Variables a host overrides

| Variable | Default |
|---|---|
| `UV` | `env -u VIRTUAL_ENV uv` |
| `PYTHON` | `$(UV) run python` |
| `FORGE` | `forge` |
| `CONDUCTOR_HOST_ROOT` | `$(CURDIR)` |
| `CONDUCTOR_PACKAGE_ROOT` | `src/conductor` |
| `CONDUCTOR_CANDIDATE_POLICY` | `candidate_policy.toml` |
| `CONDUCTOR_MUTATION_REGISTRY` | `campaigns/registry.json` |
| `CONDUCTOR_AUDIT_DIR` | `tasks/audit` |
| `CONDUCTOR_REPORTS_DIR` | `reports` |
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
| `make governance-check` | Run the native candidate-index fast review |
| `make governance-audit` | Run the native full review and export this execution's artifacts |
| `make governance-preflight` | Run the native CI review for PREFLIGHT_REF (default HEAD) |
| `make governance-fix` | Explicit native Ruff batch; set FIX_PATHS_FILE (JSON), FIX_GRANTS and FIX_RUFF_SHA256 |
| `make governance-commit` | Hold the commit mutex; pass COMMIT_ARGS='-m ... path ...' |
| `make governance-claim` | Claim exact paths; set CLAIM_PATHS and CLAIM_JUSTIFICATION (OWNER defaults to this lane) |
| `make governance-release-claim` | Release CLAIM_ID as OWNER |
| `make governance-claims` | Show active claims, one line each (CLAIM_PATH="a b" filters by overlap) |
| `make governance-claims-json` | Show the full machine-readable claim store (active and expired) |
| `make governance-close-session` | Close with explicit claim scope and memory configuration |
| `make slop-gate` | Equivalence probe over changed modules; reports only (add SLOP_ENFORCE=--enforce to fail) |
| `make slop-backlog` | Probe with the Python gate, then fold summaries using native Forge (SLOP_MODULES defaults to changed) |
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
| `make mutation-reap` | List engine runs orphaned by a dead engine (MUTATION_REAP_APPLY=1 kills them) |
| `make ci-history` | Fetch the ci_history cache the ledger's outcome join reads |
| `make ledger-rollup` | Roll this project's harness transcripts into the ledger root |
| `make ledger-report` | Print cost budget metrics and their measured statuses |
| `make route-report` | Per-tier dispatch table: over_cap, would_deny, would_route, rework, ci_red |
| `make cost-budget-audit` | Check hook latency, resend bytes and tokens/landed-PR against the recorded baseline |
| `make cost-budget-record` | Re-record the cost-ledger budget baseline for the current window |
| `make graph-seed-worktree` | Seed W=<worktree>'s code-review-graph store from this checkout (W required) |
| `make worktree-reap` | Preview finished worktrees; REAP_ARGS=--apply explicitly removes eligible trees |
| `make workspace-hygiene` | Read-only native claims, imports, checkout drift and workspace exposure report |
| `make branch-policy-audit` | Check every branch-policy rule decidable without a push; exits 1 on violations |
| `make checkout-sync` | Native guarded dirty-layer fast-forward (set CHECKOUT_SYNC_RECOVERY_DIR) |
| `make dead-tests` | Native advisory Python import-closure and unreferenced-source report |
| `make test-graph` | Advisory graph-selected tests; set GRAPH_TEST_ARGS for explicit guest prerequisites |
| `make codex-journal` | Append an Obsidian-compatible Codex journal entry |
| `make notebooklm-bundle` | Build a curated Markdown bundle for manual NotebookLM upload |
| `make notebooklm-research-bundle` | Build a research-focused NotebookLM upload bundle (includes CodexVault notes when present) |
| `make test-conductor-native` | cargo test the conductor-native crate |
| `make test-slop-core` | cargo test the slop-core crate |
| `make test-forge` | cargo test the forge crate (native hook launcher, step 1 of the Rust hook port) |
| `make forge-build` | Build forge and install it to .tools/bin for a host project |
