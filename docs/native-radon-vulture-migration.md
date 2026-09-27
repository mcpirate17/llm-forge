# Native Radon and Vulture contract migration

This cohort ports 13 named Radon cases and 13 named Vulture cases from
`src/conductor/test_radon_complexity.py` and
`src/conductor/test_vulture_audit.py` into Rust-owned PyO3 tests. The Vulture
whitelist parameterization expands to two cases, for 27 total. The original
Python baseline passed 27/27 before migration
(`/tmp/forge-radon-vulture-python-baseline.log`). The venv's `vulture`
executable must be on `PATH`; omitting that path makes ten original cases
fail before analyzer output can be assessed.

| Original case | Rust case |
| --- | --- |
| `test_conductor_is_scanned_by_default` | `conductor_is_scanned_by_default` |
| `test_a_grandfathered_symbol_that_worsens_fails` | `grandfathered_symbol_that_worsens_fails` |
| `test_a_grandfathered_symbol_holding_its_score_passes` | `grandfathered_symbol_holding_its_score_passes` |
| `test_an_improved_symbol_passes_and_asks_for_a_tighter_baseline` | `improved_symbol_passes_and_asks_for_a_tighter_baseline` |
| `test_a_new_block_above_the_minimum_rank_still_fails` | `new_block_above_minimum_rank_still_fails` |
| `test_a_repeated_baseline_key_is_held_to_its_lowest_score` | `repeated_baseline_key_is_held_to_lowest_score` |
| `test_blocks_below_the_minimum_rank_are_not_ratcheted` | `blocks_below_minimum_rank_are_not_ratcheted` |
| `test_the_ratchet_runs_in_the_pre_commit_profile` | `ratchet_runs_in_pre_commit_profile` |
| `test_the_unflagged_default_baseline_lives_in_the_host_tree` | `unflagged_default_baseline_lives_in_host_tree` |
| `test_the_default_follows_the_host_and_not_this_package` | `default_follows_host_and_not_package` |
| `test_a_relative_baseline_flag_resolves_against_the_host_root` | `relative_baseline_flag_resolves_against_host_root` |
| `test_an_absolute_baseline_flag_is_taken_as_given` | `absolute_baseline_flag_is_taken_as_given` |
| `test_a_missing_baseline_is_refused_by_name` | `missing_baseline_is_refused_by_name` |
| `test_vulture_baseline_rejects_count_and_key_mismatch` | `baseline_rejects_count_and_key_mismatch` |
| `test_vulture_audit_blocks_new_real_finding[whitelist-present]` | `audit_blocks_new_real_finding_with_whitelist` |
| `test_vulture_audit_blocks_new_real_finding[whitelist-absent]` | `audit_blocks_new_real_finding_without_whitelist` |
| `test_vulture_audit_rejects_resolved_stale_entry` | `audit_rejects_resolved_stale_entry` |
| `test_vulture_rejects_unbound_tree_and_untrusted_analyzer` | `rejects_unbound_tree_and_untrusted_analyzer` |
| `test_vulture_rejects_unrecognized_success_output` | `rejects_unrecognized_success_output` |
| `test_vulture_baseline_schema_and_entry_failures_are_blocking` | `baseline_schema_and_entry_failures_are_blocking` |
| `test_vulture_main_returns_audit_error_for_missing_baseline` | `main_returns_audit_error_for_missing_baseline` |
| `test_vulture_changed_file_caused_blocks` | `changed_file_caused_blocks` |
| `test_vulture_inherited_finding_does_not_block` | `inherited_finding_does_not_block` |
| `test_vulture_no_changed_files_blocks_every_new_finding` | `no_changed_files_blocks_every_new_finding` |
| `test_vulture_no_new_findings_exits_zero_either_way` | `no_new_findings_exits_zero_either_way` |
| `test_vulture_changed_baseline_only_file_not_reported_as_caused` | `changed_baseline_only_file_not_reported_as_caused` |
| `test_run_audit_reports_a_missing_vulture_as_an_audit_error` | `missing_vulture_is_audit_error` |

The Radon target drives the shipped `conductor.radon_complexity` API. Its
supporting providers are `conductor.project_paths`,
`conductor.candidate_review.policy_path`, `policy`, and `model`. The module's
import-time `host_root()` call enters `conductor._native`, requiring
`conductor_native`. That seam attempts an optional `slop_core` import, but
these paths never call `slop_core()` and do not require that extension.
`radon.complexity` is the declared analyzer dependency in `pyproject.toml`.

The Vulture target drives the shipped
`conductor.candidate_review.vulture_audit` API and its
`conductor.changed_files_cli` helper. Its two unmocked analyzer cases scan
only Rust-created files in isolated temporary directories. Analyzer-output
cases patch only `subprocess.run` through PyO3, preserving executable
discovery; the missing-tool case patches `shutil.which`. `vulture` is a
declared runtime dependency in `pyproject.toml`. No
test calls the audit on the real Forge project or starts a model, network, or
GPU job. Python attributes and environment variables are restored by the
shared `python_contracts/support.rs` fixture. Every Vulture case clears any
ambient `CONDUCTOR_VULTURE_WHITELIST` first; the two whitelist cases then set
their own temporary path, so the real analyzer cannot read a foreign
whitelist through inherited environment state.

Exact registry rows:

```text
src/conductor/radon_complexity.py	python_contracts_radon_complexity
src/conductor/project_paths.py	python_contracts_radon_complexity
src/conductor/_native.py	python_contracts_radon_complexity
native/conductor-native/src/project_paths.rs	python_contracts_radon_complexity
native/conductor-native/src/lib.rs	python_contracts_radon_complexity
src/conductor/candidate_review/policy_path.py	python_contracts_radon_complexity
src/conductor/candidate_review/policy.py	python_contracts_radon_complexity
src/conductor/candidate_review/model.py	python_contracts_radon_complexity
src/conductor/candidate_review/vulture_audit.py	python_contracts_vulture_audit
src/conductor/changed_files_cli.py	python_contracts_vulture_audit
native/conductor-native/tests/python_contracts/support.rs	python_contracts_radon_complexity
native/conductor-native/tests/python_contracts/support.rs	python_contracts_vulture_audit
```

The originals were retired after independent parity review, target
registration, and native validation. No production module imports either
original test file.

The scoped Rust run passed 13/13 Radon and 14/14 Vulture cases, alongside
21/21 corrected guard cases from the previous cohort
(`/tmp/forge-radon-vulture-cargo-test.log`). Scoped Clippy passed with
`-D warnings` (`/tmp/forge-radon-vulture-clippy.log`). Cargo ran offline
with two build jobs and CUDA hidden.

After restoring the original cases' live executable discovery, the Vulture
target passed 14/14 again (`/tmp/forge-radon-vulture-final-test.log`), and
scoped Clippy passed again (`/tmp/forge-radon-vulture-final-clippy.log`).
The ambient-whitelist isolation fix passed all 14 Vulture cases on the
current 0.1.65 crate (`/tmp/forge-radon-vulture-isolation-test.log`), with
scoped Clippy clean (`/tmp/forge-radon-vulture-isolation-clippy.log`).
