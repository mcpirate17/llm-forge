# conductor.mk — the platform's user-facing commands.
#
# Ported from the LLM monorepo's root Makefile (81 targets) during the 2026-09-12
# split: everything here shells only to `conductor.*` modules, tooling hooks, or the
# native crates this repo ships. Project-specific targets (training, GPU kernels,
# corpora, model code) stayed in the monorepo; see the PR that added this file for
# the full target-by-target classification.
#
# A host project includes this file from its own Makefile:
#
#   include /path/to/llm-forge/conductor.mk
#
# and overrides the variables below to match its own layout instead of editing the
# recipes. Native review, claims and session commands invoke the explicit FORGE
# executable; remaining Python maintenance commands use the installed module.

UV ?= env -u VIRTUAL_ENV uv
PYTHON ?= $(UV) run python

# The root a host project's own data lives under: candidate_policy.toml,
# campaigns/, .git, node_modules, etc. Override this when conductor.mk is included
# from a Makefile that is not itself invoked from the host root.
CONDUCTOR_HOST_ROOT ?= $(CURDIR)

# Mirrors [tool.conductor] in pyproject.toml -- see project_paths.py. A host with a
# different layout overrides these, not the recipes below.
CONDUCTOR_PACKAGE_ROOT ?= src/conductor
CONDUCTOR_CANDIDATE_POLICY ?= candidate_policy.toml
CONDUCTOR_MUTATION_REGISTRY ?= campaigns/registry.json

# Output directories. The monorepo this was split from calls these tasks/audit and
# research/reports; named here so a host with neither convention can redirect both
# in one place instead of patching every recipe that writes a report.
CONDUCTOR_AUDIT_DIR ?= tasks/audit
CONDUCTOR_REPORTS_DIR ?= reports

.PHONY: governance-check governance-audit governance-preflight governance-fix \
	governance-commit governance-claim governance-release-claim governance-claims \
	governance-claims-json governance-close-session \
	slop-gate slop-backlog slop-imports slop-probe \
	complexity-report complexity-check complexity-refresh-baseline \
	dupes dupes-jscpd dupes-jscpd-check dupes-pmd dupes-pmd-check dupes-pylint \
	dupes-nicad dupes-deep dupes-deep-check \
	mutation-retention mutation-patch-audit mutation-patch-audit-record mutation-reap \
	ledger-rollup ledger-report route-report cost-budget-audit cost-budget-record \
	graph-seed-worktree worktree-reap workspace-hygiene branch-policy branch-policy-status \
	branch-policy-audit checkout-sync crg-probe crg-check \
	dead-tests test-graph codex-journal notebooklm-bundle notebooklm-research-bundle \
	test-conductor-native test-slop-core test-forge forge-build

# ── Governance: candidate review, claims, sessions ──────────────────────
# Review executes the native coordinator and owned Stage mapping. Grants are an
# explicit operator input outside the source tree, never synthesized by Make.
# Extra native options include runner/registry/equivalence pins and --owner.
CONDUCTOR_REVIEW_GRANTS ?=
CONDUCTOR_REVIEW_ARGS ?=

governance-check:  ## Run the native candidate-index fast review
	@test -n "$(CONDUCTOR_REVIEW_GRANTS)" || { echo "Set CONDUCTOR_REVIEW_GRANTS to an operator-owned grants JSON file"; exit 2; }
	"$(FORGE)" candidate review --repo "$(CONDUCTOR_HOST_ROOT)" \
		--surface manual --kind index --profile fast --execute \
		--grants "$(CONDUCTOR_REVIEW_GRANTS)" $(CONDUCTOR_REVIEW_ARGS)

governance-audit:  ## Run the native full review and export this execution's artifacts
	@test -n "$(CONDUCTOR_REVIEW_GRANTS)" || { echo "Set CONDUCTOR_REVIEW_GRANTS to an operator-owned grants JSON file"; exit 2; }
	@mkdir -p "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)"
	"$(FORGE)" candidate review --repo "$(CONDUCTOR_HOST_ROOT)" \
		--surface manual --kind index --profile full --execute \
		--grants "$(CONDUCTOR_REVIEW_GRANTS)" $(CONDUCTOR_REVIEW_ARGS) \
		--receipt "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)/candidate_review.json" \
		--sarif-out "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)/candidate_review.sarif" \
		--junit-out "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)/candidate_review.junit.xml"

