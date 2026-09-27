# Repository utility test migration

The notebook vault-path, repository junk-guard, and notes data-guard contracts use Rust-owned
assertions in `native/conductor-native/tests/python_contracts_repo_utilities.rs`.
The target calls the existing public Python compatibility APIs through PyO3;
it does not run pytest or embed Python assertion logic. Production is unchanged.

| Original Python module and case | Rust test |
| --- | --- |
| `test_notebooklm_bundle.py::test_vault_root_derives_from_home_unless_overridden` | `vault_root_derives_from_home_unless_overridden` |
| `test_check_root_junk.py::test_rejects_each_forbidden_glob_at_the_root` | `rejects_each_forbidden_glob_at_the_root` |
| `test_check_root_junk.py::test_ignores_the_same_names_below_the_root` | `ignores_the_same_names_below_the_root` |
| `test_check_root_junk.py::test_allows_the_blessed_do_not_delete_files` | `allows_the_blessed_do_not_delete_files` |
| `test_check_root_junk.py::test_accepts_ordinary_root_config` | `accepts_ordinary_root_config` |
| `test_check_root_junk.py::test_matching_is_case_sensitive` | `matching_is_case_sensitive` |
| `test_check_root_junk.py::test_main_exits_zero_when_nothing_is_forbidden` | `main_exits_zero_when_nothing_is_forbidden` |
| `test_check_root_junk.py::test_main_exits_one_and_names_only_the_offender` | `main_exits_one_and_names_only_the_offender` |
| `test_check_json_in_notes.py::test_rejects_data_at_the_top_level` | `notes_rejects_data_at_the_top_level` |
| `test_check_json_in_notes.py::test_keeps_the_knowledge_tree_writable` | `notes_keeps_the_knowledge_tree_writable` |
| `test_check_json_in_notes.py::test_exempts_subdirectories` | `notes_exempts_subdirectories` |
| `test_check_json_in_notes.py::test_ignores_data_outside_the_notes_tree` | `notes_ignores_data_outside_the_notes_tree` |
| `test_check_json_in_notes.py::test_main_exits_zero_when_nothing_is_forbidden` | `notes_main_exits_zero_when_nothing_is_forbidden` |
| `test_check_json_in_notes.py::test_main_exits_one_and_names_only_the_offender` | `notes_main_exits_one_and_names_only_the_offender` |

All three source modules were under `src/conductor/`. Every original input name and
both empty/nonempty valid CLI invocations remain covered for each guard. The CLI
failure tests check status 1, the offending filename on stderr, and absence of the
permitted filename. Notes CLI tests preserve the `CONDUCTOR_NOTES_ROOT` override.
Vault expectations remain Python `Path` equality against the user's
home-derived default and an isolated environment override. Guards restore argv,
stderr, and the environment after each case, including assertion failure.

The 14 Rust tests passed with `python-compat-tests` and one test thread. CI
discovers the target via its existing `python_contracts_*` selection. Python
source fixture inputs elsewhere are unaffected and remain in the separate test
source metric.
