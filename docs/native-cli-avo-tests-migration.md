# CLI and AVO contract test migration

The four retired Python modules had 16 named tests and no parametrized cases.
Their assertions now live in two Rust integration targets. Rust builds the
temporary fixtures, invokes existing Python APIs through PyO3, and checks the
results. The bootstrap delegation stub remains a Rust callback replacing
`bootstrap._init_main`. The scaffold test still runs its real hook doctor in a
temporary host checkout. The AVO cases only parse synthetic receipts and update
temporary supervisor state; they never run evaluation, models, or training.

## `src/conductor/test___main__.py` (5 cases)

Rust target: `native/conductor-native/tests/python_contracts_cli_bootstrap.rs`.

| Retired Python case | Rust case | Preserved contract |
| --- | --- | --- |
| `test_no_arguments_prints_usage_and_exits_two` | `no_arguments_prints_usage_and_exits_two` | Empty argv returns 2 and prints the conductor usage to stdout. |
| `test_help_exits_zero` | `help_exits_zero` | `-h` returns 0 and prints the usage to stdout. |
| `test_unknown_subcommand_exits_two` | `unknown_subcommand_exits_two` | `frobnicate` returns 2 and names the unknown subcommand on stderr. |
| `test_every_subcommand_maps_to_a_module_with_a_main` | `every_subcommand_maps_to_a_module_with_a_main` | Every entry in `SUBCOMMANDS` imports and exposes a callable `main`. |
| `test_doctor_is_reachable_through_the_entry_point` | `doctor_is_reachable_through_the_entry_point` | Registry hook settings plus the quiet limit and one-hour cache TTL let `doctor --harness --project-dir ... --home ...` return 0. |

## `src/conductor/test_bootstrap.py` (4 cases)

Rust target: `native/conductor-native/tests/python_contracts_cli_bootstrap.rs`.

| Retired Python case | Rust case | Preserved contract |
| --- | --- | --- |
| `test_bootstrap_main_delegates_argv_to_project_init_main` | `bootstrap_main_delegates_argv_to_project_init_main` | A patched `_init_main` receives exactly `['/some/host', '--force']` once and its 0 exit code propagates. |
| `test_bootstrap_main_returns_project_init_exit_code` | `bootstrap_main_returns_project_init_exit_code` | A patched `_init_main` returning 2 makes `bootstrap.main([])` return 2. |
| `test_bootstrap_subcommand_is_wired_in_conductor_main` | `bootstrap_subcommand_is_wired_in_conductor_main` | `SUBCOMMANDS['bootstrap']` maps to `conductor.bootstrap`. |
| `test_bootstrap_cli_scaffolds_a_real_working_dispatcher` | `bootstrap_cli_scaffolds_a_real_working_dispatcher` | A temporary Git root bootstraps successfully; its launcher is owner-executable, starts with the active Python shebang, and imports the dispatcher entry point. |

## `src/conductor/test_avo_cards.py` (4 cases)

Rust target: `native/conductor-native/tests/python_contracts_avo_workflows.rs`.

| Retired Python case | Rust case | Preserved contract |
| --- | --- | --- |
| `test_load_empty_dir` | `load_empty_dir` | An empty directory loads no receipt rows. |
| `test_render_and_write_pass_receipt` | `render_and_write_pass_receipt` | A synthetic valid PASS receipt yields one PASS row; the written card contains its knowledge-card ID, PASS status, and `battery.py` target. |
| `test_rejects_non_receipt` | `rejects_non_receipt` | A JSON object lacking receipt fields raises `AvoCardsError` with `not an avo_eval receipt`. |
| `test_rejects_invalid_pass_receipt` | `rejects_invalid_pass_receipt` | PASS with `is_valid=false` raises the same typed error and message. |

## `src/conductor/test_avo_supervisor.py` (3 cases)

Rust target: `native/conductor-native/tests/python_contracts_avo_workflows.rs`.

| Retired Python case | Rust case | Preserved contract |
| --- | --- | --- |
| `test_supervisor_records_improved` | `supervisor_records_improved` | Starting from three rejections, an improved step clears stagnation and both reported and persisted rejection counts become 0. |
| `test_supervisor_triggers_stagnation_alert` | `supervisor_triggers_stagnation_alert` | A fourth rejection at patience 4 stagnates, reports count 4, supplies a PIVOT hint, and emits a `stagnation-alert` payload. |
| `test_supervisor_creates_runtime_state_atomically` | `supervisor_creates_runtime_state_atomically` | A first rejection creates JSON state with counter 1 and leaves no matching temporary state file. |

The two exact targets pass with `--features python-compat-tests` and
`-- --test-threads=1`; scoped Clippy passes with `-D warnings`. The production
modules remain intact. No active source or configuration imports a retired
test module. Historical campaign receipts and the grandfathered test-node ID
inventory still name old paths as records. `bootstrap.py` also has a prose
reference to its former test file; it does not import it.
