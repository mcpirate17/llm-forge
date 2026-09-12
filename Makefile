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
MUTATION_PATHS ?=
MUTATION_GENERATE_ARGS ?=
MUTATION_ENGINE_ARGS ?=

.PHONY: test native gate candidate-review \
	mutation-plan mutation-generate mutation-engine-run mutation-evidence \
	mutation-coverage help

test:  ## Run the conductor test suite
	$(UV) run python -m pytest $(PYTEST_TARGET) --timeout $(PYTEST_TIMEOUT) $(PYTEST_ARGS)

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

mutation-engine-run:  ## Run a generated campaign through its engine in a disposable snapshot
	@test -n "$(MUTATION_CAMPAIGN)" || { echo "Set MUTATION_CAMPAIGN=campaigns/<id>.json"; exit 2; }
	$(UV) run python -m conductor.mutation_engine_generated run "$(MUTATION_CAMPAIGN)" \
		--allow-mutations --base "$(MUTATION_BASE)" $(MUTATION_ENGINE_ARGS)

mutation-evidence:  ## Verify PASS receipts for MUTATION_PATHS, or for git-changed tests
	@if [ -n "$(MUTATION_PATHS)" ]; then \
		$(UV) run python -m conductor.mutation_testing verify-evidence $(MUTATION_PATHS); \
	else \
		$(UV) run python -m conductor.mutation_coverage changed; \
	fi

mutation-coverage:  ## Read-only inventory of test evidence; never executes mutants
	$(UV) run python -m conductor.mutation_coverage coverage

help:  ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-22s\033[0m %s\n", $$1, $$2}'