PREFLIGHT_REF ?= HEAD
PREFLIGHT_BASE ?= origin/main

governance-preflight:  ## Run the native CI review for PREFLIGHT_REF (default HEAD)
	@test -n "$(CONDUCTOR_REVIEW_GRANTS)" || { echo "Set CONDUCTOR_REVIEW_GRANTS to an operator-owned grants JSON file"; exit 2; }
	@mkdir -p "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)"
	"$(FORGE)" candidate review --repo "$(CONDUCTOR_HOST_ROOT)" \
		--surface ci --kind range --target-ref "$(PREFLIGHT_REF)" \
		--base-ref "$(PREFLIGHT_BASE)" --profile full --execute \
		--grants "$(CONDUCTOR_REVIEW_GRANTS)" $(CONDUCTOR_REVIEW_ARGS) \
		--receipt "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)/preflight.json"
	"$(FORGE)" candidate report "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)/preflight.json" --format human

FIX_PATHS_FILE ?=
FIX_GRANTS ?=
FIX_RUFF_SHA256 ?=
COMMIT_ARGS ?=
CLAIM_PATHS ?=
CLAIM_JUSTIFICATION ?=
CLAIM_HOURS ?=
CLAIM_ID ?=
CLAIM_PATH ?=
OWNER ?=
SESSION_TITLE ?=
SESSION_BODY ?=

governance-fix:  ## Explicit native Ruff batch; set FIX_PATHS_FILE (JSON), FIX_GRANTS and FIX_RUFF_SHA256
	@test -n "$(FIX_PATHS_FILE)" -a -n "$(FIX_GRANTS)" -a -n "$(FIX_RUFF_SHA256)" || { echo "Set FIX_PATHS_FILE, FIX_GRANTS and FIX_RUFF_SHA256"; exit 2; }
	"$(FORGE)" candidate fix --repo "$(CONDUCTOR_HOST_ROOT)" \
		--grants "$(FIX_GRANTS)" --ruff-sha256 "$(FIX_RUFF_SHA256)" \
		--paths-from "$(FIX_PATHS_FILE)" --execute

governance-commit:  ## Hold the commit mutex; pass COMMIT_ARGS='-m ... path ...'
	"$(FORGE)" candidate commit --repo "$(CONDUCTOR_HOST_ROOT)" -- $(COMMIT_ARGS)

governance-claim:  ## Claim exact paths; set CLAIM_PATHS and CLAIM_JUSTIFICATION (OWNER defaults to this lane)
	@test -n "$(CLAIM_PATHS)" -a -n "$(CLAIM_JUSTIFICATION)" || { echo "Set CLAIM_PATHS and CLAIM_JUSTIFICATION"; exit 2; }
	"$(FORGE)" claim create --repo "$(CONDUCTOR_HOST_ROOT)" \
		$(if $(OWNER),--owner "$(OWNER)") --justification "$(CLAIM_JUSTIFICATION)" \
		$(if $(CLAIM_HOURS),--hours "$(CLAIM_HOURS)") \
		$(CLAIM_PATHS)

governance-release-claim:  ## Release CLAIM_ID as OWNER
	@test -n "$(OWNER)" -a -n "$(CLAIM_ID)" || { echo "Set OWNER and CLAIM_ID"; exit 2; }
	"$(FORGE)" claim release --repo "$(CONDUCTOR_HOST_ROOT)" \
		--owner "$(OWNER)" "$(CLAIM_ID)"

governance-claims:  ## Show active claims, one line each (CLAIM_PATH="a b" filters by overlap)
	"$(FORGE)" claim list --repo "$(CONDUCTOR_HOST_ROOT)" --compact \
		$(foreach p,$(CLAIM_PATH),--path $(p))

governance-claims-json:  ## Show the full machine-readable claim store (active and expired)
	"$(FORGE)" claim list --repo "$(CONDUCTOR_HOST_ROOT)"

SESSION_CLOSE_ARGS ?=

governance-close-session:  ## Close with explicit claim scope and memory configuration
	@test -n "$(OWNER)" || { echo "Set OWNER='<agent>' (SESSION_CLOSE_ARGS must supply memory settings or --no-memory-index)"; exit 2; }
	"$(FORGE)" session close --host "$(CONDUCTOR_HOST_ROOT)" \
		--owner "$(OWNER)" \
		$(if $(SESSION_TITLE),--title "$(SESSION_TITLE)",) \
		$(if $(SESSION_BODY),--body "$(SESSION_BODY)",) \
		$(if $(CLAIM_ID),--claim-id "$(CLAIM_ID)",) \
		$(SESSION_CLOSE_ARGS)

