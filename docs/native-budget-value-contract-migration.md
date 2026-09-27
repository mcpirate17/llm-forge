# Probe budget, policy provenance, and value-scope contract migration

Three shipped host-tool suites now have Rust-owned PyO3 drivers. They call production Python APIs and the compiled bindings those APIs use; no Python test module or `conftest.py` is imported by a Rust target. The original baseline passed **30 tests in 0.59s** with `CUDA_VISIBLE_DEVICES=''`, bytecode writes disabled, and pytest cache disabled (`/tmp/forge-native-budget-value-original-baseline.log`). `gh issue list --limit 100 --state open --json number,title,body,url` returned `[]`; no open issue covered this migration.

| Original Python module | Rust target | Cases |
| --- | --- | ---: |
| `src/conductor/test_probe_budget.py` | `python_contracts_probe_budget` | 11 |
| `src/conductor/test_gate_policy_provenance.py` | `python_contracts_gate_policy_provenance` | 5 |
| `src/conductor/test_value_gate_scope.py` | `python_contracts_value_gate_scope` | 14 |

## Probe-budget correspondence

The temporary probe workspace carries the original scalar `fixture_mod.py`, its two-call driver test, and a local `pytest.ini`. The Rust callback clock advances one second per read and is passed through the original `_Budget` constructor; the probe remains the shipped `probe_function`. The three `slop_gate.run` cases use the original temporary empty-commit Git fixture and stub only `drivers_for` and `probe`, with their original positional argument shapes. The real native test index builds over that temporary repo.

| Original case | Retained assertions |
| --- | --- |
| `a_sweep_cut_at_any_point_is_never_reported_as_clean` | Unbounded sweep reaches a clean verdict; budgets 1, 2, 3, 5, 7, 12 each cut; every result is over-budget or equals its unbounded rule verdict; cuts cover 0, 1, and 2 usable calls. |
| `a_budget_spent_inside_the_sweep_is_not_reported_as_unusable` | Pinned 1.5-second cutoff yields nonempty results, first verdict `OVER_BUDGET`, and zero usable calls. |
| `an_unswept_construct_carries_no_difference_numbers` | Immediate and four pinned cutoffs all cut; every cut clears recorded/amplified differences and amplifier, mentions budget, and at least one had usable calls. |
| `the_direct_api_and_the_cli_still_sweep_everything_by_default` | Direct default is `None`, module default equals `FUNCTION_BUDGET_SECONDS`, `_Budget(0)` and `_Budget(None)` have no deadline, and direct budget zero yields no `OVER_BUDGET`. |
| `the_budget_latches_so_one_function_reports_one_answer` | First clock read is unexpired, second expires, subsequent `expired()` does not read clock again. |
| `the_remediation_names_a_flag_the_probe_accepts` | `_over_budget` detail says `--budget-seconds 0`; the real module `--help` accepts that flag. |
| `an_over_budget_construct_does_not_erase_its_module_from_the_report` | `OVER_BUDGET` belongs to `UNMEASURED`, that bucket is disjoint from incomplete/blocking/advisory/untested, and includes baseline-unusable/uncompilable. |
| `every_verdict_the_probe_can_emit_lands_in_a_gate_bucket` | Every public string value on `Verdict` is in a named gate bucket. |
| `every_verdict_reaches_a_summary_list_or_a_count` | A finding for every gate verdict is accounted for by the live count or six named lists. |
| `an_unclassified_verdict_stops_the_run_rather_than_vanishing` | Unknown future verdict raises an assertion containing `does not classify`. |
| `an_over_budget_module_is_still_counted_as_probed` | Over-budget plus live means one probed module, one unmeasured, one live; a separate timeout fixture means zero probed modules. |

## Gate-policy correspondence

The original `test_gate.repo` fixture is represented by a temporary one-file, one-commit Git repository; the Rust target never imports `conductor.test_gate`. Mutation-corpus and cost-budget phases return inert `PhaseResult` values, and the review callback is stubbed in the one case that enables review. Export, policy path resolution, tool preflight, and subsequent cheap checks still run against the temporary candidate.

