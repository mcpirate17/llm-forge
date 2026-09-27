# Native slop gate and ledger contract migration

This cohort ports all 19 cases from `src/conductor/test_slop_gate.py` and all
16 from `src/conductor/test_slop_ledger.py` to Rust-owned PyO3 contracts. The
original Python baseline passed 35/35 in 2.01 seconds
(`/tmp/forge-slop-contract-python-baseline.log`). The Python originals were
retired after independent parity review, registration, and native validation.

| Original gate case | Rust case |
| --- | --- |
| `test_changed_set_excludes_test_files` | `changed_set_excludes_test_files` |
| `test_drivers_are_the_tests_that_import_the_module` | `drivers_are_tests_that_import_module` |
| `test_the_run_builds_one_index_and_shares_it` | `run_builds_one_index_and_shares_it` |
| `test_a_reachable_untested_branch_blocks` | `reachable_untested_branch_blocks` |
| `test_advisory_verdicts_never_block` | `advisory_verdicts_never_block` |
| `test_a_waiver_downgrades_only_the_rule_it_names` | `waiver_downgrades_only_rule_it_names` |
| `test_unreached_is_separated_from_genuinely_untested` | `unreached_is_separated_from_genuinely_untested` |
| `test_a_module_with_no_driver_is_reported_not_silently_passed` | `module_with_no_driver_is_reported_not_silently_passed` |
| `test_parallel_and_serial_report_the_same_findings` | `parallel_and_serial_report_same_findings` |
| `test_concurrent_probes_never_share_a_report_path` | `concurrent_probes_never_share_report_path` |
| `test_more_than_one_module_is_probed_at_a_time` | `more_than_one_module_is_probed_at_a_time` |
| `test_a_nonsense_job_count_is_refused_not_clamped` | `nonsense_job_count_is_refused_not_clamped` |
| `test_the_default_worker_count_follows_the_machine` | `default_worker_count_follows_machine` |
| `test_a_timed_out_probe_is_a_finding_not_a_clean_sweep` | `timed_out_probe_is_finding_not_clean_sweep` |
| `test_a_crashed_probe_is_a_finding_not_a_clean_sweep` | `crashed_probe_is_finding_not_clean_sweep` |
| `test_a_probe_that_wrote_no_report_is_a_finding` | `probe_that_wrote_no_report_is_finding` |
| `test_an_unparseable_report_is_a_finding` | `unparseable_report_is_finding` |
| `test_a_completed_probe_still_returns_its_findings` | `completed_probe_still_returns_findings` |
| `test_the_probe_workdir_is_removed_on_every_exit` | `probe_workdir_is_removed_on_every_exit` |

| Original ledger case | Rust case |
| --- | --- |
| `test_findings_collapse_to_one_item_per_function_and_verdict` | `findings_collapse_to_one_item_per_function_and_verdict` |
| `test_shipped_code_outranks_a_much_larger_pile_of_exploratory` | `shipped_code_outranks_a_much_larger_pile_of_exploratory` |
| `test_research_tools_is_not_shipped_but_research_synthesis_is` | `research_tools_is_not_shipped_but_research_synthesis_is` |
| `test_a_rerun_of_the_same_sweep_reports_nothing_new` | `rerun_of_same_sweep_reports_nothing_new` |
| `test_fixed_counts_only_modules_this_sweep_actually_covered` | `fixed_counts_only_modules_this_sweep_actually_covered` |
| `test_a_narrow_sweep_does_not_erase_the_rest_of_the_backlog` | `narrow_sweep_does_not_erase_rest_of_backlog` |
| `test_an_item_survives_the_file_being_edited_around_it` | `item_survives_file_being_edited_around_it` |
| `test_the_report_lists_shipped_items_and_only_counts_exploratory` | `report_lists_shipped_items_and_only_counts_exploratory` |
| `test_the_report_says_so_when_nothing_changed` | `report_says_so_when_nothing_changed` |
| `test_the_cli_writes_nothing_on_a_dry_run` | `cli_writes_nothing_on_dry_run` |
| `test_the_cli_writes_both_artifacts` | `cli_writes_both_artifacts` |
| `test_every_bucket_of_a_summary_reaches_the_backlog` | `every_bucket_of_summary_reaches_backlog` |
| `test_a_missing_ledger_is_a_first_run_not_a_crash` | `missing_ledger_is_first_run_not_crash` |
| `test_the_ledger_scope_covers_every_module_it_speaks_for` | `ledger_scope_covers_every_module_it_speaks_for` |
| `test_a_sweep_and_a_gate_run_fold_into_one_backlog` | `sweep_and_gate_run_fold_into_one_backlog` |
| `test_the_cli_folds_every_summary_it_is_given` | `cli_folds_every_summary_it_is_given` |

The gate target invokes the shipped `conductor.slop_gate` API and its
`conductor.repo_index` helper. The helper calls `conductor._native.slop_core()`
and uses the native `slop_core` test index. The ledger target invokes the
shipped `conductor.slop_ledger` API, which calls `slop_core()` at import time
and delegates aggregation and diff to the native ledger. **Both targets
require `slop_core`** and belong in the runner's `SLOP_CONSUMERS` list.
`conductor.candidate_review.equivalence_probe_check` consumes both shipped
modules. The source-level consumers do not import either original test file.

Gate fixtures create Git repositories only under isolated temporary roots,
with system and global Git configuration disabled. They clear ambient Git
directory, worktree, common-directory, index, object, namespace, discovery,
template, and command-line configuration selectors for both the test process
and fixture Git children. The Rust callbacks replace every probe
child process, including timeout, crash, missing-report, malformed-report,
success, parallel-order, and concurrency cases. They preserve the original
JSON inputs and write report fixtures only into each probe's temporary
directory. Ledger cases write explicit temporary JSON and Markdown paths;
they never use the default project ledger or report path. No case runs a real
model, network request, GPU job, or audit over the Forge checkout.

Validation: the two Rust targets passed 19/19 and 16/16 cases respectively
(`/tmp/forge-slop-contract-cargo-test.log`), and scoped Clippy with
`-D warnings` passed (`/tmp/forge-slop-contract-clippy.log`). The runner used
the native 0.1.65 environment, two Cargo jobs, and hidden CUDA devices.
The integrated 0.1.66 replay passed all 64 cohort cases after retirement,
plus 28 discovery and 11 candidate-runner cases. Both Slop targets are now
registered as runtime consumers; the runner regression checks their candidate
extension build requirements. Scoped Clippy also passed for the integrated targets.
The Slop index's repository-scale check retains live-root and file-count validation,
then copies the current Python input corpus into a temporary tree with one known
driver. That preserves import-resolution coverage as the live suites move to Rust.
All 12 index unit cases and the Slop crate's test Clippy checks passed.

Exact registry rows:

```text
src/conductor/slop_gate.py	python_contracts_slop_gate
src/conductor/repo_index.py	python_contracts_slop_gate
src/conductor/slop_ledger.py	python_contracts_slop_ledger
src/conductor/_native.py	python_contracts_slop_gate
src/conductor/_native.py	python_contracts_slop_ledger
native/slop-core/src/index.rs	python_contracts_slop_gate
native/slop-core/src/ledger.rs	python_contracts_slop_ledger
native/slop-core/src/lib.rs	python_contracts_slop_gate
native/slop-core/src/lib.rs	python_contracts_slop_ledger
native/conductor-native/tests/python_contracts/support.rs	python_contracts_slop_gate
native/conductor-native/tests/python_contracts/support.rs	python_contracts_slop_ledger
```