# ── Slop: tier-1 equivalence probing ────────────────────────────────────

SLOP_BASE ?= origin/main
SLOP_MODULES ?=
SLOP_JOBS ?=
SLOP_ENFORCE ?=
SLOP_MODULE ?=
SLOP_TESTS ?=
SLOP_GRANTS ?=
SLOP_GUEST_PATH ?=
SLOP_GUEST_SHA256 ?=
SLOP_ARTIFACTS ?=
SLOP_INPUTS ?=
SLOP_ALL_TRACKED ?=
# Raw operator-supplied native gate arguments (for example, explicit resource limits).
SLOP_GATE_ARGS ?=

slop-gate:  ## Native live equivalence gate; set SLOP_GRANTS, SLOP_GUEST_PATH and SLOP_GUEST_SHA256
	@test -n "$(SLOP_GRANTS)" -a -n "$(SLOP_GUEST_PATH)" -a -n "$(SLOP_GUEST_SHA256)" || { echo "Set SLOP_GRANTS, SLOP_GUEST_PATH and SLOP_GUEST_SHA256"; exit 2; }
	"$(FORGE)" equivalence gate --root "$(CONDUCTOR_HOST_ROOT)" --live \
		--grants "$(SLOP_GRANTS)" --guest-path "$(SLOP_GUEST_PATH)" --guest-sha256 "$(SLOP_GUEST_SHA256)" \
		$(if $(SLOP_ARTIFACTS),--artifacts "$(SLOP_ARTIFACTS)") \
		$(if $(strip $(SLOP_INPUTS)),$(foreach p,$(SLOP_INPUTS),--input "$(p)"),--tracked-only) \
		$(if $(strip $(SLOP_MODULES)),$(foreach m,$(SLOP_MODULES),--module "$(m)"),$(if $(filter 1,$(SLOP_ALL_TRACKED)),--all-tracked,--changed-from "$(SLOP_BASE)")) \
		$(if $(SLOP_JOBS),--jobs $(SLOP_JOBS)) $(SLOP_ENFORCE) $(SLOP_GATE_ARGS)

slop-backlog:  ## Probe with the Python gate, then fold summaries using native Forge (SLOP_MODULES defaults to changed)
	@mkdir -p "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_REPORTS_DIR)"
	$(PYTHON) -m conductor.equivalence_gate gate \
		--base "$(SLOP_BASE)" --root "$(CONDUCTOR_HOST_ROOT)" \
		$(if $(SLOP_MODULES),$(foreach m,$(SLOP_MODULES),--module $(m))) \
		$(if $(SLOP_JOBS),--jobs $(SLOP_JOBS)) \
		--json "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_REPORTS_DIR)/slop_sweep.json"
	$(FORGE) equivalence backlog --root "$(CONDUCTOR_HOST_ROOT)" --apply --consume-recorded \
		"$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_REPORTS_DIR)/slop_sweep.json"

slop-imports:  ## Ablate the silenced (noqa F401) imports of SLOP_MODULE against its drivers
	@test -n "$(SLOP_MODULE)" || { echo "Set SLOP_MODULE=path.py"; exit 2; }
	$(PYTHON) -m conductor.import_ablation "$(SLOP_MODULE)" --root "$(CONDUCTOR_HOST_ROOT)"

slop-probe:  ## Probe one module against its driver tests: SLOP_MODULE=path.py SLOP_TESTS='tests...'
	@test -n "$(SLOP_MODULE)" -a -n "$(SLOP_TESTS)" || { echo "Set SLOP_MODULE and SLOP_TESTS"; exit 2; }
	$(PYTHON) -m conductor.equivalence_probe "$(SLOP_MODULE)" $(SLOP_TESTS)

# ── Complexity ratchet ───────────────────────────────────────────────────
# Host-root/config/environment resolution is shared with native project_paths.
# Legacy default scopes remain conductor/research/aria_core/aria_designer; hosts
# can select explicit scopes/baselines with COMPLEXITY_ARGS.
COMPLEXITY_ARGS ?=

complexity-report:  ## Report production Python cyclomatic complexity
	$(FORGE) complexity report --repo "$(CONDUCTOR_HOST_ROOT)" $(COMPLEXITY_ARGS)

