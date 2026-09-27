# Repository-index test migration

The 11 declared tests in `src/conductor/test_repo_index.py` are now Rust-owned
compatibility tests in `native/conductor-native/tests/python_contracts_repo_index.rs`.
They call the shipped Python `conductor.repo_index` and `conductor.slop_gate`
surfaces through PyO3. Rust owns the test traversal, assertions, temporary
fixtures, and isolated Git comparison; the import oracle remains Python's
actual `ast.parse`/`ast.walk`, and the naming reference remains Python `re`
with the old ASCII whole-word boundaries. No retired pytest module is imported
or invoked from Rust.

| Retired Python test | Rust replacement and preserved contract |
| --- | --- |
| `test_the_index_resolves_every_import_the_ast_matcher_did` | Same named Rust test parses every remaining live `test_*.py` with Python `ast`, gathers absolute `ImportFrom.module` and every `Import` alias, and requires each mapping in `drivers_for`. A fixed tree asserts the oracle's exact target set, excludes relative imports, skips a syntax-error file, and refuses quoted/commented imports. Independent source-root and live-file inventory checks replace the obsolete `checked > 1000` floor. |
| `test_from_package_import_module_is_resolved` | `from_package_import_module_is_resolved` asserts exact driver sets for separate direct, multiple/aliased, from-module, from-package, nested, and relative imports; no import form is masked by another in the same test file. |
| `test_named_by_agrees_with_the_git_grep_it_replaces` | `named_by_agrees_with_the_whole_word_reference` checks a fixed whole-word/substring pair, then samples 25 sorted live production function names with Python `Random(11)` and compares each indexed result with Python `re` over the independently walked live test files. |
| `test_a_package_is_named_by_its_directory` | `a_package_is_named_by_its_directory` retains all four `dotted_for` path-to-name assertions. |
| `test_the_index_is_not_degenerate` | `the_index_is_not_degenerate` checks an exact five-test fixture inventory, fixture import/name keys and queries, the independent live inventory and its exact indexed file count, and the original live import/name-key richness ratios while Python tests remain. An empty live inventory must have zero keys. |
| `test_the_gate_asks_the_index_and_gets_the_same_answer` | `the_gate_asks_the_index_and_gets_the_same_answer` compares `slop_gate.drivers_for` with the same supplied index on the fixed from-package module. |
| `test_the_cli_reports_the_root_it_actually_resolved` | `the_cli_reports_the_root_it_actually_resolved` passes a lexical `pkg/..` root and checks the printed canonical root, `TestIndex`, and zero status. |
| `test_the_index_sees_untracked_tests_and_git_grep_does_not` | `the_index_sees_untracked_tests_and_git_grep_does_not` initializes Git only in a disposable fixture, verifies Git can find a staged control test, then checks the index finds an untracked probe that `git grep` does not. Git configuration is isolated from the host. |
| `test_the_cli_answers_the_driver_question_it_was_asked` | `the_cli_answers_the_driver_question_it_was_asked` checks exact CLI driver lines for the fixed from-package import. |
| `test_the_cli_answers_the_naming_question_it_was_asked` | `the_cli_answers_the_naming_question_it_was_asked` checks exact CLI whole-word naming lines and excludes the importer-only test. |
| `test_an_impossible_root_is_refused_not_answered_emptily` | `an_impossible_root_is_refused_not_answered_emptily` retains the `NotADirectoryError` contract. |

PR 108's Python CI run passed every one of its 986 live AST import mappings
and failed only the old fixed `> 1000` count assertion as Python test files
were retired. In this checkout, the ten unaffected original cases passed before
retirement; the failing count was recorded from the PR 108 CI log. The Rust
replacement retains exhaustive live mapping and independently guards the
resolved root and indexed inventory, so further Python-test retirement does
not require changing a size constant.

The all-Rust endpoint is also supported: once the live Python test inventory is
empty, its indexed file/key counts and AST-oracle counts must be zero. The fixed
five-test fixture and source-root guards remain mandatory. A separate regression
case checks an existing tree containing production Python but no Python tests;
it must produce an empty index rather than a wrong-root error.

There are no active imports of the retired test module or its helpers.
`src/conductor/conftest.py` retains two inert historical node-id strings in
`HOST_PROJECT_TESTS`; native candidate-policy fixtures and a slop-core test
comment also retain historical path references. They do not import or execute
the retired Python test. No production Python module or shared fixture was
changed for this migration.

The new target is discovered by CI's `python_contracts_*` Cargo pattern. It
passes all 11 migrated tests plus the empty-inventory regression with one test
thread, CUDA masked, and two Cargo jobs.
`FORGE_BIN` pointed to the preserved Forge 0.8.0 release, although this target
does not invoke that binary. Scoped Clippy with warnings
denied, exact-file rustfmt, and file/function size checks pass. The Forge issue
list had no open issue covering this migration.
