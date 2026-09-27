# Dispatch entrypoint and path tests in Rust

The three dispatch-entrypoint tests and nine path tests now run as Rust-owned
PyO3 contracts. All twelve original executions passed before retirement; both
Rust targets pass with the same assertions and fixture cases. They call the
production Python APIs and never invoke the retired pytest modules.

| Retired Python case | Rust case | Preserved behavior |
| --- | --- | --- |
| `test___main__::test_session_id_reads_the_payload_and_tolerates_bad_json` | `dispatch_main::session_id_reads_the_payload_and_tolerates_bad_json` | Four byte payloads, including malformed JSON, a numeric ID, and an array |
| `test___main__::test_record_hook_timings_writes_one_event_per_outcome` | `dispatch_main::record_hook_timings_writes_one_event_per_outcome` | Two real outcomes and JSONL count, timing, status, session, and event fields |
| `test___main__::test_record_hook_timings_survives_a_telemetry_failure` | `dispatch_main::record_hook_timings_survives_a_telemetry_failure` | Unrestricted failure callback, caught OSError, and stderr warning |
| `test_paths::test_the_projects_own_copy_wins` | `dispatch_paths::the_projects_own_copy_wins` | Existing project file wins, with Python Path equality |
| `test_paths::test_the_installed_package_answers_when_the_project_has_none` | `dispatch_paths::the_installed_package_answers_when_the_project_has_none` | Installed fallback and outside-project boundary |
| `test_paths::test_a_directory_is_not_a_body` | `dispatch_paths::a_directory_is_not_a_body` | Directory cannot shadow a package file |
| `test_paths::test_the_tooling_root_holds_the_tooling_package` | `dispatch_paths::the_tooling_root_holds_the_tooling_package` | Real package-root layout |
| `test_paths::test_the_interpreter_bin_does_not_follow_the_venv_symlink` | `dispatch_paths::the_interpreter_bin_does_not_follow_the_venv_symlink` | Patched executable remains in the venv bin directory |
| `test_paths::test_own_interpreter_names_the_checkouts_python_for_a_foreign_caller` | `dispatch_paths::own_interpreter_names_each_checkouts_python_for_a_foreign_caller` | Main and worktree roots both select their own Python Path |
| `test_paths::test_own_interpreter_is_none_when_the_caller_already_runs_it` | `dispatch_paths::own_interpreter_is_none_when_the_caller_already_runs_it` | No re-execution loop |
| `test_paths::test_own_interpreter_compares_directories_not_resolved_interpreters` | `dispatch_paths::own_interpreter_compares_directories_not_resolved_interpreters` | Real base-interpreter symlink collapse and python3 alias |
| `test_paths::test_own_interpreter_is_none_when_the_checkout_has_no_venv` | `dispatch_paths::own_interpreter_is_none_when_the_checkout_has_no_venv` | Missing venv returns Python None |

Rust target names carry the `python_contracts_` prefix. The path fixtures use
real interpreter symlinks, including the relative `python3 -> python` link;
they preserve executable permissions on the fake base interpreter. Python
Path comparisons and None identity remain explicit. The telemetry failure
callback accepts arbitrary arguments, matching its original fixture.

The original private fixture helpers have no external test consumers.
The production entrypoint cases formerly in `test_runner.py` now run in the Rust-owned dispatch-runner contracts;
the registry migration retires the dispatch conftest's sole launcher skip rule. Historical campaign records
and older migration notes retain original filenames as provenance.

Validation used two Cargo jobs, one test thread, and masked CUDA after checking
CPU, RAM, VRAM, and processes. Both exact targets passed (3 + 9), as did scoped
Clippy with warnings denied and rustfmt. No shared Python environment was
changed, and no production Python algorithm was added.