complexity-check:  ## Fail on new production Python D-F complexity blocks
	$(FORGE) complexity check --repo "$(CONDUCTOR_HOST_ROOT)" $(COMPLEXITY_ARGS)

complexity-refresh-baseline:  ## Refresh the legacy D-F complexity baseline
	$(FORGE) complexity refresh-baseline --repo "$(CONDUCTOR_HOST_ROOT)" $(COMPLEXITY_ARGS)

# ── Duplicate code detectors ─────────────────────────────────────────────
# Legacy Python orchestration reads [tool.forge.duplicate_audit] source_dirs,
# defaulting to src/ and native/. Empty source selections fail explicitly.
# Detector execution and baseline semantics remain separate from native AST advice.

dupes:  ## Duplicate code detector (jscpd + pmd-python)
	$(PYTHON) -m conductor.run_duplicate_audit --root "$(CONDUCTOR_HOST_ROOT)"

dupes-jscpd:  ## JSCPD duplicate detector
	$(PYTHON) -m conductor.run_duplicate_audit --root "$(CONDUCTOR_HOST_ROOT)" --tool jscpd

dupes-jscpd-check:  ## JSCPD duplicate detector as a failing gate
	$(PYTHON) -m conductor.run_duplicate_audit --root "$(CONDUCTOR_HOST_ROOT)" --tool jscpd --check

dupes-pmd:  ## PMD CPD duplicate detector for Python
	$(PYTHON) -m conductor.run_duplicate_audit --root "$(CONDUCTOR_HOST_ROOT)" --tool pmd-python

dupes-pmd-check:  ## PMD CPD duplicate detector as a failing gate
	$(PYTHON) -m conductor.run_duplicate_audit --root "$(CONDUCTOR_HOST_ROOT)" --tool pmd-python --check

dupes-pylint:  ## Pylint duplicate-code detector
	$(PYTHON) -m conductor.run_duplicate_audit --root "$(CONDUCTOR_HOST_ROOT)" --tool pylint

dupes-nicad:  ## NiCad near-miss clone detector
	$(PYTHON) -m conductor.run_duplicate_audit --root "$(CONDUCTOR_HOST_ROOT)" --tool nicad-python

dupes-deep:  ## Run JSCPD and PMD CPD duplicate audits
	$(PYTHON) -m conductor.run_duplicate_audit --root "$(CONDUCTOR_HOST_ROOT)"

dupes-deep-check:  ## Run duplicate detectors as a failing gate
	$(PYTHON) -m conductor.run_duplicate_audit --root "$(CONDUCTOR_HOST_ROOT)" --check

# ── Mutation evidence maintenance ────────────────────────────────────────

MUTATION_RETENTION_APPLY ?=
MUTATION_RETENTION_ARGS ?=
MUTATION_AUDIT_ARGS ?=
MUTATION_REAP_APPLY ?=

mutation-retention:  ## Report (or with MUTATION_RETENTION_APPLY=1, delete) uncitable receipts
	$(PYTHON) -m conductor.mutation_retention --repo-root "$(CONDUCTOR_HOST_ROOT)" \
		$(if $(MUTATION_RETENTION_APPLY),--apply,) $(MUTATION_RETENTION_ARGS)

# Operator-provisioned pins; the build profile asserts the exact system attributes
# path for the pinned Git binary (or an explicitly attribute-free build).
CONDUCTOR_MUTATION_GIT ?=
CONDUCTOR_MUTATION_GIT_SHA256 ?=
CONDUCTOR_MUTATION_GIT_BUILD_PROFILE ?=
CONDUCTOR_MUTATION_GIT_BUILD_PROFILE_SHA256 ?=
CONDUCTOR_MUTATION_RUNNER_BUNDLE ?=
CONDUCTOR_MUTATION_RUNNER_SHA256 ?=

