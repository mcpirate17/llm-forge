# Candidate style and Vulture test migration

The assertions from `src/conductor/candidate_review/test_style_scan.py` and
`src/conductor/candidate_review/test_vulture_baseline_init.py` now live in two
Rust integration targets under `native/conductor-native/tests/`. The targets
call the shipped Python entry points through PyO3 and use Rust assertions for
the results, errors, mock calls, output streams, and file contents. Python is
used only for mock objects, attribute patching, and input fixtures. The source
snippets passed to the scanners are parser input, not test programs.

There were no parameterized rows in either original module. Each of the 34
named Python cases maps to one named Rust case below.

## Style scan: 23 cases

Rust target: `python_contracts_candidate_style`.

| Original Python case | Rust case | Contract retained |
| --- | --- | --- |
| `test_a_changed_range_includes_its_first_line` | `changed_range_includes_first_line` | First changed line is inclusive. |
| `test_a_changed_range_includes_its_last_line` | `changed_range_includes_last_line` | Last changed line is inclusive. |
| `test_a_line_outside_every_range_is_not_in_scope` | `line_outside_every_range_is_not_in_scope` | Out-of-range line is excluded. |
| `test_without_a_base_every_finding_is_kept` | `without_base_every_finding_is_kept` | Full-tree run retains rows and has no unreadable files. |
| `test_a_finding_on_an_unchanged_line_is_dropped` | `finding_on_unchanged_line_is_dropped` | Diff scoping drops unchanged line. |
| `test_a_finding_on_a_changed_line_is_kept` | `finding_on_changed_line_is_kept` | Diff scoping retains changed line. |
| `test_each_file_is_diffed_once_however_many_findings_it_has` | `each_file_is_diffed_once_for_many_findings` | One diff call per file, in file order, with base and cwd. |
| `test_an_undiffable_file_is_reported_rather_than_raised` | `undiffable_file_is_reported_rather_than_raised` | DiffError becomes unreadable message. |
| `test_only_python_files_reach_the_scanner` | `only_python_files_reach_scanner` | Python suffix selection and scanner call arguments. |
| `test_a_candidate_with_no_python_never_builds_the_scanner` | `no_python_file_never_builds_scanner` | Empty selection returns zero without scanner call. |
| `test_version_answers_without_the_native_extension` | `version_answers_without_native_extension` | Version exits zero and prints name without scanner call. |
| `test_an_undiffable_file_fails_the_check` | `undiffable_file_fails_check` | Unreadable diff fails and reports stderr. |
| `test_a_surviving_finding_fails_the_check` | `surviving_finding_fails_check` | Kept finding returns one. |
| `test_findings_scoped_away_leave_the_check_passing` | `scoped_away_findings_leave_check_passing` | Empty scoped result returns zero. |
| `test_findings_are_printed_where_the_gate_captures_them` | `findings_are_printed_on_stderr` | Finding path and line appear on stderr, stdout empty. |
| `test_the_published_rule_list_matches_the_scanner` | `published_python_rule_list_matches_scanner` | Native Python style rules match published prefix. |
| `test_a_swallowed_error_is_reported_under_the_published_rule_name` | `swallowed_error_uses_published_rule_name` | Fallback parser emits the published rule. |
| `test_both_scanners_report_one_pass_down_the_file` | `both_scanners_report_one_pass_down_file` | Mixed scanner rows have exact rule and line order. |
| `test_only_rust_files_reach_the_rust_scanner` | `only_rust_files_reach_rust_scanner` | Rust suffix selection excludes Cargo and Python files. |
| `test_the_language_reaches_the_scanner` | `language_reaches_scanner` | Rust and Python language arguments arrive in order. |
| `test_the_published_rust_rule_list_matches_the_scanner` | `published_rust_rule_list_matches_scanner` | Sorted native Rust rules match published list. |
| `test_a_production_unwrap_is_reported_under_the_published_rule_name` | `production_unwrap_uses_published_rule_name` | Rust unwrap emits the published rule. |
| `test_a_wired_in_endpoint_is_reported_under_the_published_rule_name` | `hardcoded_endpoint_uses_published_rule_and_binding_exemption` | Literal endpoint emits rule; module binding suppresses it. |

## Vulture baseline initialization: 11 cases

Rust target: `python_contracts_candidate_vulture`.

| Original Python case | Rust case | Contract retained |
| --- | --- | --- |
| `test_git_tree_chunks_splits_a_real_oid` | `git_tree_chunks_splits_real_oid_and_uses_exact_command` | Exact Git argv, 40-hex input, five 8-character chunks. |
| `test_git_tree_chunks_raises_on_git_failure` | `git_tree_chunks_raises_on_git_failure` | Git exit failure raises named error and message. |
| `test_git_tree_chunks_raises_when_oid_has_wrong_length` | `git_tree_chunks_raises_on_truncated_oid` | Successful Git exit with short OID still raises. |
| `test_run_vulture_findings_raises_when_vulture_missing` | `run_vulture_findings_raises_when_tool_missing` | Tool lookup uses `vulture`; missing tool raises named error. |
| `test_run_vulture_findings_raises_on_unexpected_exit_code` | `run_vulture_findings_raises_on_unexpected_exit` | Exit one raises named error with code. |
| `test_run_vulture_findings_parses_real_output` | `run_vulture_findings_parses_real_output` | Exit three output produces one path/line finding. |
| `test_run_vulture_findings_builds_the_exact_command` | `run_vulture_findings_builds_exact_command` | Tool lookup, whitelist, full argv and subprocess options. |
| `test_build_baseline_is_an_empty_allowlist_with_a_real_tree` | `build_baseline_is_empty_allowlist_with_real_tree` | Exact baseline schema and empty findings. |
| `test_main_writes_a_schema_valid_baseline_file` | `main_writes_schema_valid_baseline_file` | Zero exit, JSON fields, trailing newline, no debt banner. |
| `test_main_reports_current_findings_as_debt_on_stderr` | `main_reports_current_findings_as_debt_on_stderr` | Zero exit and exact debt banner/finding on stderr. |
| `test_main_reports_analyzer_error_without_writing_a_file` | `main_reports_analyzer_error_without_writing_file` | Exit two, no output file, error on stderr. |

Validation uses the two exact `cargo +1.98.0 test --offline --locked` targets
with `python-compat-tests` and one test thread. Neither retired Python module
has a code import consumer; the Vulture module remains named in a historical
mutation campaign and receipt, which are retained as provenance.
