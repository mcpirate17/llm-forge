# Equivalence probe contract test migration

`src/conductor/test_equivalence_probe.py` defines 34 tests. The native target
`native/conductor-native/tests/python_contracts_equivalence_probe.rs` maps each
to one Rust test and calls the shipped `conductor.equivalence_probe`,
`conductor.equivalence_ablations`, and `conductor.probe_replay_stubs` APIs
through PyO3. Rust owns the case assertions, callbacks, test setup, and
control flow. The target exercises the real native ablation engine.

The seven files under
`native/conductor-native/tests/fixtures/equivalence_probe/` are Python
programs that the production probe parses or imports. `fixture_mod.py`,
`test_fixture_mod.py`, and `test_fixture_mod_looping.py` preserve the original
module and driver program strings byte for byte. The method and unary source
inputs preserve their original dedented program text. `replay_subjects.py`
retains a real Python `query` function because production inspects its
`__defaults__`; Rust provides its default objects and all executable callback
algorithms, including the uncopyable payload, destructive consumer, and embed
stubs. The fixtures remain counted as Python source inputs, while the retired
Python test provider supplies no assertions.

Torch is required by these program inputs. `pyproject.toml` declares it only
in the `test` extra; `uv.lock` selects `torch 2.14.0+cpu` from the explicit
PyTorch CPU index on Linux x86_64. The current-platform install adds no CUDA
or NVIDIA packages. The universal lock has marker-gated packages for other
platforms. The target fails when Torch is absent and runs with GPU devices
hidden; no model training or GPU job is involved.

## Case map

| Original Python case | Rust case |
| --- | --- |
| `test_inert_guard_is_reported_as_no_difference` | `inert_guard_is_reported_as_no_difference` |
| `test_expensive_unproven_rules_are_off_unless_asked_for` | `expensive_unproven_rules_are_off_unless_asked_for` |
| `test_load_bearing_construct_is_reported_live` | `load_bearing_construct_is_reported_live` |
| `test_saturation_only_guard_is_untested_rather_than_dead` | `saturation_only_guard_is_untested_rather_than_dead` |
| `test_float_round_off_is_not_reported_as_a_real_difference` | `float_round_off_is_not_reported_as_a_real_difference` |
| `test_a_guard_driven_only_by_its_error_path_reads_live` | `a_guard_driven_only_by_its_error_path_reads_live` |
| `test_both_ends_failing_identically_is_inconclusive_not_agreement` | `both_ends_failing_identically_is_inconclusive_not_agreement` |
| `test_a_call_that_exits_the_process_is_measured_not_propagated` | `a_call_that_exits_the_process_is_measured_not_propagated` |
| `test_methods_record_their_receiver` | `methods_record_their_receiver` |
| `test_trailing_argument_rule_needs_a_defaulted_callee` | `trailing_argument_rule_needs_a_defaulted_callee` |
| `test_unary_call_rule_needs_a_one_argument_callee` | `unary_call_rule_needs_a_one_argument_callee` |
| `test_a_module_probe_shares_one_driver_run_across_its_functions` | `a_module_probe_shares_one_driver_run_across_its_functions` |
| `test_recorders_are_removed_after_a_shared_run` | `recorders_are_removed_after_a_shared_run` |
| `test_batching_bounds_recorder_memory_without_losing_a_target` | `batching_bounds_recorder_memory_without_losing_a_target` |
| `test_whole_function_knockout_reaches_the_probe` | `whole_function_knockout_reaches_the_probe` |
| `test_the_probe_uses_the_native_engine_not_the_python_one` | `the_probe_uses_the_native_engine_not_the_python_one` |
| `test_a_function_that_disagrees_with_itself_yields_no_blocking_finding` | `a_function_that_disagrees_with_itself_yields_no_blocking_finding` |
| `test_the_jitter_control_reports_rather_than_swallows` | `the_jitter_control_reports_rather_than_swallows` |
| `test_the_control_does_not_suppress_a_deterministic_finding` | `the_control_does_not_suppress_a_deterministic_finding` |
| `test_the_jitter_floor_is_pooled_across_the_functions_ablations` | `the_jitter_floor_is_pooled_across_the_functions_ablations` |
| `test_repeated_probes_of_unchanged_code_agree` | `repeated_probes_of_unchanged_code_agree` |
| `test_a_decorator_ablation_reaches_the_probe` | `a_decorator_ablation_reaches_the_probe` |
| `test_an_amplifier_that_cannot_touch_an_argument_is_not_replayed` | `an_amplifier_that_cannot_touch_an_argument_is_not_replayed` |
| `test_a_settled_sweep_stops_early_and_says_the_number_is_a_lower_bound` | `a_settled_sweep_stops_early_and_says_the_number_is_a_lower_bound` |
| `test_a_sweep_with_no_floor_returns_the_true_maximum` | `a_sweep_with_no_floor_returns_the_true_maximum` |
| `test_an_uncopyable_argument_is_refused_rather_than_shared` | `an_uncopyable_argument_is_refused_rather_than_shared` |
| `test_the_baseline_never_differs_from_itself_on_an_uncopyable_argument` | `the_baseline_never_differs_from_itself_on_an_uncopyable_argument` |
| `test_a_dropped_recording_is_counted_rather_than_swallowed` | `a_dropped_recording_is_counted_rather_than_swallowed` |
| `test_the_drop_limit_counts_refused_calls_too` | `the_drop_limit_counts_refused_calls_too` |
| `test_uncopyable_arguments_report_unusable_evidence_not_a_clean_sweep` | `uncopyable_arguments_report_unusable_evidence_not_a_clean_sweep` |
| `test_replay_stub_overrides_swaps_attributes_and_captured_defaults` | `replay_stub_overrides_swaps_attributes_and_captured_defaults` |
| `test_replay_stub_creates_then_deletes_attrs_that_did_not_exist` | `replay_stub_creates_then_deletes_attrs_that_did_not_exist` |
| `test_stub_embeddings_are_deterministic_and_satisfy_the_meta_contract` | `stub_embeddings_are_deterministic_and_satisfy_the_meta_contract` |
| `test_unlisted_modules_get_no_overrides` | `unlisted_modules_get_no_overrides` |

## Selection and validation

The Rust target has case modules
`python_contracts/equivalence_probe_cases_{a,b,c}.rs` and Rust fixtures in
`python_contracts/equivalence_probe_support.rs`, plus the shared
`python_contracts/support.rs`. Registry selection needs the probe,
ablations, replay stubs, native engine and fixture source paths. The Python
provider retired after those rows and its consumers were checked. The seven
Python programs remain tracked fixture inputs; a change to any of them selects
the 34-case Rust target. The slop-core engine and binding are registered
providers too.

With Rust 1.98.0, Forge Python 3.12, two Cargo jobs, one test thread, and
`CUDA_VISIBLE_DEVICES=''`, the exact target passed **34/34** with zero ignored
cases. Scoped `cargo clippy --test python_contracts_equivalence_probe --
-D warnings` passed. `uv sync --locked --extra test` installed the CPU wheel;
`torch.__version__` was `2.14.0+cpu`, `torch.version.cuda` was `None`, and
`torch.cuda.is_available()` was `False`.
