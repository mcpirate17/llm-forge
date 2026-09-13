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
# recipes. Every recipe invokes `$(PYTHON) -m conductor.<module>`, never a script
# path, so it runs under whatever interpreter installed conductor-tooling rather
# than an agent's ambient PATH.

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
CONDUCTOR_GATE_FINDINGS_DIR ?= $(CONDUCTOR_REPORTS_DIR)/gate_findings

.PHONY: governance-check governance-audit governance-preflight governance-fix \
	governance-commit governance-claim governance-release-claim governance-claims \
	governance-claims-json governance-close-session \
	slop-gate slop-backlog slop-imports slop-probe \
	complexity-report complexity-check complexity-refresh-baseline \
	dupes dupes-jscpd dupes-jscpd-check dupes-pmd dupes-pmd-check dupes-pylint \
	dupes-nicad dupes-deep dupes-deep-check \
	mutation-retention mutation-patch-audit mutation-patch-audit-record \
	cost-budget-audit cost-budget-record \
	graph-seed-worktree worktree-reap workspace-hygiene branch-policy \
	branch-policy-audit checkout-sync crg-probe crg-sync crg-check \
	dead-tests test-graph codex-journal notebooklm-bundle notebooklm-research-bundle \
	test-conductor-native test-slop-core test-forge forge-build

# ── Governance: candidate review, claims, sessions ──────────────────────
# All of these wrap conductor.candidate_review.cli / conductor.session_close,
# which already take --repo (default ".") -- passed explicitly here so the
# target works when make is invoked from somewhere other than CONDUCTOR_HOST_ROOT.

governance-check:  ## Run the exact candidate-index fast review
	$(PYTHON) -m conductor.candidate_review.cli review --repo "$(CONDUCTOR_HOST_ROOT)" \
		--surface manual --candidate index --profile fast

governance-audit:  ## Run the exact candidate-index full review and emit CI-format artifacts
	@mkdir -p "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)"
	$(PYTHON) -m conductor.candidate_review.cli review --repo "$(CONDUCTOR_HOST_ROOT)" \
		--surface manual --candidate index --profile full \
		--json-out "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)/candidate_review.json" \
		--sarif-out "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)/candidate_review.sarif" \
		--junit-out "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)/candidate_review.junit.xml"

PREFLIGHT_REF ?= HEAD
PREFLIGHT_BASE ?= origin/main

governance-preflight:  ## Reproduce the CI review locally for PREFLIGHT_REF (default HEAD)
	@mkdir -p "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)"
	$(PYTHON) -m conductor.candidate_review.cli review --repo "$(CONDUCTOR_HOST_ROOT)" \
		--surface ci --candidate range --target-ref "$(PREFLIGHT_REF)" \
		--base-ref "$(PREFLIGHT_BASE)" --profile full \
		--json-out "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)/preflight.json"
	@echo "unexcepted blocking findings:"
	@$(PYTHON) -c "import json,collections; \
		f=[x for x in json.load(open('$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)/preflight.json'))['findings'] \
		   if x['severity'] in ('critical','high') and not x.get('exception_id')]; \
		print(' total', len(f)); \
		[print(f'  {v:3d}  {k}') for k,v in collections.Counter(x['check_id']+'/'+x['rule_id'] for x in f).most_common()]"

FIX_PATHS ?=
COMMIT_ARGS ?=
CLAIM_PATHS ?=
CLAIM_JUSTIFICATION ?=
CLAIM_HOURS ?=
CLAIM_ID ?=
CLAIM_PATH ?=
OWNER ?=
SESSION_TITLE ?=
SESSION_BODY ?=

governance-fix:  ## Explicitly fix only FIX_PATHS='path ...' outside commit hooks
	@test -n "$(FIX_PATHS)" || { echo "Set FIX_PATHS to explicit paths"; exit 2; }
	$(PYTHON) -m conductor.candidate_review.cli fix --repo "$(CONDUCTOR_HOST_ROOT)" $(FIX_PATHS)

