# Conductor doctor contract migration

`src/conductor/test_doctor.py` has 25 named pytest functions and 34 expanded cases. `native/conductor-native/tests/python_contracts_doctor.rs` keeps all 34 as separately named Rust tests. Rust owns fixture construction, JSON edits, file checks, output checks, exceptions, and assertions. PyO3 calls the shipped `conductor.doctor` and `tooling.hooks.dispatch.registry` APIs; no Python test module or inline Python business logic is imported. The fixture writes only temporary `.claude/settings.json` files and restores `HOME` and `CLAUDE_PROJECT_DIR` after its environment test. The hook commands in JSON are parsed by the doctor, never executed.

| Original pytest case | Rust case |
| --- | --- |
| `test_all_checks_pass_on_the_canonical_settings` | `canonical_settings_pass_all_checks` |
| `test_short_prompt_cache_ttl_fails_and_fix_sets_one_hour` | `short_prompt_cache_ttl_fails_and_fix_sets_one_hour` |
| `test_missing_project_prompt_cache_ttl_inherits_user_value` | `missing_project_ttl_inherits_user_value` |
| `test_missing_effective_prompt_cache_ttl_still_fails` | `missing_effective_ttl_still_fails` |
| `test_stray_hook_command_fails_and_fix_canonicalizes` | `stray_hook_command_fails_and_fix_canonicalizes` |
| `test_project_event_can_be_supplied_by_user_settings` | `project_event_can_be_supplied_by_user_settings` |
| `test_the_future_forge_hook_command_is_accepted` | `future_forge_hook_command_is_accepted` |
| `test_absolute_and_environment_prefixed_forge_commands_pass[/opt/bin/forge hook PostToolUse]` | `absolute_forge_hook_command_passes` |
| `test_absolute_and_environment_prefixed_forge_commands_pass[env quoted path]` | `env_quoted_forge_hook_command_passes` |
| `test_absolute_and_environment_prefixed_forge_commands_pass[FORGE_MODE assignment]` | `env_assignment_forge_hook_command_passes` |
| `test_project_values_override_invalid_user_values_without_rewriting_user` | `project_values_override_invalid_user_values_without_rewriting_user` |
| `test_unset_bash_quiet_limit_inherits_the_user_default` | `unset_project_limit_inherits_user_and_fix_preserves_empty_env` |
| `test_non_positive_limit_values_fail[zero]` | `non_positive_limit_zero_word_fails` |
| `test_non_positive_limit_values_fail[0]` | `non_positive_limit_zero_digit_fails` |
| `test_non_positive_limit_values_fail[-1]` | `non_positive_limit_negative_fails` |
| `test_non_positive_limit_values_fail[empty]` | `non_positive_limit_empty_fails` |
| `test_foreign_model_id_fails_and_fix_removes_the_key` | `foreign_model_id_fails_and_fix_removes_key` |
| `test_known_model_ids_pass[claude-opus-5]` | `known_model_claude_opus_passes` |
| `test_known_model_ids_pass[sonnet]` | `known_model_sonnet_passes` |
| `test_known_model_ids_pass[claude-fable-5-1]` | `known_model_claude_fable_passes` |
| `test_a_non_string_model_value_in_user_settings_fails` | `non_string_model_in_user_settings_fails` |
| `test_a_model_with_a_thinking_budget_suffix_passes` | `model_with_thinking_budget_suffix_passes` |
| `test_user_settings_without_dispatcher_hooks_pass` | `user_settings_without_dispatcher_hooks_pass` |
| `test_a_stray_command_in_user_settings_hook_events_fails` | `stray_user_hook_command_fails` |
| `test_missing_user_settings_reports_one_skip_not_a_failure` | `missing_user_settings_reports_skip_without_failure` |
| `test_missing_project_settings_fails_and_fix_writes_the_canonical_block` | `missing_project_settings_fails_then_fix_writes_canonical_block` |
| `test_malformed_settings_json_fails_loud_with_exit_two` | `malformed_settings_json_fails_loud_with_exit_two` |
| `test_invalid_serialized_schema_remains_a_value_error_and_cli_exit_two[[]]` | `schema_array_is_value_error_and_cli_exit_two` |
| `test_invalid_serialized_schema_remains_a_value_error_and_cli_exit_two[null]` | `schema_null_is_value_error_and_cli_exit_two` |
| `test_invalid_serialized_schema_remains_a_value_error_and_cli_exit_two[{\"env\": []}]` | `schema_env_array_is_value_error_and_cli_exit_two` |
| `test_fix_is_idempotent_a_second_run_changes_nothing` | `fix_is_idempotent_a_second_run_changes_nothing` |
| `test_roots_come_from_the_environment_when_flags_are_absent` | `roots_come_from_environment_when_flags_absent` |
| `test_a_mode_is_required` | `mode_is_required` |
| `test_json_mode_reports_findings_and_the_fix_count` | `json_mode_reports_post_fix_findings_and_fix_count` |

## Provider closure and registry rows

The exact rows to add to `native/conductor-native/src/python_contract_targets.tsv` are:

```text
src/conductor/doctor.py	python_contracts_doctor
src/tooling/hooks/dispatch/registry.py	python_contracts_doctor
native/conductor-native/tests/python_contracts/support.rs	python_contracts_doctor
native/conductor-native/tests/python_contracts/agent_comm_support.rs	python_contracts_doctor
native/conductor-native/tests/python_contracts/doctor_fixture.rs	python_contracts_doctor
```

The first two rows are the direct Python APIs used by the Rust target and its fixture. `conductor.doctor` imports `tooling.hooks.dispatch.registry` eagerly; registry imports only standard-library modules. The three helper rows exactly match the target's `#[path]` includes; the new doctor fixture has no nested module includes. `src/conductor/__main__.py` routes the CLI subcommand to doctor, but this target calls `doctor.main` directly, so it is not an eager provider here. Search of executable `src/` and `native/` code found no import of `conductor.test_doctor` or the test file, so retiring that Python test does not strand a fixture importer. Neither doctor nor registry imports `slop_core`, `conductor_native`, or Forge native bindings at module import, so this target has no hard slop/native consumer. The `forge hook ...` values are inert fixture strings.

The top-level Rust test is auto-discovered from `tests/python_contracts_doctor.rs` under the existing `python_contracts_*` Cargo selector. Keep the registry rows beside related doctor/dispatch rows. The parent agent owns registry edits, version changes, Python original retirement, integration, and commit. The open issue list was empty at draft time.

## Verification boundary

The exact Rust target passed all 34 cases with the Forge PyO3 environment and `--test-threads=1`. Scoped Clippy passed with warnings denied. The test uses no network, real GitHub calls, service calls, or installed host settings writes. The final cohort also runs the full local check before landing.
