# CRG gate test migration

The 70 collected cases formerly in `src/tooling/hooks/agent/test_crg_gate.py`
are Rust-owned integration tests. The four targets use PyO3 to call shipped
Python compatibility APIs and assert their returned values in Rust. They do
not execute pytest, embed Python assertions, or change the production hook.
The Bash parser compatibility API delegates to the existing Forge binary; the
checks below were run against the verified release binary from main `a755941`.

The fixture initializes a repository inside a temporary `Case` root. Worktree
cases create a linked checkout and an unrelated repository inside that same
root; `Case` removes them after each test. The tests never create a checkout of
the actual Forge or host LLM repository. The Bash resolver exception is a
minimal PyO3 callback that preserves the hook's intentional fail-open behavior.

Target abbreviations in the map:

| Abbreviation | Rust target | Cases |
| --- | --- | ---: |
| P | `python_contracts_crg_gate_parser.rs` | 37 |
| C | `python_contracts_crg_gate_claims.rs` | 15 |
| W | `python_contracts_crg_gate_worktrees.rs` | 9 |
| S | `python_contracts_crg_gate_session.rs` | 9 |

The first column names the original pytest case. For the parameterized family,
the command column identifies each independently collected input. All Rust
names below refer to `#[test]` functions in the corresponding target.

