# Native graph context relationships and Markdown

`conductor.graph_context` now delegates graph relationship reads, syntactic
caller discovery, test-path classification, and Markdown grouping to the
interpreter-free Rust core in `native/conductor-native/src/graph_context*.rs`.
The Python module converts the core's JSON rows back to its existing frozen
`GraphRelationship` dataclass and keeps the `FileContextSummary`, CLI, and
`get_file_context` public shapes. The Rust graph reader opens SQLite read-only,
excludes `contains` edges, applies the old distinct/sort/50-row rules, and
supplements graph callers by file with syntactic hits. Ripgrep has the old
five-second limit and a hidden-directory-pruning file scan fallback. Ripgrep's
captured output is capped at 2 MB, and the scanner fails loudly after 64 MB
of source, 100,000 directory entries, or 10,000 hits. The process group is
killed and reaped on timeout or overflow. Corrupt or absent graph databases
produce an unavailable status; neither path calls a
model or changes the graph store.

The native Markdown renderer keeps the three relationship roles, test-file and
test-kind recognition, the 15-item bound per role, and the unavailable-graph
notice. Duplicate Tested By labels are removed after the 15-item selection;
their order is deterministic. Previously Python rendered these labels from a
set, whose iteration order varied with the interpreter hash seed.

`extract_ast_skeleton` and `SignatureStubifier` remain in Python. Their public
output uses CPython `ast.unparse` for canonical signatures, decorators,
annotations, docstrings, and literals. The current Ruff parser has no
compatible statement unparser, so source slicing would change that contract.
This part is explicitly pending a native formatter with golden parity evidence.

Existing Python tests continue to cover the public AST, CLI, dataclasses,
full-flow, graph, fallback, and Markdown APIs. The two former tests that
monkeypatched `sqlite3.connect` were retired because the native graph reader
owns the connection. `native/conductor-native/tests/graph_context.rs` now
checks malformed-database refusal followed by a successful rebuilt-database
read, as well as edge sorting, deduplication, symbol filtering, 50-row limit,
syntactic fallback, test-path classification, and Markdown role bounds. A
repository search found no active consumer of the retired spy classes.
Two private native unit tests exercise output overflow and an inherited stdout
pipe across the short deadline, using fresh PID files and no PATH changes.

| Existing Python case | Native or retained coverage |
| --- | --- |
| `test_query_graph_relationships`, `test_query_graph_with_target_symbol` | Native `graph_edges_are_distinct_ordered_filtered_and_limited_to_fifty`; Python API cases retained |
| `test_find_syntactic_callers` | Native `absent_graph_uses_bounded_syntactic_callers_without_own_or_hidden_file`; Python API case retained |
| `test_format_markdown_context_with_relationships`, `test_format_markdown_binning_survives_test_substring_in_name` | Native `markdown_separates_test_calls_from_normal_calls_and_caps_each_role`; Python API cases retained |
| `test_is_test_path_discriminates_test_files` | Native `test_path_classifier_uses_filename_or_tests_directory`; Python API case retained |
| `test_a_failed_query_still_closes_the_connection` (execute and fetch variants), `test_a_successful_query_closes_the_connection_too` | Retired Python sqlite mocks; native `malformed_database_fails_closed_and_a_rebuilt_database_can_be_read` exercises connection recovery, and bounded-process unit tests exercise resource cleanup |
| AST skeleton, missing-file, full-flow, CLI, and frozen-dataclass cases | Retained in `test_graph_context.py`; skeleton stays with CPython `ast.unparse` |

The focused native graph target passed five tests, and the two private bounded
process unit tests passed. The 68 focused Python graph and hook regressions
passed after the native source move; a final 18-case AST and API run passed
after factoring the pre-existing sync/async stubifier duplication. The scoped
five-file duplicate scan found zero clones, and Clippy passed with warnings
denied. The final directory-entry cap was added after these runs and is part of
the parent's final native and CI verification.

No covering issue appeared in `gh issue list --limit 100` at the start of this
port. There is no new dependency or mutation campaign.
