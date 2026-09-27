# Session policy test migration

`native/conductor-native/tests/python_contracts_session_policy.rs` replaces
`src/conductor/test_session_policy.py`. All six named Rust tests match the
Python names without the `test_` prefix, with the eight strict-config parameter
rows preserved as isolated Rust fixture cases (13 original executions total).

| Original case | Preserved assertions |
| --- | --- |
| `test_a_host_projects_session_policy_round_trips_exactly` | Exact tuple-valued preamble and mandates, and `FrozenInstanceError` on reassignment. |
| `test_this_packages_own_root_has_no_opinion_by_default` | Identity with `EMPTY_SESSION_POLICY` for the original module-derived `src` root; also checks the actual repository root described by the original test. |
| `test_missing_file_or_session_table_is_generic_empty` | Same empty-policy singleton for both missing file and missing session table. |
| `test_missing_repository_is_not_a_generic_policy` | `SessionPolicyError` names the existing-directory requirement. |
| `test_present_policy_is_complete_and_strict` | Each of eight malformed/incomplete/extra-key/wrong-type/blank-string configurations raises `SessionPolicyError`. |
| `test_policy_reader_refuses_an_oversized_or_nonregular_config` | A 65,537-byte file raises the size error; replacing it with a directory raises the regular-file error. |

Rust creates temporary inputs and owns every assertion. PyO3 exercises the
existing Python compatibility API. No host policy, claim, or shared repository
configuration is changed. CI includes this target through `python_contracts_*`.
