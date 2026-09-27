# Mutation attribution contract migration

`src/conductor/test_mutation_attribution.py` collects 26 expanded pytest cases
from 20 named functions; one function has seven parameter cases. The proposed
replacement is `native/conductor-native/tests/python_contracts_mutation_attribution.rs`
with the dedicated Rust fixture
`native/conductor-native/tests/python_contracts/mutation_attribution_support.rs`.
The existing `tests/python_contracts/support.rs` supplies isolated directories,
interpreter access, and restored process state. The original test module remains
in place pending independent parity review and retirement.

The Rust callback reads `pkg/calc.py` on every simulated run and writes the
same pytest-shaped JUnit report as the original `FakeRunner`. A mutation that
does not reach disk, or is not restored, changes the observed result. Rust
constructs all mutants, campaign inputs, runner behavior, assertions, and
capture streams. PyO3 calls shipped Python APIs. These are fake process
fixtures; they never run fest, cargo-mutants, Mull, pytest children, or a
mutation engine.

## Exact expanded case map

| Original pytest case | Rust case |
| --- | --- |
| `test_attribution_charges_each_mutant_to_the_test_that_actually_failed` | `attribution_charges_each_mutant_to_the_test_that_actually_failed` |
| `test_a_mutant_whose_covering_set_still_passes_is_recorded_not_dropped` | `a_mutant_whose_covering_set_still_passes_is_recorded_not_dropped` |
| `test_only_killed_mutants_are_re_applied_and_none_says_why` | `only_killed_mutants_are_reapplied_and_none_says_why` |
| `test_the_coverage_map_is_ranked_by_what_a_junit_report_can_name` | `coverage_map_is_ranked_by_what_a_junit_report_can_name` |
| `test_the_baseline_precedes_the_mutants_and_both_select_only_covering_tests` | `baseline_precedes_mutants_and_both_select_only_covering_tests` |
| `test_narrowing_keeps_the_flags_and_cuts_selection_and_early_exit[argv0-expected0]` | `narrowing_removes_path_selection` |
| `test_narrowing_keeps_the_flags_and_cuts_selection_and_early_exit[argv1-expected1]` | `narrowing_removes_exit_first` |
| `test_narrowing_keeps_the_flags_and_cuts_selection_and_early_exit[argv2-expected2]` | `narrowing_removes_separate_maxfail` |
| `test_narrowing_keeps_the_flags_and_cuts_selection_and_early_exit[argv3-expected3]` | `narrowing_removes_inline_maxfail` |
| `test_narrowing_keeps_the_flags_and_cuts_selection_and_early_exit[argv4-expected4]` | `narrowing_removes_keyword_selection` |
| `test_narrowing_keeps_the_flags_and_cuts_selection_and_early_exit[argv5-expected5]` | `narrowing_removes_nodeid_selection` |
| `test_narrowing_keeps_the_flags_and_cuts_selection_and_early_exit[argv6-expected6]` | `narrowing_keeps_plugin_flag_and_value` |
| `test_the_source_and_its_bytecode_are_restored_around_every_mutation` | `source_and_bytecode_are_restored_around_every_mutation` |
| `test_the_summary_names_every_field_a_reader_of_it_needs` | `summary_names_every_field_a_reader_needs` |
| `test_an_absolute_pytest_is_recognised_and_survives_narrowing` | `absolute_pytest_is_recognized_and_survives_narrowing` |
| `test_narrowing_keeps_what_follows_a_selector_and_refuses_what_it_cannot` | `narrowing_keeps_later_flags_and_refuses_unusable_commands` |
| `test_selection_pins_rootdir_to_the_snapshot_worktree` | `selection_pins_rootdir_to_the_snapshot_worktree` |
| `test_re_runs_never_disable_the_run_private_cache` | `reruns_never_disable_the_run_private_cache` |
| `test_reapplying_a_mutant_evicts_its_cached_bytecode` | `reapplying_a_mutant_evicts_beside_and_private_bytecode` |
| `test_reports_are_written_to_their_own_directory` | `reports_are_written_to_their_own_directory` |
| `test_progress_prints_every_position_when_the_run_is_small` | `progress_prints_every_position_when_the_run_is_small` |
| `test_progress_step_arithmetic_is_pinned_at_a_chosen_total` | `progress_step_arithmetic_is_pinned_at_chosen_totals` |
| `test_a_mutant_that_aborts_the_rerun_is_unattributed_not_fatal` | `mutant_that_aborts_the_rerun_is_unattributed_not_fatal` |
| `test_a_baseline_that_writes_no_report_still_refuses` | `baseline_that_writes_no_report_still_refuses` |
| `test_a_hung_mutant_rerun_gets_the_baseline_derived_limit_and_says_so` | `hung_mutant_gets_baseline_derived_limit_and_reports_it` |
| `test_the_rerun_limit_never_exceeds_the_campaign_limit_nor_drops_below_the_floor` | `rerun_limit_is_capped_and_floored` |