| Original case | Retained assertions |
| --- | --- |
| `run_gate_loads_the_policy_from_the_export_not_the_working_tree` | A load occurs, its path is outside the working repo, is named `candidate_policy.toml`, and contains `gate-export-`. |
| `run_gate_exports_before_it_loads_the_policy` | Wrapped real export is called before the stub policy loader. |
| `run_gate_still_refuses_when_the_candidate_policy_is_bad` | Candidate policy `PolicyError` becomes `GateRefusal` naming `policy did not load`. |
| `run_gate_refuses_rather_than_fails_when_a_declared_tool_is_missing` | Missing declared tool yields `EXIT_REFUSED`, not `EXIT_FAIL`, exactly export/preflight phases, and the missing tool ID. |
| `run_gate_runs_the_review_unless_it_is_skipped` | Review callback receives `target_ref=HEAD` and the review phase appears when `skip_review=False`. |

## Value-gate correspondence

Each case initializes a temporary Git repo, writes the candidate source under a temporary snapshot, and constructs production `Change`, `Candidate`, and `ReviewContext` objects with the original fields. Base blobs are created through `git hash-object -w --stdin` only in that repo; the policy is loaded through the shipped resolver. The Python wrapper's native AST and gating decisions remain in production bindings.

| Original case | Retained assertion |
| --- | --- |
| `an_added_file_gates_every_definition` | Both `test_edited` and `test_kept` node IDs are gated. |
| `a_definition_added_to_an_existing_file_is_gated` | Only the new definition is gated. |
| `a_modified_definition_is_gated` | Only the edited definition is gated. |
| `an_untouched_definition_sharing_the_file_is_not_gated` | `test_kept` is absent from the selected node IDs. |
| `a_file_changed_only_outside_its_tests_gates_nothing` | Import-only change produces an empty selection. |
| `a_decorator_only_change_still_counts_as_modified` | Parametrize change selects `test_p`. |
| `a_grandfathered_definition_stays_excluded_when_modified` | Modified grandfathered `test_edited` produces no selection. |
| `an_unreadable_base_blob_gates_the_whole_file` | Missing base OID gates both definitions. |
| `a_base_that_does_not_parse_gates_the_whole_file` | Malformed base gates both definitions. |
| `a_renamed_file_is_compared_against_its_old_path` | Rename with an old base blob gates only new definition. |
| `class_nested_tests_are_scoped_by_their_qualified_label` | Only `TestThing::test_edited` is gated. |
| `a_candidate_that_does_not_parse_is_refused_not_skipped` | Malformed candidate raises `cannot parse test definitions`. |
| `a_mutant_patch_fragment_is_never_a_test_definition` | Patch fragment is not selected as a test. |
| `a_new_non_python_test_file_is_gated_as_a_whole_path` | New JavaScript test selects its complete file path. |

The Git-using cases clear ambient repository, worktree, index, object, namespace, ceiling, inline-config, `GIT_CONFIG`, and `GIT_TEMPLATE_DIR` selectors before production calls; child Git clears the latter two too and uses isolated global/system config. This prevents an ambient `git config` destination or init template from affecting the fixture. All writes remain in `Case::root()` or gate's own temporary export. The existing GPU process was left untouched; the baseline and native tests hide CUDA and do not launch actual campaigns, models, or network calls.

## Providers for target registration

Exact provider/helper rows for the integration owner's registry:

