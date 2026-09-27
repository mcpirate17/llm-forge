# Mutation registry and pycache test migration

The assertions formerly owned by two Python test modules now live in Rust
integration tests. These tests call the remaining Python production APIs through
PyO3 where appropriate; their fixtures, expected values, error checks, and mock
behavior are asserted in Rust. Child-interpreter checks exercise import and
bytecode behavior, including real pytest plugin startup, with Rust checking
their status and output. The tiny pytest host fixture retains its original
mutation-target assertion; it is input to the plugin integration check, not
the retired Python test suite.

| Retired Python case | Rust case | Preserved contract |
| --- | --- | --- |
| `test_mutation_registry_split.py::test_split_moves_every_row_into_one_fragment_per_campaign` | `python_contracts_mutation_registry_split::split_moves_every_row_into_one_fragment_per_campaign` | Reverse input order; exact written fragment paths and JSON; newline; sorted reader order; empty array and unchanged envelope fields. |
| `test_mutation_registry_split.py::test_split_is_idempotent` | `python_contracts_mutation_registry_split::split_is_idempotent` | First fragment path; second run writes nothing; byte-for-byte tree snapshot. |
| `test_mutation_registry_split.py::test_split_refuses_before_writing_anything` | `python_contracts_mutation_registry_split::split_refuses_before_writing_anything` | Duplicate `dup` id, missing manifest string, and conflicting `a` fragment each raise `CampaignError` with the expected message; no premature fragment creation or overwrite. |
| `test_mutation_registry_split.py::test_two_branches_each_adding_a_campaign_merge_cleanly` | `python_contracts_mutation_registry_split::two_branches_each_adding_a_campaign_merge_cleanly` | Local Git commits and merges for campaigns `one`, `two`, `three`, `four`; final native-reader manifest order. |
| `test_mutation_pycache_evict.py::test_evict_now_deletes_every_tag_of_the_named_sources` | `python_contracts_mutation_pycache_evict::evict_now_deletes_every_tag_of_the_named_sources` | Stale plain, `.opt-1`, and `.opt-2` caches are all reported and removed. |
| `test_mutation_pycache_evict.py::test_evict_now_without_both_engine_variables_does_nothing` | `python_contracts_mutation_pycache_evict::evict_now_without_both_engine_variables_does_nothing` | Neither variable and sources-only configurations both return an empty eviction list. |
| `test_mutation_pycache_evict.py::test_a_broken_eviction_fails_closed_on_the_whole_prefix` | `python_contracts_mutation_pycache_evict::a_broken_eviction_fails_closed_on_the_whole_prefix` | A mocked cache mapper raises `TypeError`; eviction returns empty and deletes the marked scratch. |
| `test_mutation_pycache_evict.py::test_the_fail_closed_deletion_requires_the_run_marker` | `python_contracts_mutation_pycache_evict::the_fail_closed_deletion_requires_the_run_marker` | The same raising mapper triggers `RuntimeError` containing `is absent`; the unmarked cache tree survives. |
| `test_mutation_pycache_evict.py::test_the_fail_closed_deletion_requires_a_pycache_tree` | `python_contracts_mutation_pycache_evict::the_fail_closed_deletion_requires_a_pycache_tree` | The same raising mapper triggers `RuntimeError` containing `pycache does not exist`; the marked directory survives. |
| `test_mutation_pycache_evict.py::test_the_fail_closed_deletion_refuses_the_four_forbidden_places` | `python_contracts_mutation_pycache_evict::the_fail_closed_deletion_refuses_the_four_forbidden_places` | `/` → `filesystem root`; fake home → `home directory`; fake Git root → `repository root`; cwd parent → `parent of the working directory`. All survive; a marked ordinary sibling is deleted. |
| `test_mutation_pycache_evict.py::test_the_fail_closed_deletion_refuses_the_working_directory_itself` | `python_contracts_mutation_pycache_evict::the_fail_closed_deletion_refuses_the_working_directory_itself` | `RuntimeError` contains `is the working directory itself`; the marked cwd survives. |
| `test_mutation_pycache_evict.py::test_the_deletion_marker_literal_matches_the_one_the_launcher_writes` | `python_contracts_mutation_pycache_evict::the_deletion_marker_literal_matches_the_one_the_launcher_writes` | Plugin and launcher marker names agree. |
| `test_mutation_pycache_evict.py::test_the_plugin_imports_nothing_of_the_module_under_mutation` | `python_contracts_mutation_pycache_evict::the_plugin_imports_nothing_of_the_module_under_mutation` | A clean child imports only the plugin and reports that `conductor.bytecode_isolation` is absent from `sys.modules`. |
| `test_mutation_pycache_evict.py::test_the_plugin_evicts_at_pytest_startup_before_the_imports` | `python_contracts_mutation_pycache_evict::the_plugin_evicts_at_pytest_startup_before_the_imports` | A child warms `m.py` at value `1`; a same-size rewrite with restored mtime produces a failing pytest child naming stale `1`. A second pytest child loads the plugin via `PYTEST_ADDOPTS=-p`, evicts before collection imports, and passes on value `2`. Rust verifies both exit outcomes and the stale assertion output. |

The historical campaign manifests and receipts under `campaigns/` name the
retired Python paths as provenance. They are preserved. No active source or
test imports either retired test module.