| Original case | Input, where parameterized | Rust target and test |
| --- | --- | --- |
| `test_owner_with_live_claim_is_allowed` | | C `owner_with_live_claim_is_allowed` |
| `test_denial_names_the_holder_and_expiry` | | C `denial_names_the_holder_and_expiry` |
| `test_denial_without_any_holder_keeps_plain_message` | | C `denial_without_any_holder_keeps_plain_message` |
| `test_read_only_commands_have_no_write_targets` | | P `read_only_commands_have_no_write_targets` |
| `test_descriptor_duplication_is_not_a_write` | | P `descriptor_duplication_is_not_a_write` |
| `test_interpreter_heredoc_resolves_a_literal_target` | | P `interpreter_heredoc_resolves_a_literal_target` |
| `test_write_content_is_not_mistaken_for_a_path` | | P `write_content_is_not_mistaken_for_a_path` |
| `test_unresolvable_interpreter_write_is_reported_opaque` | | P `unresolvable_interpreter_write_is_reported_opaque` |
| `test_data_heredoc_body_is_not_scanned_for_writes` | | P `data_heredoc_body_is_not_scanned_for_writes` |
| `test_paths_outside_the_repo_are_dropped` | | P `paths_outside_the_repo_are_dropped` |
| `test_verify_bash_denies_a_write_to_another_owners_path` | | C `verify_bash_denies_a_write_to_another_owners_path` |
| `test_verify_bash_allows_a_write_to_a_claimed_path` | | C `verify_bash_allows_a_write_to_a_claimed_path` |
| `test_verify_bash_allows_a_read_only_command` | | C `verify_bash_allows_a_read_only_command` |
| `test_verify_bash_denies_an_unresolvable_write` | | C `verify_bash_denies_an_unresolvable_write` |
| `test_verify_bash_requires_the_graph_call_before_a_write` | | C `verify_bash_requires_the_graph_call_before_a_write` |
| `test_stream_merge_redirect_is_a_write` | | P `stream_merge_redirect_is_a_write` |
| `test_command_local_variable_is_expanded` | | P `command_local_variable_is_expanded` |
| `test_unresolvable_variable_is_reported_opaque` | | P `unresolvable_variable_is_reported_opaque` |
| `test_cd_outside_the_repo_moves_relative_targets` | | P `cd_outside_the_repo_moves_relative_targets` |
| `test_cd_into_the_repo_resolves_against_that_subdirectory` | | P `cd_into_the_repo_resolves_against_that_subdirectory` |
| `test_unresolvable_cd_makes_relative_targets_opaque` | | P `unresolvable_cd_makes_relative_targets_opaque` |
| `test_quoted_heredoc_mention_is_not_a_redirection` | | P `quoted_heredoc_mention_is_not_a_redirection` |
| `test_commands_on_separate_lines_stay_separate` | | P `commands_on_separate_lines_stay_separate` |
| `test_literal_loop_list_expands_to_every_target` | | P `literal_loop_list_expands_to_every_target` |
| `test_computed_loop_list_stays_opaque` | | P `computed_loop_list_stays_opaque` |
| `test_loop_list_hoisted_into_a_variable_still_expands` | | P `loop_list_hoisted_into_a_variable_still_expands` |
| `test_a_path_the_script_only_reads_is_not_a_target` | | P `a_path_the_script_only_reads_is_not_a_target` |
| `test_write_shaped_command_families` | `dd` | P `family_dd` |
| `test_write_shaped_command_families` | `tee` | P `family_tee` |
| `test_write_shaped_command_families` | `git checkout` | P `family_git_checkout` |
| `test_write_shaped_command_families` | `git apply` | P `family_git_apply` |
| `test_write_shaped_command_families` | `patch` | P `family_patch` |
| `test_write_shaped_command_families` | `cp` | P `family_cp` |
| `test_write_shaped_command_families` | `mv` | P `family_mv` |
| `test_write_shaped_command_families` | `install` | P `family_install` |
| `test_write_shaped_command_families` | `ln` | P `family_ln` |
| `test_write_shaped_command_families` | `rsync` | P `family_rsync` |
| `test_write_shaped_command_families` | `truncate` | P `family_truncate` |
| `test_write_shaped_command_families` | `sed --in-place` | P `family_sed` |
| `test_write_shaped_command_families` | `rm -- --weird-name.txt` | P `family_rm_dash_name` |
| `test_write_shaped_command_families` | `bash -c` | P `family_bash_c` |
| `test_write_shaped_command_families` | `bash` heredoc | P `family_bash_heredoc` |
| `test_write_shaped_command_families` | backslash newline | P `family_backslash_newline` |
| `test_unparseable_command_falls_back_to_write_shape` | | P `unparseable_command_falls_back_to_write_shape` |
| `test_verify_bash_fails_open_when_the_resolver_raises` | | C `verify_bash_fails_open_when_the_resolver_raises` |
| `test_a_heredoc_does_not_disturb_the_write_target` | | P `a_heredoc_does_not_disturb_the_write_target` |
| `test_a_sibling_worktree_path_is_claim_relevant` | | W `a_sibling_worktree_path_is_claim_relevant` |
| `test_scratchpad_and_foreign_repos_remain_unclaimable` | | W `scratchpad_and_foreign_repos_remain_unclaimable` |
| `test_a_denied_sibling_worktree_write_is_still_recorded` | | W `a_denied_sibling_worktree_write_is_still_recorded` |
| `test_sibling_worktree_write_is_denied_under_enforcement` | | W `sibling_worktree_write_is_denied_under_enforcement` |
| `test_a_claim_spans_every_worktree` | | W `a_claim_spans_every_worktree` |
| `test_bash_resolves_targets_against_the_worktree_it_runs_in` | | W `bash_resolves_targets_against_the_worktree_it_runs_in` |
| `test_bash_in_a_worktree_is_denied_by_default` | | W `bash_in_a_worktree_is_denied_by_default` |
| `test_enforcement_is_off_only_when_it_is_explicitly_turned_off` | | W `enforcement_is_off_only_when_it_is_explicitly_turned_off` |
| `test_exposure_log_stops_at_its_cap` | | W `exposure_log_stops_at_its_cap` |
| `test_an_allowed_write_stamps_the_claim` | | C `an_allowed_write_stamps_the_claim` |
| `test_a_lapsed_claim_of_our_own_says_so` | | C `a_lapsed_claim_of_our_own_says_so` |
| `test_another_owners_lapsed_claim_does_not_hold_the_path` | | C `another_owners_lapsed_claim_does_not_hold_the_path` |
| `test_a_lane_inherits_a_claim_written_before_lanes_had_names` | | C `a_lane_inherits_a_claim_written_before_lanes_had_names` |
| `test_inheriting_a_vendor_claim_is_logged_so_the_fallback_can_be_retired` | | C `inheriting_a_vendor_claim_is_logged_so_the_fallback_can_be_retired` |
| `test_a_lane_does_not_inherit_another_vendors_claim` | | C `a_lane_does_not_inherit_another_vendors_claim` |
| `test_session_checkout_is_the_worktree_the_session_runs_in` | | S `session_checkout_is_the_worktree_the_session_runs_in` |
| `test_session_checkout_falls_back_to_the_hook_process_cwd` | | S `session_checkout_falls_back_to_the_hook_process_cwd` |
| `test_session_checkout_ignores_a_cwd_in_another_repository` | | S `session_checkout_ignores_a_cwd_in_another_repository` |
| `test_the_lane_is_named_for_the_session_not_for_the_hooks_root` | | S `the_lane_is_named_for_the_session_not_for_the_hooks_root` |
| `test_a_denial_tells_the_lane_how_to_unblock_itself` | | S `a_denial_tells_the_lane_how_to_unblock_itself` |
| `test_a_local_denial_also_carries_the_remedy` | | S `a_local_denial_also_carries_the_remedy` |
| `test_a_hook_outside_a_checkout_names_the_root` | | S `a_hook_outside_a_checkout_names_the_root` |
| `test_an_unresolvable_cwd_is_not_guessed_at` | | S `an_unresolvable_cwd_is_not_guessed_at` |
| `test_main_names_the_owner_from_the_sessions_checkout` | | S `main_names_the_owner_from_the_sessions_checkout` |

Each target passed with `cargo +1.98.0 test --offline --locked --manifest-path
native/conductor-native/Cargo.toml --features python-compat-tests --test
<target> -- --test-threads=1`, `FORGE_BIN` pointing at
`native/forge/target/release/forge`, and the repository's existing isolated
PyO3 environment. Scoped Clippy passed with `-D warnings`. Rustfmt used
`+1.98.0 --edition 2021 --config skip_children=true`.

The former Python test has no runtime imports. Historical mutation campaign
manifests and receipts, the duplication baseline, and the earlier migration
inventory still name it as evidence from their original runs; those are
records, not active test imports, and remain unchanged.
