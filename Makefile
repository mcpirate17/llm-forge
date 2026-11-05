# llm-forge Makefile — the mechanical quality routine.
#
# Build, install and the default test suite use native Cargo artifacts. Remaining
# host governance entry points below are being migrated independently.
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

GATE_REF ?= HEAD
GATE_BASE ?= origin/main
GATE_PROFILE ?= full
GATE_ARGS ?=
LOCAL_CHECK_ARGS ?= --all

REVIEW_PROFILE ?= full
REVIEW_ARGS ?=

MUTATION_LANGUAGE ?= python
MUTATION_BASE ?= origin/main
MUTATION_CAMPAIGN ?=
# Empty overrides select the host's native project-path configuration at execution.
MUTATION_RECEIPT ?=
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

.PHONY: build install test test-install test-contract-registry native gate candidate-review local-preview local-check local-verify \
	mutation-plan mutation-generate mutation-engine-run mutation-evidence \
	mutation-canary mutation-coverage baselines baseline-jscpd baseline-pmd \
	baseline-complexity baseline-vulture help

# Default installation is isolated from shared user environments and binaries.
# An operator may select a different explicit prefix via INSTALL_ROOT.
INSTALL_ROOT ?= $(CURDIR)/target/install
FORGE_TARGET_ROOT = $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),$(CURDIR)/native/forge/target)
# Keep the registry feature matrix out of the default cdylib/rlib output paths.
CONTRACT_REGISTRY_TARGET_ROOT = $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR),$(CURDIR)/native/conductor-native/target)/contract-registry
CARGO_BUILD_JOBS ?= 2
RUST_TEST_THREADS ?= 2
NATIVE_ENV = CUDA_VISIBLE_DEVICES= CARGO_BUILD_JOBS="$(CARGO_BUILD_JOBS)" RUST_TEST_THREADS="$(RUST_TEST_THREADS)" RUSTUP_TOOLCHAIN=1.98.0 PYO3_PYTHON=/nonexistent/forbidden-python

build:  ## Build the release executable and its native core libraries
	$(NATIVE_ENV) cargo +1.98.0 build --release --locked --jobs "$(CARGO_BUILD_JOBS)" --manifest-path native/forge/Cargo.toml --bin forge --target-dir "$(FORGE_TARGET_ROOT)"

install:  ## Install the native executable into INSTALL_ROOT (default target/install)
	$(NATIVE_ENV) cargo +1.98.0 install --path native/forge-cli --bin forge --locked --jobs "$(CARGO_BUILD_JOBS)" --force --root "$(INSTALL_ROOT)" --target-dir "$(FORGE_TARGET_ROOT)"

test-install: build
	$(NATIVE_ENV) cargo +1.98.0 install --path native/forge-cli --bin forge --locked --jobs "$(CARGO_BUILD_JOBS)" --force --root "$(FORGE_TARGET_ROOT)/test-install" --target-dir "$(FORGE_TARGET_ROOT)"

test: test-install  ## Run all default native release tests without interpreter discovery
	$(NATIVE_ENV) cargo +1.98.0 test --release --locked --jobs "$(CARGO_BUILD_JOBS)" --manifest-path native/conductor-native/Cargo.toml --features bundled-sqlite
	$(MAKE) test-contract-registry
	$(NATIVE_ENV) cargo +1.98.0 test --release --locked --jobs "$(CARGO_BUILD_JOBS)" --manifest-path native/slop-core/Cargo.toml
	$(NATIVE_ENV) FORGE_TEST_BIN="$$(cd "$(FORGE_TARGET_ROOT)/test-install" && pwd -P)/bin/forge" cargo +1.98.0 test --release --locked --jobs "$(CARGO_BUILD_JOBS)" --manifest-path native/forge/Cargo.toml --target-dir "$(FORGE_TARGET_ROOT)"

# This native feature matrix retains all 33 discovery cases and the library's
# Slop-consumer registry assertion; it never enables python/python-compat-tests.
test-contract-registry:  ## Check embedded contract discovery without Python
	$(NATIVE_ENV) cargo +1.98.0 test --release --offline --locked --jobs "$(CARGO_BUILD_JOBS)" --manifest-path native/conductor-native/Cargo.toml --features bundled-sqlite,contract-registry --lib --test python_contracts_discovery --target-dir "$(CONTRACT_REGISTRY_TARGET_ROOT)"

native: build  ## Build native Forge; test fixtures compile natively on demand

local-preview:  ## Plan dirty-source tests; LOCAL_PREVIEW_ARGS='--execute' runs isolated targets
	CARGO_BUILD_JOBS="$(CARGO_BUILD_JOBS)" cargo +1.98.0 run --offline --locked --manifest-path native/forge/Cargo.toml -- preview $(LOCAL_PREVIEW_ARGS)

local-check:  ## Check the committed checkout before opening or merging a PR (full by default)
	CARGO_BUILD_JOBS="$(CARGO_BUILD_JOBS)" cargo +1.98.0 run --offline --locked --manifest-path native/forge/Cargo.toml -- check $(LOCAL_CHECK_ARGS)

local-verify:  ## Revalidate the latest source-bound full local-check receipt
	CARGO_BUILD_JOBS="$(CARGO_BUILD_JOBS)" cargo +1.98.0 run --offline --locked --manifest-path native/forge/Cargo.toml -- verify --require-all