## Provider closure for `python_contract_targets.tsv`

Every line below would map to `python_contracts_mutation_attribution`:

```
src/conductor/mutation_attribution.py
src/conductor/mutation_engine_generated.py
src/conductor/mutation_campaign_model.py
src/conductor/mutation_scope.py
src/conductor/mutation_value.py
src/conductor/bytecode_isolation.py
src/conductor/mutation_testing_support.py
src/conductor/mutation_patch_apply.py
src/conductor/project_paths.py
src/conductor/snapshot_worktree.py
src/conductor/_native.py
native/conductor-native/src/lib.rs
native/conductor-native/src/mutation_value.rs
native/conductor-native/src/mutation_value_inputs.rs
native/conductor-native/src/project_paths.rs
native/conductor-native/tests/python_contracts/support.rs
native/conductor-native/tests/python_contracts/mutation_attribution_support.rs
```

`mutation_attribution.attribute` directly uses the generated-engine constants and
`pinned`, bytecode eviction, `CampaignError`, and value analysis/JUnit parsing.
The Rust fixture directly instantiates `mutation_campaign_model.CommandResult`.
The generated engine and campaign model import mutation support and path
resolution; the generated engine also imports snapshot handling. The campaign
model records mutation patch application among its runner components. The path
resolver calls the native `project_paths.rs` provider.
The observed value-analysis paths cross `conductor._native` into the
`conductor-native` extension's `mutation_value.rs`, registered by `lib.rs`;
the value module calls its sibling `mutation_value_inputs.rs` for JUnit input
normalization.
Both shared and dedicated Rust helpers are included, so helper changes select
the target. The target source itself is discovered as a Rust test file by the
registry. Its case fixture data (`pkg/calc.py`, `pkg/test_calc.py`) is created at
runtime and has no repository provider path.

## Importer and retirement map

| Importer | Relationship |
| --- | --- |
| `src/conductor/mutation_engine_fest.py` | Production consumer: `from conductor import mutation_attribution as _attribution`; preserve. |
| `src/conductor/test_mutation_attribution.py` | Retired original, replaced by the 26 Rust contracts above. |
| `src/conductor/mutation_attribution.py` | Imports generated engine, bytecode isolation, scope, and value analysis; preserve. |

No production module imports `test_mutation_attribution.py` or its private
helpers (`FakeRunner`, `Campaign`, `mutant`, `run_attribution`). The installed
`conductor_native` module is loaded through `src/conductor/_native.py`. The
GitHub issue list was empty when this migration began, so no issue covers it.
The original module is retired in this cohort. Provider mappings select the
replacement target when its production sources or Rust helpers change.

## Integration gate

The original pytest module passed 26/26
(`/tmp/forge-native-attribution-original-pytest.log`). The exact Rust target
passed 26/26 with Rust 1.98.0, two Cargo jobs, one test thread, and CUDA
hidden (`/tmp/forge-native-attribution-26-tests.log`). Scoped Clippy with
`-D warnings` passed (`/tmp/forge-native-attribution-clippy.log`). These
checks exercise fake process fixtures only. Independent review checked all
cases and the provider closure. After making the classification assertion
order-insensitive to match the original dictionary comparison, the Rust
target again passed 26/26. The full local check remains required on the
committed tree before landing. `gh issue list` returned no open issues.
