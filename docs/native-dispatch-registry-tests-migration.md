# Dispatch registry tests in Rust

`python_contracts_dispatch_registry` replaces the fifteen named tests and
nineteen expanded executions in `tooling/hooks/dispatch/test_registry.py`.
The original pytest baseline passed all nineteen before retirement. Rust owns
the inputs, loops, expected values, and assertions; it imports production APIs
through PyO3 without invoking Python test modules.

| Original case (without `test_`) | Preserved assertions |
| --- | --- |
| `settings_template_is_generated_from_the_registry` | Complete template JSON equals generated settings |
| `dispatcher_recognition_handles_shell_quoting_and_rejects_extra_commands` | Five original command/expected-result rows |
| `settings_block_wires_every_event_once_to_the_launcher` | Event ordering, one hook per event, command, timeout, and dispatcher resolution |
| `launcher_is_tracked_and_executable` | Real launcher exists, is nonempty and executable, and has its Python shebang |
| `every_spec_has_a_runnable_body` | Known event, exclusive adapter/argv, callable adapter or executable/nonempty body |
| `names_are_unique` | No duplicate registered names |
| `every_live_command_resolves_to_a_registered_spec` | All sixteen frozen legacy commands resolve; unknown hook does not |
| `natively_served_is_empty_by_default` | Absent environment gives empty frozenset |
| `natively_served_parses_and_trims_the_env_var` | Whitespace and empty entries discarded |
| `native_answers_is_empty_by_default` | Absent environment gives empty dictionary |
| `native_answers_is_empty_when_the_env_var_is_blank` | Blank environment gives empty dictionary |
| `native_answers_parses_the_json_object` | Full nested permission-decision payload equality |
| `native_answers_rejects_malformed_json` | ValueError and original diagnostic |
| `native_answers_rejects_a_non_object_json_value` | ValueError and original diagnostic |
| `matchers` | Both MCP naming conventions, edit/notebook/read distinctions, and wildcard matches |

Rust case names match the originals without their `test_` prefix. The launcher
is tracked in this Forge checkout, so its contract executes here. The retired
pytest collection exception for an uninitialized host project does not apply
to these checkout-specific Rust tests. The adjacent conftest contained only
that retired test's skip rule and had no importers, so it is retired with the
suite. Remaining dispatch pytest suites did not use that rule.

The frozen legacy-command tuple has no external consumers. Its old comment
said eighteen, but the actual fixture contains sixteen entries; all sixteen
are preserved. No live gitignored settings file is read. Historical mutation
records retain old test names as provenance.

The source-to-contract registry includes the production registry and adapters,
the settings template, launcher, both referenced shell bodies, and the shared
Rust fixture helper. The port passed fifteen Rust tests, preserving all nineteen
original executions, with bounded CPU-only execution after resource inspection.