mutation-patch-audit:  ## Report campaigns that cannot be applied, re-run, or vouched for
	@test -n "$(CONDUCTOR_MUTATION_GIT)" -a -n "$(CONDUCTOR_MUTATION_GIT_SHA256)" -a -n "$(CONDUCTOR_MUTATION_GIT_BUILD_PROFILE)" -a -n "$(CONDUCTOR_MUTATION_GIT_BUILD_PROFILE_SHA256)" -a -n "$(CONDUCTOR_MUTATION_RUNNER_BUNDLE)" -a -n "$(CONDUCTOR_MUTATION_RUNNER_SHA256)" || { echo "Set operator-pinned Git, build profile and runner bundle paths and SHA-256 values"; exit 2; }
	"$(FORGE)" candidate mutation-corpus --repo "$(CONDUCTOR_HOST_ROOT)" \
		--registry "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_MUTATION_REGISTRY)" \
		--git "$(CONDUCTOR_MUTATION_GIT)" --git-sha256 "$(CONDUCTOR_MUTATION_GIT_SHA256)" \
		--git-build-profile "$(CONDUCTOR_MUTATION_GIT_BUILD_PROFILE)" --git-build-profile-sha256 "$(CONDUCTOR_MUTATION_GIT_BUILD_PROFILE_SHA256)" \
		--mutation-runner-bundle "$(CONDUCTOR_MUTATION_RUNNER_BUNDLE)" --mutation-runner-sha256 "$(CONDUCTOR_MUTATION_RUNNER_SHA256)" $(MUTATION_AUDIT_ARGS)

# Records today's findings as the new debt. Run only after repairing campaigns.
mutation-patch-audit-record:  ## Re-record the reproducibility baseline after repairs
	@test -n "$(CONDUCTOR_MUTATION_GIT)" -a -n "$(CONDUCTOR_MUTATION_GIT_SHA256)" -a -n "$(CONDUCTOR_MUTATION_GIT_BUILD_PROFILE)" -a -n "$(CONDUCTOR_MUTATION_GIT_BUILD_PROFILE_SHA256)" -a -n "$(CONDUCTOR_MUTATION_RUNNER_BUNDLE)" -a -n "$(CONDUCTOR_MUTATION_RUNNER_SHA256)" || { echo "Set operator-pinned Git, build profile and runner bundle paths and SHA-256 values"; exit 2; }
	"$(FORGE)" candidate mutation-corpus --repo "$(CONDUCTOR_HOST_ROOT)" \
		--registry "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_MUTATION_REGISTRY)" \
		--git "$(CONDUCTOR_MUTATION_GIT)" --git-sha256 "$(CONDUCTOR_MUTATION_GIT_SHA256)" \
		--git-build-profile "$(CONDUCTOR_MUTATION_GIT_BUILD_PROFILE)" --git-build-profile-sha256 "$(CONDUCTOR_MUTATION_GIT_BUILD_PROFILE_SHA256)" \
		--mutation-runner-bundle "$(CONDUCTOR_MUTATION_RUNNER_BUNDLE)" --mutation-runner-sha256 "$(CONDUCTOR_MUTATION_RUNNER_SHA256)" --summary --write-baseline

mutation-reap:  ## List engine runs orphaned by a dead engine (MUTATION_REAP_APPLY=1 kills them)
	$(PYTHON) -m conductor.mutation_testing_support reap \
		--repo-root "$(CONDUCTOR_HOST_ROOT)" $(if $(MUTATION_REAP_APPLY),--apply,)

# ── Cost ledger budget ratchet (docs/design/cost_ledger.md section 4) ───
# Native budget orchestration shares the ledger audit computation. Reporting
# names measured statuses directly; only explicit --record changes a baseline.
# Refresh the optional outcome cache separately with `make ci-history`.

COST_BUDGET_ARGS ?=
LEDGER_ROOT ?=
# An unset root is resolved by native code from LEDGER_ROOT/XDG_STATE_HOME/HOME.
LEDGER_ROOT_ARG = $(if $(strip $(LEDGER_ROOT)),--ledger-root "$(LEDGER_ROOT)",)
LEDGER_ARGS ?=

# Rolls this project's harness transcripts (~/.claude/projects/<this repo,
# dashes for slashes>) into the ledger root, joined to this repository's
# landed commits. Idempotent per session per day: rerunning replaces a
# session's rows for that day instead of duplicating them, which is what
# makes cost-budget-audit's dependency below safe -- the audit always reads
# fresh rows, never a stale or empty root.
CI_HISTORY_OUT ?= ledger/ci_history/$(shell git remote get-url origin 2>/dev/null | sed -e 's#.*github.com[:/]##' -e 's#\.git$$##' -e 's#/#_#g').json

ci-history:  ## Fetch the ci_history cache the ledger's outcome join reads
	$(PYTHON) -m conductor.ci_history_fetch --repo . --out "$(CI_HISTORY_OUT)" $(CI_HISTORY_ARGS)

