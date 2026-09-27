# Graph-selection test migration

`src/conductor/test_graph_selection.py` and
`src/conductor/test_graph_test_select.py` contained 20 named tests and 21
executions: the latter parametrized missing and corrupt graph databases. The
cases now live in the dedicated Rust-owned compatibility targets
`python_contracts_graph_selection.rs` and
`python_contracts_graph_test_select.rs`. Each target calls the existing Python
API through PyO3; fixture construction and all assertions are in Rust. Neither
target imports or invokes a retired pytest test module.

The candidate-review target builds the original `Candidate` and `ReviewContext`
shape, including empty tuples, the production policy loader, `manual` surface,
and `fast` profile. It writes the same snapshot Python sources. Assertions
that originally compared with `set()` or `{}` compare with real Python sets or
dicts, preserving container equality.

| Retired `test_graph_selection.py` case | Rust case and preserved contract |
| --- | --- |
| `test_reexported_import_selects_the_test` | Same suffix; package re-export selects the importing test. |
| `test_unrelated_package_import_is_not_selected` | Same suffix; unrelated import equals an empty Python set. |
| `test_aliased_reexport_matches_the_alias` | Same suffix; aliased package surface selects the test. |
| `test_reexport_inside_try_except_is_found` | Same suffix; nested `try` import is found. |
| `test_star_reexport_uses_module_public_names` | Same suffix; star import uses the source's public names. |
| `test_star_reexport_respects_dunder_all` | Same suffix; excluded name yields an empty Python set. |
| `test_relative_import_in_test_does_not_match` | Same suffix; relative import yields an empty Python set. |
| `test_filename_convention_still_selects` | Same suffix; `test_foo.py` remains selected without an import. |
| `test_dotted_module_string_still_selects` | Same suffix; a full dotted path in the test still selects it. |
| `test_surfaces_skip_non_python_and_packageless_sources` | Same suffix; both non-package and non-Python source queries equal empty Python dicts. |
| `test_public_names_excludes_underscored` | Same suffix; exact public function, class, and constant Python set. |
| `test_public_names_prefers_dunder_all` | Same suffix; exact `__all__` Python set. |

The advisory selector target initializes Git and a minimal SQLite graph only
inside a disposable fixture, with isolated Git configuration. Its graph edge
keys use Python `Path.resolve()` even for the intentionally absent source file,
matching the original fixture. The missing/corrupt rows each get a fresh
repository and preserve their error, CLI status, stdout, and stderr checks.

| Retired `test_graph_test_select.py` case | Rust case and preserved contract |
| --- | --- |
| `test_convention_tests_for_path` | `convention_tests_for_path`; matching sibling test is selected. |
| `test_convention_tests_for_self_test` | `convention_tests_for_self_test`; exact Python `True`/`False` singleton results and self-selection. |
| `test_query_graph_tests` | `query_graph_tests`; an absolute graph edge selects the test. |
| `test_graph_unavailable_fails_loud[missing]` | `graph_unavailable_fails_loud_missing`; missing graph raises the error and CLI returns 2 with empty stdout. |
| `test_graph_unavailable_fails_loud[corrupt]` | `graph_unavailable_fails_loud_corrupt`; corrupt graph has the same fail-loud contract. |
| `test_select_tests_for_sources` | `select_tests_for_sources`; exact Python list equality for source selection and empty non-source selection. |
| `test_git_changed_and_untracked_files` | `git_changed_and_untracked_files`; both modified tracked and untracked files appear. |
| `test_run_tests_empty` | `run_tests_empty`; no selected tests prints the notice and returns zero. |
| `test_main_cli` | `main_cli`; JSON mode reports the exact selected list and scope, and plain mode returns zero. |

Before retirement, both Python targets passed all 21 executions. The Rust
targets passed 12 and 9 tests respectively with the pinned Rust toolchain,
offline Cargo, one test thread, at most two Cargo jobs, CUDA masked, and the
existing Forge 0.8.0 baseline path exported as `FORGE_BIN`; these targets do
not invoke that executable. CI discovers both through its `python_contracts_*`
test selector. Scoped Clippy with warnings denied, exact-file rustfmt, and
Forge's file/function caps were checked.

No active code imports either retired module or its private fixtures. The
grandfathered test-nodeid JSON and older migration inventories contain
historical path strings; they remain unchanged. No production Python source,
shared support, or historical evidence was edited. The Forge issue list had
no open issue covering this migration.
