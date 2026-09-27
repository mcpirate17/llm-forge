# Campaign planner and refresh test migration

The mutation planner's Rust core owns subject discovery, pairing, and manifest
construction. Refresh calls that same core and returns replacement manifest
content to the Python file-I/O boundary. The seven Rust planner contract cases
passed before the duplicate Python planner was removed. The public Python
boundary remains covered by the frozen fixture test and campaign tests.

The fifteen frozen `tests/fixtures/mutation_plan` cases compare native planning
against historical Python output, including both refusals and full manifests.
`mutation_plan_contract.rs` adds specific assertions for behavior that the
Python helper tests reached directly. This is the removal map:

| Rust contract case or frozen fixture | Python tests whose planner assertions it covers | Python boundary still needed |
| --- | --- | --- |
| `cargo_package_identity_ignores_workspace_bin_and_dependency_names`; case09-12 | `test_the_package_name_comes_from_package_not_bin_or_dependencies`, `test_a_workspace_root_declaring_no_package_is_not_a_subject` | `plan()` JSON bridge and Cargo manifest validation |
| `package_with_no_rust_sources_is_refused`; case11 | `test_a_crate_with_a_package_but_no_sources_is_refused` | `CampaignError` translation |
| `cargo_manifest_pins_inline_and_integration_tests_and_exact_source_scope`; case09, case10, case12 | `test_rust_test_discovery_includes_integration_and_inline_tests_but_skips_venvs`, `test_cargo_manifest_preserves_the_exact_scoped_engine_contract`, `test_rust_planning_intersects_scope_instead_of_expanding_to_the_crate` | Generated campaign model loading and public `plan()` |
| `python_manifest_keeps_engine_contract_and_rejects_skipped_trees`; case01, case13 | `test_test_file_recognition_covers_all_supported_layouts_only`, `test_fest_manifest_binds_its_generated_engine_contract`, `test_a_vendored_or_virtualenv_tree_is_never_a_subject` | Public `plan()` and generated campaign model loading |
| `mirror_wins_over_unrelated_name_and_orphan_lines_are_reported`; case02-05 | `test_both_test_layouts_in_this_repo_are_paired`, `test_same_basename_tests_in_two_packages_pair_with_their_own_modules`, `test_an_unmirrored_basename_collision_refuses_naming_both_candidates`, `test_a_sole_unmirrored_same_basename_test_still_pairs`, `test_a_tests_mirror_beats_a_same_basename_stranger` | Public `plan()` error translation and output ordering |
| `slug_collisions_disambiguate_without_changing_unique_campaign_ids`; case14 | `test_two_subjects_never_share_one_campaign_id` | Manifest file-name collision protection in `write()` |
| `covered_subject_is_skipped_until_include_covered_requests_a_narrow_campaign`; case07, case15 | `test_a_subject_a_committed_campaign_already_covers_is_skipped`, `test_a_narrow_second_campaign_may_be_planned_over_a_covered_source` | Public `plan()` scope and include-covered wiring |
| case06 and native `compute_refresh` tests | `test_an_extra_test_pairs_a_subject_no_test_is_named_after`, refresh planner assertions | CLI `--extra-test` validation, file writes, and ratchet retention |

The retired mock-only `test_rust_plan_uses_zero_lines_only_for_partial_subject_metadata`
creates a Rust crate record with missing line metadata. Real crate discovery
always records a line count; case11 and the public untested-crate test cover
that observable behavior. The fixture parity test selected the removed Python
algorithm as its live oracle; the frozen Rust fixture corpus is the historical
oracle after the fallback is retired.

The new `native/conductor-native/tests/mutation_plan_refresh.rs` exercises the
pure Rust `compute_refresh` API without Python. Its five cases passed with
`--no-default-features`; public-wrapper validation remains a separate gate.

| Rust case | Existing Python behavior covered | Boundary still retained in Python |
| --- | --- | --- |
| `python_refresh_rebinds_source_and_test_pins_without_erasing_ratchet` | `test_refresh_rebinds_a_fest_campaign_without_erasing_its_baseline`: source/test pin regeneration, timeout, ratchet fields | Manifest file load, pretty JSON write, relative path return, and error translation |
| `python_extra_test_refresh_requires_recorded_test_list` | `test_refresh_carries_an_extra_test_campaign_forward_without_erasing_its_baseline`: unpaired subject's recorded test list and missing-list refusal | On-disk ratchet persistence and public command path |
| `rust_refresh_scopes_pins_and_keeps_every_recorded_ratchet_field` | `test_refresh_rebinds_a_cargo_campaign_to_the_sources_it_was_asked_for`: exact scope, source/test hash binding, jobs, timeout, ratchet fields, unknown file refusal | On-disk write and public command path |
| `rust_implicit_scope_rejects_empty_malformed_and_unbound_paths` | Retired `test_declared_rust_scope_requires_a_nonempty_list_and_returns_exact_paths`; retained `test_rust_refresh_rejects_empty_or_unbound_implicit_scope`: current-crate containment and malformed scope refusal | Python wrapper exception type and file-preserving refusal |
| `rust_legacy_manifest_path_repairs_only_an_unambiguous_package` | `test_rust_refresh_repairs_one_legacy_manifest_path_and_retains_its_note`, `test_rust_refresh_refuses_an_ambiguous_legacy_package_path`: unique-package repair and ambiguity refusal | Manifest file persistence and path return |

The native refresh function returns data only. Python still reads the recorded
manifest, writes the replacement with its trailing newline, and returns the
repository-relative path. That seam remains tested in Python.

The duplicate private Python refresh helpers for recorded tests, crate
selection, scope resolution, hash rebinding, and ratchet copying were removed.
The Python planning fallback and its separate subject, pairing, slug, coverage,
and manifest helpers were also removed after the Rust contract cases passed.
Python still owns changed-file scope selection through Git and live claims,
operator input validation, manifest writes, and public error translation.