governance-commit:  ## Hold the commit mutex; pass COMMIT_ARGS='-m ... path ...'
	$(PYTHON) -m conductor.candidate_review.cli commit --repo "$(CONDUCTOR_HOST_ROOT)" -- $(COMMIT_ARGS)

governance-claim:  ## Claim exact paths; set CLAIM_PATHS and CLAIM_JUSTIFICATION (OWNER defaults to this lane)
	@test -n "$(CLAIM_PATHS)" -a -n "$(CLAIM_JUSTIFICATION)" || { echo "Set CLAIM_PATHS and CLAIM_JUSTIFICATION"; exit 2; }
	$(PYTHON) -m conductor.candidate_review.cli claim --repo "$(CONDUCTOR_HOST_ROOT)" \
		$(if $(OWNER),--owner "$(OWNER)") --justification "$(CLAIM_JUSTIFICATION)" \
		$(if $(CLAIM_HOURS),--hours "$(CLAIM_HOURS)") \
		$(CLAIM_PATHS)

governance-release-claim:  ## Release CLAIM_ID as OWNER
	@test -n "$(OWNER)" -a -n "$(CLAIM_ID)" || { echo "Set OWNER and CLAIM_ID"; exit 2; }
	$(PYTHON) -m conductor.candidate_review.cli release-claim --repo "$(CONDUCTOR_HOST_ROOT)" \
		--owner "$(OWNER)" "$(CLAIM_ID)"

governance-claims:  ## Show active claims, one line each (CLAIM_PATH="a b" filters by overlap)
	$(PYTHON) -m conductor.candidate_review.cli claims --repo "$(CONDUCTOR_HOST_ROOT)" --compact \
		$(foreach p,$(CLAIM_PATH),--path $(p))

governance-claims-json:  ## Show the full machine-readable claim store (active and expired)
	$(PYTHON) -m conductor.candidate_review.cli claims --repo "$(CONDUCTOR_HOST_ROOT)"

governance-close-session:  ## Atomically close session: release claims, append handoff, update state
	@test -n "$(OWNER)" || { echo "Set OWNER='<agent>' (optional: SESSION_TITLE, SESSION_BODY, CLAIM_ID)"; exit 2; }
	$(PYTHON) -m conductor.session_close --repo "$(CONDUCTOR_HOST_ROOT)" \
		--owner "$(OWNER)" \
		$(if $(SESSION_TITLE),--title "$(SESSION_TITLE)",) \
		$(if $(SESSION_BODY),--body "$(SESSION_BODY)",) \
		$(if $(CLAIM_ID),--claim-id "$(CLAIM_ID)",)

# ── Slop: tier-1 equivalence probing ────────────────────────────────────

SLOP_BASE ?= origin/main
SLOP_MODULES ?=
SLOP_JOBS ?=
SLOP_ENFORCE ?=
SLOP_MODULE ?=
SLOP_TESTS ?=

slop-gate:  ## Tier-1 equivalence probe over changed modules; reports only (add SLOP_ENFORCE=--enforce to fail)
	$(PYTHON) -m conductor.slop_gate --base "$(SLOP_BASE)" --root "$(CONDUCTOR_HOST_ROOT)" $(SLOP_ENFORCE)

