# Native policy contract migration

The three Python suites below have Rust-owned PyO3 contracts. Each Rust target
calls the shipped Python behavior and keeps assertions in Rust. The original
Python files remain in place for independent review. No production Python code,
Cargo manifest, discovery registry, or package metadata changes in this draft.

| Original suite | Expanded Python cases | Rust target | Rust cases |
| --- | ---: | --- | ---: |
| `src/conductor/test_sandbox.py` | 8 | `python_contracts_sandbox` | 8 |
| `src/conductor/test_local_ai_policy.py` | 17 | `python_contracts_local_ai_policy` | 17 |
| `src/conductor/test_policy_path.py` | 14 | `python_contracts_policy_path` | 14 |
| **Total** | **39** | | **39** |

The mapping preserves each named test and each parameterized expansion:

| Python case | Rust case(s) |
| --- | --- |
| `test_export_takes_only_the_named_paths` | `export_takes_only_the_named_paths` |
| `test_export_is_frozen_at_the_commit` | `export_is_frozen_at_the_commit` |
| `test_export_replaces_a_previous_sandbox` | `export_replaces_a_previous_sandbox` |
| `test_export_refuses_an_empty_path_list` | `export_refuses_an_empty_path_list` |
| `test_export_fails_loudly_on_an_unknown_commit` | `export_fails_loudly_on_an_unknown_commit` |
| `test_main_prints_the_sandbox_path` | `main_prints_the_sandbox_path` |
| `test_main_reports_failure_without_a_traceback` | `main_reports_failure_without_a_traceback` |
| `test_default_root_prefers_the_session_scratchpad` | `default_root_prefers_the_session_scratchpad` |
| `test_clerical_classes_accept_low_risk_prompts` | `clerical_classes_accept_low_risk_prompts` (all four classes) |
| `test_clerical_label_cannot_hide_authority_request`, five prompt parameters | `clerical_label_cannot_hide_approval`, `_conduct`, `_verdict`, `_resume`, `_permission` |
| `test_hook_requires_explicit_clerical_class_for_local_chat` | `hook_requires_explicit_clerical_class_for_local_chat` |
| `test_hook_allows_classified_low_risk_local_chat` | `hook_allows_classified_low_risk_local_chat` |
| `test_hook_denies_authority_request_even_when_classified` | `hook_denies_authority_request_even_when_classified` |
| `test_local_agent_runtime_cannot_send_approval_verdict` | `local_agent_runtime_cannot_send_approval_verdict` |
| `test_frontier_runtime_command_is_not_misclassified_as_local` | `frontier_runtime_command_is_not_misclassified_as_local` |
| `test_hook_ignores_mentions_embeddings_and_non_inference_commands` | `hook_ignores_mentions_embeddings_and_non_inference_commands` |
| `test_every_agent_shell_hook_reaches_shared_policy`, four agent parameters | `codex_shell_hook_reaches_shared_policy`, `claude_shell_hook_reaches_shared_policy`, `qwen_shell_hook_reaches_shared_policy`, `grok_shell_hook_reaches_shared_policy` |
| `test_qwen_clerk_hook_declares_local_runtime` | `qwen_clerk_hook_declares_local_runtime` |
| `test_explicit_flag_beats_environment` | `explicit_flag_beats_environment` |
| `test_environment_beats_the_default` | `environment_beats_the_default` |
| `test_empty_flag_and_blank_environment_mean_default` | `empty_flag_and_blank_environment_mean_default` |
| `test_default_is_the_enclosing_repo_from_a_subdirectory` | `default_is_the_enclosing_repo_from_a_subdirectory` |
| `test_default_outside_any_repo_is_the_package_policy` | `default_outside_any_repo_is_the_package_policy` |
| `test_repo_without_a_policy_falls_through_to_the_package` | `repo_without_a_policy_falls_through_to_the_package` |
| `test_missing_explicit_or_environment_path_fails_loud` | `missing_explicit_or_environment_path_fails_loud` |
| `test_tree_default_is_joined_to_the_tree` | `tree_default_is_joined_to_the_tree` |
| `test_tree_explicit_and_environment_are_tree_relative` | `tree_explicit_and_environment_are_tree_relative` |
| `test_tree_rejects_paths_that_escape_the_candidate`, three path parameters | `tree_rejects_absolute_path`, `tree_rejects_parent_path`, `tree_rejects_nested_parent_escape` |
| `test_tree_without_a_policy_never_falls_back_to_the_package` | `tree_without_a_policy_never_falls_back_to_the_package` |
| `test_enclosing_repo_stops_at_the_nearest_git_marker` | `enclosing_repo_stops_at_the_nearest_git_marker` |

The sandbox tests run `git init`, `git commit`, and `git archive` only inside a
`Case` temporary directory. Their destination directories are also temporary;
they do not register a worktree or touch host claims/jobs. The hook matrix is
built by `python_contracts/hook_matrix_fixture.rs` in that temporary directory:
it writes all four agent configs and scripts, copies the shipped graph-gate body,
and uses a native child launcher for the gate and vault-mirror executable paths.
It calls the production `save_active_state` function with the temporary root and
restores the patched root and environment through Rust guards. Policy-path tests
restore process environment and cwd through `Case`.
No migrated target imports one of the three original test modules.

The native targets use `python_contracts/support.rs` for isolated `Case`, Python
module import, `Path`, and exception assertions. Sandbox CLI capture uses
`python_contracts/agent_comm_support.rs::capture` and `buffer_text`. No Python
callback is introduced, so the callback signature helper is not needed. The
runtime provider is the Forge Python environment and production `conductor`
modules. The hook matrix does not import `pytest` or `conductor.conftest`.
These three targets do not import or require `slop-core` directly.

Before running the original suites, their fixtures and production calls were
audited for writes. The bounded CPU-only Python baseline passed 39/39 cases and
is recorded at `/tmp/forge-native64-policy-baseline.log`. The exact native
targets passed 39/39 under `--features python-compat-tests`, with two Cargo
workers and CUDA masked; the log is `/tmp/forge-native64-policy-cargo-test.log`.
Scoped Clippy evidence is `/tmp/forge-native64-policy-clippy.log`. After the
native hook fixture replaced the executable Python fixture dependency, the
same three Rust targets passed 39/39 with `python-compat-tests,bundled-sqlite`.
That run used the freshly built native extension on `PYTHONPATH` because the
installed environment held an older extension. Its log is
`/tmp/forge-native64-policy-native-fixture-test.log`; scoped Clippy with
`-D warnings` for those targets and `hook_matrix_child` is at
`/tmp/forge-native64-policy-native-fixture-clippy.log`.

The three Python suites were retired after independent assertion review, native
fixture review, and 39 passing native cases. The registry includes each direct
helper, the native child fixture, and production providers. The policy-path
tests bind the production `POLICY_ENV` constant once, matching the original
import rather than hardcoding the environment name.
