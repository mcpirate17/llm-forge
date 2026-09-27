# Journal test migration

`native/conductor-native/tests/python_contracts_codex_journal.rs` replaces
`src/conductor/test_codex_journal.py`. Its three Rust test names match the
original Python names without the `test_` prefix:

| Original case | Preserved assertions |
| --- | --- |
| `test_status_lines_passes_pathspecs_and_filters_sensitive_paths` | Exact filtered status list and one exact Git argv containing the pathspec. |
| `test_capped_status_lines_reports_omitted_count` | Exact retained status lines and omission message, including the suggested options. |
| `test_build_entry_accepts_path_scope_and_status_cap` | Branch, short HEAD, both scoped paths, omitted count, and the supplied test-command text in the entry. Unexpected Git arguments fail the fixture. |

All assertions and Git mocks are implemented in Rust. PyO3 calls the existing
Python compatibility API; no Git command or journal write is performed. The
old pytest command string remains presentation input and is never executed.
The target is included by CI's existing `python_contracts_*` selection.
