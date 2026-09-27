# Candidate policy native boundary and test migration

`conductor.candidate_review.policy` keeps its public dataclasses, `PolicyError`,
file reads, SHA-256 digest, TOML syntax diagnostics, and Python date, enum, and
tuple objects. The deterministic schema, field validation, expiration checks,
and change classification live in `native/conductor-native/src/candidate_policy*.rs`.
The adapter passes decoded TOML as JSON to `candidate_policy_parse_native` and
reconstructs the public objects from its normalized result. The native extension
is required; policy rules have no silent Python fallback. The Rust core is also
callable without PyO3 through `parse_policy`, `fragment`, `parse_value_waivers`,
and `classify`. Explicit dates make these operations deterministic.

The Rust `serde_json` dependency enables `preserve_order`: TOML table order is
observable in check execution and the public tuples. Core assertions live in
`native/conductor-native/tests/candidate_policy.rs`. The PyO3 contracts live in
`python_contracts_candidate_policy.rs`,
`python_contracts_candidate_policy_regressions.rs`,
`python_contracts_candidate_cli.rs`, and
`python_contracts_candidate_benchmark.rs` under the same test directory.
The path-glob matcher uses iterative dynamic programming, including Python's
leading-`]` class syntax; a PyO3 oracle table checks custom classification
globs against `fnmatch.fnmatchcase`.

## Original Python case map

| Original suite and cases | Rust replacement |
| --- | --- |
| `test_candidate_review_cli_policy.py`: analyzer resource limits, version exit, timeout and crash | `command_analyzer_errors_block_and_resource_limits_are_bounded` |
| Same: review, receipt verification, attestation, claim, listing and release | `review_attest_claim_and_release_protocol_is_bound_to_the_receipt` |
| Same: malformed and unbound receipts, sealed failure, missing message, absent claim, corrupt store and `fix` exit/paths | `cli_failure_paths_write_a_sealed_failure_and_refuse_unbound_inputs`; `fix_command_preserves_subprocess_exit_and_requires_paths` |
| Same: real Git benchmark scenarios, cold/warm identity, invalid summaries, unknown scenario and output payload | `benchmark_uses_isolated_real_git_candidates_and_preserves_cold_warm_identity`; `benchmark_cli_writes_the_selected_result_payload` |
| Same: policy primitives, schema, classes, baseline and exception validation | Nine core tests in `candidate_policy.rs`; `load_policy_keeps_dates_enums_tuples_defaults_and_order`; `policy_dataclass_validation_rejects_duplicate_unknown_and_unbounded_exceptions` |
| Same: baseline receipts, exception fingerprint, pathless exception and ambiguous match | `baseline_receipt_requires_matching_profile_class_and_file`; `exception_match_uses_fingerprint_and_rejects_ambiguous_exemptions`; `pathless_fingerprint_exception_matches_only_its_exact_finding` |
| Same: ownership path and duration limits, claim-store tampering | `ownership_rejects_broad_paths_overlong_claims_and_tampered_store` |
| `test_policy_exceptions.py`: own-check staleness, live exemption, glob, report fields and rule | `exception_staleness_requires_own_check_and_examined_path`; `exception_finding_match_glob_and_rule_are_auditable` |
| Same: union of examined files, stale report line and nonblocking count | `examined_paths_unions_shards_for_the_same_check`; `human_report_shows_stale_debt_and_every_waived_line_without_blocking_them` |
| `test_value_waivers.py`: base, expiry, exact nodeid, empty list, rewritten finding and its audit fields | `value_waivers_require_base_expiry_and_exact_nodeid` |
| Same: waiver parser refusals and date/tuple conversion | `value_waiver_parser_refuses_empty_pattern_and_short_base`; `value_waiver_parser_keeps_exact_nodeids_and_date_objects` |
| Same: index, range and commit base binding, including unrelated history | `index_range_and_commit_candidates_bind_waivers_to_the_integration_base`; `unrelated_history_cannot_be_a_value_waiver_base` |
| Same: direct mutation gate admission, active and expired waiver metrics; result-level rewrite and pass promotion | `mutation_gate_names_new_nodeids_and_applies_only_active_integration_waivers`; `mutation_result_uses_integration_base_expiry_and_waived_pass_promotion` |
| Same: human report prints and counts every waived finding | `human_report_shows_stale_debt_and_every_waived_line_without_blocking_them` |

The three mapped Python suites were retired after the native core, public PyO3,
CLI, regression, and benchmark contract targets passed. The retired CLI suite
was also removed from the benchmark's `GOVERNANCE_PATHS` and
`BENCHMARK_CLAIM_PATHS` and from the host-only pytest inventory. Historical
campaign receipts, grandfather inventories, and duplication baselines still
record past evidence and remain unchanged as archives.

The focused native and PyO3 suites run through the `conductor-native` Cargo
test targets with `python-compat-tests` enabled; the embedded Python interpreter
uses the installed extension. Benchmark contracts create isolated real Git
fixtures and exercise cold and warm review subprocesses.
