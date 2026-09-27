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
.DEFAULT_GOAL := help

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

.PHONY: install test native gate candidate-review \
	mutation-plan mutation-generate mutation-engine-run mutation-evidence \
	mutation-canary mutation-coverage baselines baseline-jscpd baseline-pmd \
	baseline-complexity baseline-vulture help

INSTALL_REINSTALL ?=
INSTALL_SYNC_ARGS ?=

install:  ## Sync the developer venv and precompile all native test targets
	@set -eu; \
	export CUDA_VISIBLE_DEVICES= CARGO_BUILD_JOBS=2 UV_CONCURRENT_BUILDS=1; \
	$(UV) sync --frozen --extra test $(INSTALL_SYNC_ARGS) $(INSTALL_REINSTALL); \
	unset PYO3_CONFIG_FILE PYO3_NO_PYTHON; \
	venv=$${UV_PROJECT_ENVIRONMENT:-$(CURDIR)/.venv}; \
	case "$$venv" in /*) ;; *) venv="$(CURDIR)/$$venv" ;; esac; \
	test -x "$$venv/bin/python"; \
	test -x "$$venv/bin/forge"; \
	export PYO3_PYTHON="$$venv/bin/python" FORGE_BIN="$$venv/bin/forge"; \
	cargo test --manifest-path native/conductor-native/Cargo.toml \
		--locked --jobs 2 --all-targets --features python-compat-tests --no-run; \
	cargo build --manifest-path native/conductor-native/Cargo.toml \
		--locked --jobs 2 --bins --features python-compat-tests; \
	cargo test --manifest-path native/slop-core/Cargo.toml \
		--locked --jobs 2 --all-targets --no-run; \
	cargo test --manifest-path native/forge/Cargo.toml \
		--locked --jobs 2 --all-targets --no-run; \
	target_dir=$${CARGO_TARGET_DIR:-native/forge/target}; \
	if [ -n "$${CARGO_BUILD_TARGET:-}" ]; then target_dir="$$target_dir/$$CARGO_BUILD_TARGET"; fi; \
	fixture_dir="$$target_dir/debug/prebuilt-fixtures"; \
	mkdir -p "$$fixture_dir"; \
	rm -f "$$fixture_dir/task_worker.sha256"; \
	sha256sum native/forge/tests/support/task_worker.rs \
		> "$$fixture_dir/task_worker.inputs"; \
	rustc --edition=2021 --crate-name=forge_task_test_worker \
		-C codegen-units=1 -C debuginfo=0 \
		native/forge/tests/support/task_worker.rs \
		-o "$$fixture_dir/task-worker"; \
	sha256sum -c "$$fixture_dir/task_worker.inputs"; \
	sha256sum "$$fixture_dir/task-worker" >> "$$fixture_dir/task_worker.inputs"; \
	awk '{print $$1}' "$$fixture_dir/task_worker.inputs" \
		> "$$fixture_dir/task_worker.sha256.tmp"; \
	mv "$$fixture_dir/task_worker.sha256.tmp" "$$fixture_dir/task_worker.sha256"; \
	rm -f "$$fixture_dir/stub_dispatch.sha256"; \
	sha256sum native/forge/tests/fixtures/stub_dispatch/Cargo.toml \
		native/forge/tests/fixtures/stub_dispatch/Cargo.lock \
		native/forge/tests/fixtures/stub_dispatch/src/main.rs \
		> "$$fixture_dir/stub_dispatch.inputs"; \
	cargo build --manifest-path native/forge/tests/fixtures/stub_dispatch/Cargo.toml \
		--locked --jobs 2 --target-dir "$$fixture_dir/stub-dispatch-target"; \
	stub_profile=$${CARGO_BUILD_TARGET:+$$CARGO_BUILD_TARGET/}debug; \
	sha256sum -c "$$fixture_dir/stub_dispatch.inputs"; \
	sha256sum \
		"$$fixture_dir/stub-dispatch-target/$$stub_profile/forge-test-stub-dispatch" \
		>> "$$fixture_dir/stub_dispatch.inputs"; \
	awk '{print $$1}' "$$fixture_dir/stub_dispatch.inputs" \
		> "$$fixture_dir/stub_dispatch.sha256.tmp"; \
	mv "$$fixture_dir/stub_dispatch.sha256.tmp" "$$fixture_dir/stub_dispatch.sha256"

test:  ## Run the conductor test suite
	$(UV) run python -m pytest $(PYTEST_TARGET) \
		$(if $(PYTEST_TIMEOUT),--timeout $(PYTEST_TIMEOUT)) $(PYTEST_ARGS)

native:  ## Force-rebuild all three native runtimes, then precompile test targets
	$(MAKE) install INSTALL_REINSTALL='--reinstall-package forge-cli --reinstall-package conductor-native --reinstall-package slop-core'

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
	@# REPO_ROOT is the true host root; baseline and scan path are src-relative.
	$(UV) run python -m conductor.radon_complexity refresh-baseline \
		--baseline src/conductor/radon_complexity_baseline.json --path src

baseline-vulture:  ## Record an empty justified-findings allowlist for vulture (see script docstring)
	$(UV) run python -m conductor.candidate_review.vulture_baseline_init \
		--baseline src/conductor/vulture_baseline.json \
		--expires "$(BASELINE_EXPIRES)" \
		src

help:  ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-22s\033[0m %s\n", $$1, $$2}'