.PHONY: ci-history

ledger-rollup:  ## Roll this project's harness transcripts into the ledger root
	"$(FORGE)" ledger budget --host "$(CONDUCTOR_HOST_ROOT)" \
		$(LEDGER_ROOT_ARG) rollup $(LEDGER_ARGS)

ledger-report:  ## Print cost budget metrics and their measured statuses
	"$(FORGE)" ledger budget --host "$(CONDUCTOR_HOST_ROOT)" \
		$(LEDGER_ROOT_ARG) report $(LEDGER_ARGS)

# The routing side of the same ledger: forge's per-tier dispatch table incl.
# over_cap, would_deny, would_route, rework and ci_red over a trailing
# window. Goes straight to the binary (no Python shim): the table is the
# thing itself, and `forge` is installable standalone via cargo.
FORGE ?= forge
ROUTE_REPORT_ARGS ?= --window-days 7

route-report:  ## Per-tier dispatch table: over_cap, would_deny, would_route, rework, ci_red
	@if ! command -v "$(FORGE)" >/dev/null 2>&1; then \
		echo "forge is not on PATH: cargo install --git https://github.com/mcpirate17/llm-forge --locked forge" >&2; \
		exit 2; \
	fi
	"$(FORGE)" ledger report $(LEDGER_ROOT_ARG) $(ROUTE_REPORT_ARGS)

# The audit reads whatever rows sit in the ledger root, so refresh them first
# (ledger-rollup above) rather than auditing a stale window.
cost-budget-audit: ledger-rollup  ## Check hook latency, resend bytes and tokens/landed-PR against the recorded baseline
	"$(FORGE)" ledger budget --host "$(CONDUCTOR_HOST_ROOT)" \
		$(LEDGER_ROOT_ARG) audit $(COST_BUDGET_ARGS)

# Records this window's metrics as the new baseline. Run only after a deliberate
# improvement or an accepted regression -- recording a grown baseline is how the
# ratchet is defeated.
cost-budget-record:  ## Re-record the cost-ledger budget baseline for the current window
	"$(FORGE)" ledger budget --host "$(CONDUCTOR_HOST_ROOT)" \
		$(LEDGER_ROOT_ARG) audit --record $(COST_BUDGET_ARGS)

# ── Worktree and graph maintenance ───────────────────────────────────────
# graph-seed-worktree and worktree-reap manage OTHER worktrees a host project
# creates for its own agents; this repository's own AGENTS.md forbids worktrees
# for work on llm-forge itself, which is unrelated to whether a host wants this
# capability for its own layout.

REAP_ARGS ?=

graph-seed-worktree:  ## Seed W=<worktree>'s code-review-graph store from this checkout (W required)
	@test -n "$(W)" || { echo "usage: make graph-seed-worktree W=/path/to/worktree" >&2; exit 2; }
	$(PYTHON) -m conductor.crg_seed_worktree "$(CONDUCTOR_HOST_ROOT)" $(W)

worktree-reap:  ## Preview finished worktrees; REAP_ARGS=--apply explicitly removes eligible trees
	$(PYTHON) -m conductor.worktree_reap --repo "$(CONDUCTOR_HOST_ROOT)" $(REAP_ARGS)

CRG_REPO ?= $(CONDUCTOR_HOST_ROOT)
MCP_SOCKET ?=

crg-probe crg-check:  ## Verify the native MCP binding; MCP_SOCKET must name the running service
	@test -n "$(MCP_SOCKET)" || { echo "Set MCP_SOCKET to the socket returned by forge harness start" >&2; exit 2; }
	forge mcp --host "$(CRG_REPO)" check --socket "$(MCP_SOCKET)"

# ── Workspace / branch hygiene ───────────────────────────────────────────

workspace-hygiene:  ## Read-only native claims, imports, checkout drift and workspace exposure report
	"$(FORGE)" workspace-hygiene --repo "$(CONDUCTOR_HOST_ROOT)"

branch-policy branch-policy-status:  ## Branch binding status: live claim/branch pairs, age, PR number, staleness
	"$(FORGE)" branch-policy --repo "$(CONDUCTOR_HOST_ROOT)" status

branch-policy-audit:  ## Check every branch-policy rule decidable without a push; exits 1 on violations
	"$(FORGE)" branch-policy --repo "$(CONDUCTOR_HOST_ROOT)" audit