```text
src/conductor/equivalence_probe.py\tpython_contracts_probe_budget
src/conductor/native_ablations.py\tpython_contracts_probe_budget
src/conductor/slop_gate.py\tpython_contracts_probe_budget
src/conductor/repo_index.py\tpython_contracts_probe_budget
src/conductor/_native.py\tpython_contracts_probe_budget
native/slop-core/src/lib.rs\tpython_contracts_probe_budget
native/slop-core/src/engine.rs\tpython_contracts_probe_budget
native/slop-core/src/rules.rs\tpython_contracts_probe_budget
native/slop-core/src/index.rs\tpython_contracts_probe_budget
native/conductor-native/tests/python_contracts/support.rs\tpython_contracts_probe_budget
src/conductor/gate.py\tpython_contracts_gate_policy_provenance
src/conductor/candidate_review/policy.py\tpython_contracts_gate_policy_provenance
src/conductor/candidate_review/policy_path.py\tpython_contracts_gate_policy_provenance
src/conductor/project_paths.py\tpython_contracts_gate_policy_provenance
src/conductor/_native.py\tpython_contracts_gate_policy_provenance
native/conductor-native/src/project_paths.rs\tpython_contracts_gate_policy_provenance
native/conductor-native/src/lib.rs\tpython_contracts_gate_policy_provenance
native/conductor-native/tests/python_contracts/support.rs\tpython_contracts_gate_policy_provenance
src/conductor/candidate_review/verification.py\tpython_contracts_value_gate_scope
src/conductor/candidate_review/checks.py\tpython_contracts_value_gate_scope
src/conductor/candidate_review/model.py\tpython_contracts_value_gate_scope
src/conductor/candidate_review/policy.py\tpython_contracts_value_gate_scope
src/conductor/candidate_review/policy_path.py\tpython_contracts_value_gate_scope
src/conductor/candidate_review/git_source.py\tpython_contracts_value_gate_scope
src/conductor/project_paths.py\tpython_contracts_value_gate_scope
native/conductor-native/src/project_paths.rs\tpython_contracts_value_gate_scope
src/conductor/_native.py\tpython_contracts_value_gate_scope
native/conductor-native/src/candidate_verification.rs\tpython_contracts_value_gate_scope
native/conductor-native/src/candidate_verification_runtime.rs\tpython_contracts_value_gate_scope
native/conductor-native/src/lib.rs\tpython_contracts_value_gate_scope
native/conductor-native/tests/python_contracts/support.rs\tpython_contracts_value_gate_scope
```

`python_contracts_probe_budget` is a hard `slop_core` consumer: `native_ablations.py` and `repo_index.py` both call `slop_core()` at module import. Gate provenance and value scope resolve policy paths through the native project-path bindings. Value scope also calls the freshly built `conductor_native` functions `candidate_verification_ast_native` and `candidate_verification_native`. No source import search found an executable importer of these three original test modules outside pytest collection; the original gate-provenance suite imported the `repo` fixture from `conductor.test_gate`, which the Rust target reproduces without importing that Python test module.

## Native verification

The exact three Rust targets passed with `cargo +1.98.0 test --offline --locked --manifest-path native/conductor-native/Cargo.toml --features python-compat-tests --test python_contracts_probe_budget --test python_contracts_gate_policy_provenance --test python_contracts_value_gate_scope -- --test-threads=1`: **11 + 5 + 14 passed**. The same targets passed `cargo +1.98.0 clippy --offline --locked --manifest-path native/conductor-native/Cargo.toml --features python-compat-tests --test python_contracts_probe_budget --test python_contracts_gate_policy_provenance --test python_contracts_value_gate_scope -- -D warnings`. Both runs used `/tmp/forge-native62-install-venv` for Python, `/tmp/forge-contract-native66` for fresh `conductor_native` and `slop_core`, `SQLITE3_LIB_DIR=/tmp/forge-sqlite-link`, `CUDA_VISIBLE_DEVICES=''`, and two Cargo jobs. Final canary replay set `GIT_CONFIG` to a scratch file and `GIT_TEMPLATE_DIR` to a scratch template containing a failing pre-commit hook; all 30 cases passed, and the ambient config file's SHA-256 remained unchanged. Logs: `/tmp/forge-budget-review-canary/native-test.log` and `/tmp/forge-budget-review-canary/native-clippy.log`.
