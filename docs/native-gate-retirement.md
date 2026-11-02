# Native gate retirement

`make gate` invokes `forge candidate gate` with an explicitly granted CI range
review. It exports the selected target tree to a bounded private snapshot,
loads policy from that tree, checks declared tool versions through granted
program aliases, probes candidate pytest configurations in isolated stages,
reports waiver base matches under explicitly pinned waiver authority, checks
live-index/live-worktree import closure, audits the captured candidate corpus,
checks the native cost budget, and executes the complete native review. It
never treats an existing receipt or a review plan as executed evidence.

The gate binds target commit, merge-base tree, candidate policy and operator
grants to the eventual fresh review. It holds and rechecks the corpus runner,
Git build profile and inputs, executable identities, cost baseline and seven
days of actual ledger inputs, and replays the live import closure including
source hashes and negative lookups before disposition. The closure deliberately
uses the live Git index and live working tree: this preserves the legacy check
for untracked modules reachable from tracked code. The policy, corpus, pytest
probes and actual review instead use the selected candidate tree. Same-user
replace-and-restore races remain outside the existing trusted-parent contract.

`--skip-review` preserves the preflight-only exit behavior while explicitly
marking review as skipped; that result is not a complete review. The optional
`--skip-pytest-config` similarly marks the probe as skipped. Normal runs report
PASS only when every phase and the fresh complete review pass; failed audit or
review phases exit 1, and invalid or unsupported authority, preflight failure,
drift, cancellation or incomplete setup refuse with exit 2. The review receipt
is written to `--json-out` (default `tasks/audit/gate.json`); `--json` controls
the gate phase report on stdout. A valid review receipt can already exist if a
later gate input recheck refuses; that receipt attests the review, not a gate PASS.

Both `make gate` and the native CLI require the operator-owned review grants,
the pinned Git path and SHA-256, the separately pinned Git build profile and
its SHA-256, and a pinned mutation runner bundle. The profile is a provisioning
assertion about the actual system attributes path of that exact Git binary; it
is not inferred from executable strings or version output. Tool paths and the
pytest interpreter come only from grants, not from an automatically searched
`node_modules/.bin` or from candidate policy. Shebang interpreter chains are
held and revalidated; `/usr/bin/env` shebangs are explicitly unsupported because
they select an indirect interpreter. Missing grants and an unpinned
candidate waiver policy refuse explicitly. See `native-audit-contract-migration.md`
and the `candidate mutation-corpus --help` contract for corpus profile details.

Admission limits are explicit: candidate snapshots use the native 64 MiB per
blob and 2 GiB tree limits; the live import closure admits at most 100,000
files and 100,000 lookup paths, 16 MiB per Python file, 1 GiB of Python source
and 16 MiB of lookup path bytes. Gate preflight admits at most 128 declared
tools, 64 pytest configurations, 1 GiB per granted executable and 2 GiB total
for executable observations. The cost phase holds the four native ledger
streams over the seven-day window using the corpus input bounds. Unsafe paths,
symlinks, parse failures, drift, unsupported Git environment overrides and
limit violations are explicit refusals.

## Observation map

| Retired Python contract | Native coverage |
| --- | --- |
| Export candidate bytes, omit untracked files and reject invalid refs; candidate policy beats dirty working policy | `candidate_gate_tests::exported_policy_is_candidate_bound_and_untracked_files_are_excluded`, `malformed_candidate_policy_is_rejected_even_when_worktree_is_clean`, and the opt-in `actual_ci_review_runs_despite_failed_corpus_and_is_bound_to_exported_tree` fixture |
| Missing/drifted tools, profile filtering, failing version commands and exact observed version | `candidate_gate_tools::tests`, plus opt-in `missing_declared_tool_refuses_before_review_or_receipt`, `granted_version_probe_runs_and_skip_review_is_reported`, `granted_version_drift_refuses_before_corpus_and_review`, `tool_required_only_for_full_profile_does_not_block_fast_gate`, and `failing_version_command_is_versionless_and_refuses_the_gate` |
| npm/PATH hint | Explicit grant authority replaces PATH auto-discovery; the missing-tool fixture includes an ungranted `node_modules/.bin` decoy and verifies refusal |
| Config discovery, smallest in-scope sample, valid/invalid pytest and isolated bytecode | `candidate_gate_pytest::tests` plus opt-in `invalid_pytest_configuration_refuses_before_any_review` and `valid_pytest_configuration_collects_before_explicit_review_skip`; the latter asserts the granted executable path, isolated cache prefix and disabled writes in real conftest code |
| Waiver base counts and authority | `candidate_gate_waivers::tests::waiver_base_matches_never_substitute_for_operator_authority`; actual full activation remains the native review's date/hash/base checks |
| PASS/FAIL/REFUSED and skipped versus actual review (including setup errors, incomplete execution and mismatched bindings) | `candidate_gate_tests::report_statuses_distinguish_pass_failure_and_refusal`, `full_review_requires_real_execution_and_exact_candidate_authority`, and opt-in actual/skip review CLI fixtures |
| Corpus debt ratchet, named new/resolved findings and candidate bytes | Native corpus seven-dimensional tests and `candidate_gate_corpus_tests`; opt-in actual gate fixture makes the live registry invalid while auditing the committed registry |
| Clean clone import closure and live-source drift | Native `dead_tests::bounded` transitive, symlink, parser, size and replay/negative-lookup tests |
| Cost phase and execution failure | `candidate_gate_ledger_inputs::tests` binds the exact native seven-day input window; the opt-in actual gate fixture executes the native ledger phase before review; native ledger budget tests cover the calculation itself |
| Installed gate help does not use the Python site | `native_candidate_gate::installed_native_gate_help_needs_no_python_site_or_runtime_profile` |

The actual cgroup-backed review, tool and pytest integration fixtures are
explicitly ignored in the ordinary test run. Their opt-in execution requires a
source-matching `FORGE_TEST_BIN`, a delegated `FORGE_TEST_CGROUP_PARENT`, and
an operator-established `FORGE_TEST_GIT_SYSTEM_ATTRIBUTES` (`FORGE_TEST_PYTHON`
with pytest is additionally required for guest probes). The interpreter path
retains its absolute spelling through launch, including a virtualenv symlink;
reducing it to a basename loses virtualenv discovery with the cleared `PATH`.
Provision guest packages within the granted environment: an editable install
pointing outside those grants is deliberately inaccessible. The test fixture
accepts up to 16 additional absolute read capabilities in the path-separated
`FORGE_TEST_PYTHON_READ_ONLY`, for explicitly provisioned runtime files such as
a distribution's `sitecustomize.py` symlink target. This setting does not add
production grants or disable plugin loading.

Failed pytest probes retain labeled stdout/stderr excerpts within a shared
4096-byte diagnostic budget, preserving both ends of long UTF-8 messages. This
summarizes Stage's already bounded capture; bytes discarded upstream cannot be
reconstructed. No runtime or parity
result is implied by authoring these fixtures. Retired Python gate and its
three obsolete compatibility targets are removed; `gate_rollout` remains a
separate command and test target.

Corpus Git probes use the owned interrupt supervisor and restore each child's
signal mask. Cancellation tears down their owned process group; corpus baseline
publication checks cancellation immediately before rename. Other setup work is
bounded but not asynchronously preempted. The existing review lock acquisition
may delay cancellation up to its bounded timeout.