# Explicit operator-controlled private recovery parent; dry runs need no parent.
CHECKOUT_SYNC_RECOVERY_DIR ?=
CHECKOUT_SYNC_ARGS ?=
checkout-sync:  ## Native guarded dirty-layer fast-forward (set CHECKOUT_SYNC_RECOVERY_DIR)
	"$(FORGE)" checkout-sync --repo "$(CONDUCTOR_HOST_ROOT)" \
		$(if $(CHECKOUT_SYNC_RECOVERY_DIR),--recovery-dir "$(CHECKOUT_SYNC_RECOVERY_DIR)") $(CHECKOUT_SYNC_ARGS)

dead-tests:  ## Native advisory Python import-closure and unreferenced-source report
	"$(FORGE)" analyze --root "$(CONDUCTOR_HOST_ROOT)" --max-bytes 4194304 tests \
		--json-out "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)/dead_tests.json"

# Test execution is explicit; an absent guest interpreter/toolchain fails visibly.
GRAPH_TEST_ARGS ?=
test-graph:  ## Advisory graph-selected tests; set GRAPH_TEST_ARGS for explicit guest prerequisites
	"$(FORGE)" graph-test-select --repo "$(CONDUCTOR_HOST_ROOT)" --run $(GRAPH_TEST_ARGS)

# ── Journals and bundles ──────────────────────────────────────────────────

CODEX_JOURNAL_NOTE ?=
CODEX_JOURNAL_TEST ?=
CODEX_JOURNAL_PATHS ?=
CODEX_JOURNAL_MAX_STATUS ?= 80
CODEX_JOURNAL_OUT_DIR ?= tasks/codex_journal
NOTEBOOKLM_OUT ?= tasks/notebooklm/codex_context_bundle.md
NOTEBOOKLM_RESEARCH_OUT ?= tasks/notebooklm/research_briefing_bundle.md

codex-journal:  ## Append an Obsidian-compatible Codex journal entry
	$(PYTHON) -m conductor.codex_journal --note "$(CODEX_JOURNAL_NOTE)" \
		$(if $(CODEX_JOURNAL_TEST),--test "$(CODEX_JOURNAL_TEST)",) \
		$(foreach path,$(CODEX_JOURNAL_PATHS),--path "$(path)") \
		--out-dir "$(CONDUCTOR_HOST_ROOT)/$(CODEX_JOURNAL_OUT_DIR)" \
		--max-status "$(CODEX_JOURNAL_MAX_STATUS)"

notebooklm-bundle:  ## Build a curated Markdown bundle for manual NotebookLM upload
	$(PYTHON) -m conductor.notebooklm_bundle --out "$(CONDUCTOR_HOST_ROOT)/$(NOTEBOOKLM_OUT)" --no-vault

notebooklm-research-bundle:  ## Build a research-focused NotebookLM upload bundle (includes CodexVault notes when present)
	$(PYTHON) -m conductor.notebooklm_bundle --out "$(CONDUCTOR_HOST_ROOT)/$(NOTEBOOKLM_RESEARCH_OUT)"

# ── Native crates: per-crate cargo test ──────────────────────────────────
# In Forge's own Makefile, `install` syncs all three native runtimes and compiles
# the test targets; `native` forces their reinstall. These per-crate targets
# remain available to a host including conductor.mk.

test-conductor-native:  ## cargo test the conductor-native crate
	cd "$(CONDUCTOR_HOST_ROOT)/native/conductor-native" && cargo test

test-slop-core:  ## cargo test the slop-core crate
	cd "$(CONDUCTOR_HOST_ROOT)/native/slop-core" && cargo test

test-forge:  ## cargo test the forge crate (native hook launcher, step 1 of the Rust hook port)
	cd "$(CONDUCTOR_HOST_ROOT)/native/forge" && cargo test

# Standalone host install into .tools/bin; Forge developers use `make install`
# for the uv-managed .venv/bin/forge and complete precompiled test targets.
forge-build:  ## Build forge and install it to .tools/bin for a host project
	cd "$(CONDUCTOR_HOST_ROOT)/native/forge" && \
		CUDA_VISIBLE_DEVICES= CARGO_BUILD_JOBS=2 cargo install --path . --locked \
			--jobs 2 --root "$(CONDUCTOR_HOST_ROOT)/.tools" --force