gate:  ## The governance gate: config self-check plus the CI-identical review
	@test -n "$(CONDUCTOR_REVIEW_GRANTS)" -a -n "$(CONDUCTOR_MUTATION_GIT)" -a -n "$(CONDUCTOR_MUTATION_GIT_SHA256)" -a -n "$(CONDUCTOR_MUTATION_GIT_BUILD_PROFILE)" -a -n "$(CONDUCTOR_MUTATION_GIT_BUILD_PROFILE_SHA256)" -a -n "$(CONDUCTOR_MUTATION_RUNNER_BUNDLE)" -a -n "$(CONDUCTOR_MUTATION_RUNNER_SHA256)" || { echo "Set operator review grants and pinned Git, Git build profile and mutation runner bundle paths and SHA-256 values"; exit 2; }
	"$(FORGE)" candidate gate --repo "$(CONDUCTOR_HOST_ROOT)" --surface ci --kind range \
		--ref "$(GATE_REF)" --base "$(GATE_BASE)" --profile "$(GATE_PROFILE)" \
		--grants "$(CONDUCTOR_REVIEW_GRANTS)" --git "$(CONDUCTOR_MUTATION_GIT)" --git-sha256 "$(CONDUCTOR_MUTATION_GIT_SHA256)" \
		--git-build-profile "$(CONDUCTOR_MUTATION_GIT_BUILD_PROFILE)" --git-build-profile-sha256 "$(CONDUCTOR_MUTATION_GIT_BUILD_PROFILE_SHA256)" \
		--mutation-runner-bundle "$(CONDUCTOR_MUTATION_RUNNER_BUNDLE)" --mutation-runner-sha256 "$(CONDUCTOR_MUTATION_RUNNER_SHA256)" $(GATE_ARGS)

candidate-review:  ## Run the native index review with explicit operator grants
	@test -n "$(CONDUCTOR_REVIEW_GRANTS)" || { echo "Set CONDUCTOR_REVIEW_GRANTS to an operator-owned grants JSON file"; exit 2; }
	"$(FORGE)" candidate review --repo "$(CONDUCTOR_HOST_ROOT)" \
		--surface manual --kind index --profile "$(REVIEW_PROFILE)" \
		--grants "$(CONDUCTOR_REVIEW_GRANTS)" --execute $(REVIEW_ARGS)

mutation-plan:  ## Show the campaigns mutation-generate would write for THIS BRANCH
	"$(FORGE)" mutation plan \
		$(MUTATION_LANGUAGE) --base "$(MUTATION_BASE)" $(MUTATION_GENERATE_ARGS)

mutation-generate:  ## Write campaigns for THIS BRANCH's changed files (nothing else)
	"$(FORGE)" mutation write \
		$(MUTATION_LANGUAGE) --base "$(MUTATION_BASE)" $(MUTATION_GENERATE_ARGS)

# Ratchet iterations write their receipts under `.iterations/` (gitignored):
# every run of a campaign grows a ~350 KB receipt and the tracked tree only
# ever needs the final one, so a loop that commits each iteration was paying
# for all of them in every clone. `mutation-receipt-promote` copies the newest
# iteration receipt into the tracked directory when the loop settles.
MUTATION_RECEIPT_ITERATIONS ?=

# Expand inside a recipe so native path failures stop the target, never Make parsing.
define mutation-receipt-paths
receipt_root="$(MUTATION_RECEIPT)"; \
if [ -z "$$receipt_root" ]; then \
    receipt_root=$$("$(FORGE)" project --host "$(CONDUCTOR_HOST_ROOT)" path mutation-receipt-root) || exit $$?; \
fi; \
test -n "$$receipt_root" || { echo "empty mutation receipt root" >&2; exit 2; }; \
receipt_iterations="$(MUTATION_RECEIPT_ITERATIONS)"; \
if [ -z "$$receipt_iterations" ]; then receipt_iterations="$$receipt_root/.iterations"; fi
endef

mutation-engine-run:  ## Run a generated campaign through its engine in a disposable snapshot
	@test -n "$(MUTATION_CAMPAIGN)" || { echo "Set MUTATION_CAMPAIGN=campaigns/<id>.json"; exit 2; }
	@$(mutation-receipt-paths); \
	"$(FORGE)" mutation run "$(MUTATION_CAMPAIGN)" \
		--allow-mutations --base "$(MUTATION_BASE)" \
		--receipt "$$receipt_iterations/$(notdir $(basename $(MUTATION_CAMPAIGN)))_$(RUN_STAMP).json" \
		$(MUTATION_ENGINE_ARGS) || exit $$?; \
	echo "iteration receipt: $$receipt_iterations/$(notdir $(basename $(MUTATION_CAMPAIGN)))_$(RUN_STAMP).json (gitignored)"; \
	echo "tracked receipts stay at $$receipt_root/ -- promote with 'make mutation-receipt-promote'"

mutation-receipt-promote:  ## Copy the newest .iterations receipt of MUTATION_CAMPAIGN into the tracked receipt dir
	@test -n "$(MUTATION_CAMPAIGN)" || { echo "Set MUTATION_CAMPAIGN=campaigns/<id>.json"; exit 2; }
	@$(mutation-receipt-paths); \
	id=$(notdir $(basename $(MUTATION_CAMPAIGN))); \
	newest=$$(ls -t "$$receipt_iterations/$${id}_"*.json 2>/dev/null | head -1); \
	test -n "$$newest" || { echo "no iteration receipts for $$id under $$receipt_iterations/"; exit 2; }; \
	mkdir -p "$$receipt_root" || exit $$?; \
	cp "$$newest" "$$receipt_root/" || exit $$?; \
	echo "promoted $$newest -> $$receipt_root/$$(basename $$newest) -- git add it with the PR"

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
	$(FORGE) complexity refresh-baseline --path src

baseline-vulture:  ## Record an empty justified-findings allowlist for vulture (see script docstring)
	$(UV) run python -m conductor.candidate_review.vulture_baseline_init \
		--baseline src/conductor/vulture_baseline.json \
		--expires "$(BASELINE_EXPIRES)" \
		src

help:  ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-22s\033[0m %s\n", $$1, $$2}'
