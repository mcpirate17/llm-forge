# llm-forge Makefile — the mechanical quality routine.
#
# Every conductor invocation is `$(UV) run python -m conductor.<module>`: a script
# path resolves against whatever interpreter the caller's shell happens to carry,
# which is how the monorepo spent weeks testing against a second, unmaintained venv.
#
# Host data paths are NOT spelled out here. `[tool.conductor]` in pyproject.toml
# names `candidate_policy.toml` and `campaigns/registry.json`, and every CLI
# resolves them through `conductor.project_paths`. Repeating them on the command
# line would reintroduce exactly the hardcoding that module exists to remove.

# Drop the inherited VIRTUAL_ENV so uv stops warning on every invocation.
UV ?= env -u VIRTUAL_ENV uv
.SILENT:

# The platform's user-facing commands (governance, slop, complexity, dupes,
# mutation maintenance, worktree/graph hygiene) live in conductor.mk so a host
# project can `include` them too. Keep new platform targets there, not here.
include conductor.mk

PYTEST_TARGET ?= src/conductor
PYTEST_TIMEOUT ?= 600
PYTEST_ARGS ?=

GATE_REF ?= HEAD
GATE_BASE ?= origin/main
GATE_PROFILE ?= full

REVIEW_PROFILE ?= full
REVIEW_ARGS ?=

MUTATION_LANGUAGE ?= python
MUTATION_BASE ?= origin/main
MUTATION_CAMPAIGN ?=
# conductor.mutation_receipt_build still defaults to the monorepo's
# research/reports/mutation_testing/. campaigns/registry.json declares
# campaigns/receipts as this repository's receipt directory, and a receipt written
# anywhere else is invisible to evidence verification, so name it here.
# `?=` is recursively expanded, so neither of these runs until the recipe needs it.
MUTATION_RECEIPT ?= $(shell $(UV) run python -c \
  'from conductor.project_paths import receipts_relative; print(receipts_relative("."))')
RUN_STAMP ?= $(shell date -u +%Y%m%dT%H%M%SZ)
MUTATION_PATHS ?=
MUTATION_GENERATE_ARGS ?=
MUTATION_ENGINE_ARGS ?=

# candidate_policy.toml's own `baseline_expires` gates every baseline at once
# (policy.py refuses the whole review once it lapses), so a freshly generated
# baseline expires alongside it rather than drifting out of sync on its own.
BASELINE_EXPIRES ?= $(shell $(UV) run python -c \
  'import tomllib; from conductor.project_paths import project_paths; \
   print(tomllib.loads(project_paths(".").policy_path.read_text(encoding="utf-8"))["baseline_expires"])')

.PHONY: test native gate candidate-review \
	mutation-plan mutation-generate mutation-engine-run mutation-evidence \
	mutation-canary mutation-coverage baselines baseline-jscpd baseline-pmd \
	baseline-complexity baseline-vulture help

test:  ## Run the conductor test suite
	@# --timeout needs pytest-timeout, which nothing in pyproject declares yet, so
	@# this target fails loud with "unrecognized arguments: --timeout 600" until the
	@# test extra carries it. Deliberate: a suite with no per-test timeout is how a
	@# hung test becomes a hung CI job. PYTEST_TIMEOUT= disables the flag if you
	@# need the suite before the dependency lands.
	$(UV) run python -m pytest $(PYTEST_TARGET) \
		$(if $(PYTEST_TIMEOUT),--timeout $(PYTEST_TIMEOUT)) $(PYTEST_ARGS)

native:  ## Rebuild both Rust crates and install them into the venv
	@# Both crates are path sources in [tool.uv.sources]; `uv sync --reinstall-package`
	@# is what actually rebuilds them. A plain `uv sync` keeps the cached wheel and
	@# silently tests yesterday's Rust.
	$(UV) sync --extra test \
		--reinstall-package conductor-native \
		--reinstall-package slop-core

gate:  ## The governance gate: config self-check plus the CI-identical review
	$(UV) run python -m conductor.gate \
		--ref "$(GATE_REF)" \
		--base "$(GATE_BASE)" \
		--profile "$(GATE_PROFILE)"

candidate-review:  ## Run the candidate-index review over the working tree
	$(UV) run python -m conductor.candidate_review.cli review \
		--surface manual --candidate index --profile "$(REVIEW_PROFILE)" $(REVIEW_ARGS)

mutation-plan:  ## Show the campaigns mutation-generate would write for THIS BRANCH
	$(UV) run python -m conductor.mutation_campaign_generate plan \
		$(MUTATION_LANGUAGE) --base "$(MUTATION_BASE)" $(MUTATION_GENERATE_ARGS)