slop-backlog:  ## Sweep SLOP_MODULES (default: changed) into the ranked backlog + ledger
	@mkdir -p "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_REPORTS_DIR)"
	$(PYTHON) -m conductor.slop_gate \
		--base "$(SLOP_BASE)" --root "$(CONDUCTOR_HOST_ROOT)" \
		$(if $(SLOP_MODULES),$(foreach m,$(SLOP_MODULES),--module $(m))) \
		$(if $(SLOP_JOBS),--jobs $(SLOP_JOBS)) \
		--json "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_REPORTS_DIR)/slop_sweep.json"
	@# Folds in every summary a prior `gate` run left under CONDUCTOR_GATE_FINDINGS_DIR.
	@# Globbed in the recipe, not with $$(wildcard): that expands when the Makefile is
	@# parsed, so the list folded in and the list deleted could differ.
	@set -e; folded=$$(ls "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_GATE_FINDINGS_DIR)"/*.json 2>/dev/null || true); \
	$(PYTHON) -m conductor.slop_ledger \
		"$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_REPORTS_DIR)/slop_sweep.json" $$folded \
		--ledger "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_PACKAGE_ROOT)/slop_ledger.json" \
		--report "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_REPORTS_DIR)/slop_backlog.md"; \
	rm -f $$folded

slop-imports:  ## Ablate the silenced (noqa F401) imports of SLOP_MODULE against its drivers
	@test -n "$(SLOP_MODULE)" || { echo "Set SLOP_MODULE=path.py"; exit 2; }
	$(PYTHON) -m conductor.import_ablation "$(SLOP_MODULE)" --root "$(CONDUCTOR_HOST_ROOT)"

slop-probe:  ## Probe one module against its driver tests: SLOP_MODULE=path.py SLOP_TESTS='tests...'
	@test -n "$(SLOP_MODULE)" -a -n "$(SLOP_TESTS)" || { echo "Set SLOP_MODULE and SLOP_TESTS"; exit 2; }
	$(PYTHON) -m conductor.equivalence_probe "$(SLOP_MODULE)" $(SLOP_TESTS)

# ── Complexity ratchet ───────────────────────────────────────────────────
# conductor.radon_complexity resolves its own paths and baseline relative to the
# conductor package's own tree root, not CONDUCTOR_HOST_ROOT -- correct only when
# this Makefile is the one shipped beside src/conductor (this repo). A host
# consuming conductor-tooling as a dependency should call the module directly
# with explicit --path/--baseline instead of this target.

complexity-report:  ## Report production Python cyclomatic complexity
	$(PYTHON) -m conductor.radon_complexity report

complexity-check:  ## Fail on new production Python D-F complexity blocks
	$(PYTHON) -m conductor.radon_complexity check

complexity-refresh-baseline:  ## Refresh the legacy D-F complexity baseline
	$(PYTHON) -m conductor.radon_complexity refresh-baseline

# ── Duplicate code detectors ─────────────────────────────────────────────
# conductor.run_duplicate_audit scans conductor.duplicate_audit_config.DEFAULT_SOURCE_DIRS
# (research/aria_core/aria_designer/component_fab/conductor), a monorepo directory
# list with no CLI override and no src/conductor entry. In this repo's src layout
# every one of those names is absent at the root, so these targets run without
# error but scan zero files until that module gains a src-aware default or a
# --source flag. Ported anyway: the plumbing (tool selection, --check exit code)
# is correct, and a host laid out like the monorepo gets real results today.

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

mutation-retention:  ## Report (or with MUTATION_RETENTION_APPLY=1, delete) uncitable receipts
	$(PYTHON) -m conductor.mutation_retention --repo-root "$(CONDUCTOR_HOST_ROOT)" \
		$(if $(MUTATION_RETENTION_APPLY),--apply,) $(MUTATION_RETENTION_ARGS)

mutation-patch-audit:  ## Report campaigns that cannot be applied, re-run, or vouched for
	$(PYTHON) -m conductor.mutation_patch_audit \
		--registry "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_MUTATION_REGISTRY)" $(MUTATION_AUDIT_ARGS)

# Records today's findings as the new debt. Run only after repairing campaigns --
# recording a grown baseline is how the ratchet is defeated.
mutation-patch-audit-record:  ## Re-record the reproducibility baseline after repairs
	$(PYTHON) -m conductor.mutation_patch_audit \
		--registry "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_MUTATION_REGISTRY)" \
		--summary --write-baseline

# ── Cost ledger budget ratchet (docs/design/cost_ledger.md section 4) ───
# Shells to `forge ledger audit` (native/forge/src/ledger/audit.rs); the Python
# side only resolves the binary and maps its verdict to a gate phase / exit
# code -- see src/conductor/cost_budget_audit.py.

COST_BUDGET_ARGS ?=

cost-budget-audit:  ## Check hook latency, resend bytes and tokens/landed-PR against the recorded baseline
	$(PYTHON) -m conductor.cost_budget_audit --repo-root "$(CONDUCTOR_HOST_ROOT)" $(COST_BUDGET_ARGS)

# Records this window's metrics as the new baseline. Run only after a deliberate
# improvement or an accepted regression -- recording a grown baseline is how the
# ratchet is defeated.
cost-budget-record:  ## Re-record the cost-ledger budget baseline for the current window
	$(PYTHON) -m conductor.cost_budget_audit --repo-root "$(CONDUCTOR_HOST_ROOT)" \
		--record $(COST_BUDGET_ARGS)

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
CRG_EXPECT_TOOLS ?=

crg-probe:  ## Handshake with the code-review-graph MCP server declared in CRG_REPO's .mcp.json
	$(PYTHON) -m conductor.crg_mcp_probe --repo "$(CRG_REPO)" \
		$(if $(CRG_EXPECT_TOOLS),--expect-tools $(CRG_EXPECT_TOOLS),)

crg-sync:  ## Install this tree's native crates into the code-review-graph MCP interpreter
	$(PYTHON) -m conductor.crg_venv_sync --repo "$(CRG_REPO)"

crg-check:  ## Report (never repair) drift between this tree and the MCP interpreter
	$(PYTHON) -m conductor.crg_venv_sync --repo "$(CRG_REPO)" --check

# ── Workspace / branch hygiene ───────────────────────────────────────────

workspace-hygiene:  ## Read-only: what is safe to delete (branches, worktrees, claims) and what breaks a clean clone
	$(PYTHON) -m conductor.workspace_hygiene

branch-policy:  ## Branch binding status: live claim/branch pairs, age, PR number, staleness
	$(PYTHON) -m conductor.branch_policy status

branch-policy-audit:  ## Check every branch-policy rule decidable without a push; exits 1 on violations
	$(PYTHON) -m conductor.branch_policy audit

checkout-sync:  ## Snapshot the dirty tree, then fast-forward this checkout onto the remote line
	$(PYTHON) -m conductor.checkout_sync --repo "$(CONDUCTOR_HOST_ROOT)"

dead-tests:  ## Import-closure dead-test finder: broken tests, test-only targets, unreferenced sources
	@mkdir -p "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)"
	$(PYTHON) -m conductor.dead_tests --root "$(CONDUCTOR_HOST_ROOT)" \
		--json-out "$(CONDUCTOR_HOST_ROOT)/$(CONDUCTOR_AUDIT_DIR)/dead_tests.json"

test-graph:  ## Run pytest on tests selected via code-review-graph for changed code
	$(PYTHON) -m conductor.graph_test_select --repo "$(CONDUCTOR_HOST_ROOT)" --run

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
# `native` (in Makefile) rebuilds and installs both crates via `uv sync
# --reinstall-package`; these run each crate's own Rust test suite without
# touching the installed extension.

test-conductor-native:  ## cargo test the conductor-native crate
	cd "$(CONDUCTOR_HOST_ROOT)/native/conductor-native" && cargo test

test-slop-core:  ## cargo test the slop-core crate
	cd "$(CONDUCTOR_HOST_ROOT)/native/slop-core" && cargo test

test-forge:  ## cargo test the forge crate (native hook launcher, step 1 of the Rust hook port)
	cd "$(CONDUCTOR_HOST_ROOT)/native/forge" && cargo test

# Installs to .tools/bin/forge, where project_init.resolve_forge_binary looks for
# it: the next `conductor init`/`conductor.bootstrap` run then wires
# .claude/settings.json hooks to `forge hook <Event>` instead of the Python
# launcher script. No hook logic is native yet (step 1 of the port) -- forge just
# delegates whole to the same Python dispatcher, so this is safe to build early.
forge-build:  ## Build forge and install it to .tools/bin
	cd "$(CONDUCTOR_HOST_ROOT)/native/forge" && \
		cargo install --path . --root "$(CONDUCTOR_HOST_ROOT)/.tools" --force