mutation-generate:  ## Write campaigns for THIS BRANCH's changed files (nothing else)
	$(UV) run python -m conductor.mutation_campaign_generate write \
		$(MUTATION_LANGUAGE) --base "$(MUTATION_BASE)" $(MUTATION_GENERATE_ARGS)

# Ratchet iterations write their receipts under `.iterations/` (gitignored):
# every run of a campaign grows a ~350 KB receipt and the tracked tree only
# ever needs the final one, so a loop that commits each iteration was paying
# for all of them in every clone. `mutation-receipt-promote` copies the newest
# iteration receipt into the tracked directory when the loop settles.
MUTATION_RECEIPT_ITERATIONS ?= $(MUTATION_RECEIPT)/.iterations

mutation-engine-run:  ## Run a generated campaign through its engine in a disposable snapshot
	@test -n "$(MUTATION_CAMPAIGN)" || { echo "Set MUTATION_CAMPAIGN=campaigns/<id>.json"; exit 2; }
	@mkdir -p "$(MUTATION_RECEIPT_ITERATIONS)"
	$(UV) run python -m conductor.mutation_engine_generated run "$(MUTATION_CAMPAIGN)" \
		--allow-mutations --base "$(MUTATION_BASE)" \
		$(if $(MUTATION_RECEIPT),--receipt "$(MUTATION_RECEIPT_ITERATIONS)/$(notdir $(basename $(MUTATION_CAMPAIGN)))_$(RUN_STAMP).json") \
		$(MUTATION_ENGINE_ARGS)
	@echo "iteration receipt: $(MUTATION_RECEIPT_ITERATIONS)/$(notdir $(basename $(MUTATION_CAMPAIGN)))_$(RUN_STAMP).json (gitignored)"
	@echo "tracked receipts stay at $(MUTATION_RECEIPT)/ -- promote with 'make mutation-receipt-promote'"

mutation-receipt-promote:  ## Copy the newest .iterations receipt of MUTATION_CAMPAIGN into the tracked receipt dir
	@test -n "$(MUTATION_CAMPAIGN)" || { echo "Set MUTATION_CAMPAIGN=campaigns/<id>.json"; exit 2; }
	@id=$(notdir $(basename $(MUTATION_CAMPAIGN))); \
	newest=$$(ls -t "$(MUTATION_RECEIPT_ITERATIONS)/$${id}_"*.json 2>/dev/null | head -1); \
	test -n "$$newest" || { echo "no iteration receipts for $$id under $(MUTATION_RECEIPT_ITERATIONS)/"; exit 2; }; \
	cp "$$newest" "$(MUTATION_RECEIPT)/"; \
	echo "promoted $$newest -> $(MUTATION_RECEIPT)/$$(basename $$newest) -- git add it with the PR"

mutation-evidence:  ## Verify PASS receipts for MUTATION_PATHS, or changed-since-MUTATION_BASE tests
	@if [ -n "$(MUTATION_PATHS)" ]; then \
		$(UV) run python -m conductor.mutation_testing verify-evidence $(MUTATION_PATHS); \
	else \
		$(UV) run python -m conductor.mutation_coverage changed --base "$(MUTATION_BASE)"; \
	fi

mutation-canary:  ## Repo-wide receipt decode canary; fails only on unreadable receipts
	$(UV) run python -m conductor.mutation_coverage canary

mutation-coverage:  ## Read-only inventory of test evidence; never executes mutants
	$(UV) run python -m conductor.mutation_coverage coverage

baselines: baseline-jscpd baseline-pmd baseline-complexity baseline-vulture  ## Regenerate all four candidate-review baselines from real tool output

baseline-jscpd:  ## Record current jscpd duplicate pairs as the baseline (needs jscpd on PATH)
	$(UV) run python -m conductor.run_duplicate_audit --tool jscpd --save-baseline

baseline-pmd:  ## Record current PMD-CPD duplicate pairs as the baseline (needs pmd on PATH; see README)
	@command -v pmd >/dev/null 2>&1 || { echo "pmd not on PATH -- see README.md for the pinned release"; exit 2; }
	$(UV) run python -m conductor.run_duplicate_audit --tool pmd-python --save-baseline

baseline-complexity:  ## Record current complexity blocks as the ratchet baseline
	@# --baseline is resolved against radon_complexity.py's own REPO_ROOT (this
	@# package's src/ directory), not the repo root -- see [checks.complexity].
	$(UV) run python -m conductor.radon_complexity refresh-baseline \
		--baseline conductor/radon_complexity_baseline.json --path .

baseline-vulture:  ## Record an empty justified-findings allowlist for vulture (see script docstring)
	$(UV) run python -m conductor.candidate_review.vulture_baseline_init \
		--baseline src/conductor/vulture_baseline.json \
		--expires "$(BASELINE_EXPIRES)" \
		src

help:  ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-22s\033[0m %s\n", $$1, $$2}'
