# Agent context and hook test migration

> **Historical inventory:** This map records an earlier Python test cohort and
> its then-current mappings. The CPU embedding and embedding-contract targets
> listed below have since been retired with the Python broker adapters. Current
> native broker coverage and interfaces are documented in
> [`native-embedding-broker.md`](native-embedding-broker.md); this inventory is
> not a current target list or validation receipt.

This map tracks the exact 40-file agent context and hook cohort below. A
Python test file is retired only after a Rust assertion covers its public
behavior or a cited existing Rust test exercises the same contract. The shared
`src/conductor/conftest.py` remains for other Python consumers.

Conductor files (21):

```text
src/conductor/test_a2a_compaction.py
src/conductor/test_a2a_graph_context.py
src/conductor/test_a2a_retention.py
src/conductor/test_a2a_supervisor.py
src/conductor/test_agent_a2a.py
src/conductor/test_crg_embedding_text.py
src/conductor/test_crg_mcp_probe.py
src/conductor/test_crg_response_shim.py
src/conductor/test_crg_seed_worktree.py
src/conductor/test_crg_server.py
src/conductor/test_crg_server_wiring.py
src/conductor/test_crg_venv_sync.py
src/conductor/test_crg_workspace_tools.py
src/conductor/test_cpu_embed.py
src/conductor/test_embedding_contract.py
src/conductor/test_graph_context.py
src/conductor/test_graph_selection.py
src/conductor/test_graph_test_select.py
src/conductor/test_memory_auto_index.py
src/conductor/test_memory_index_native_streaming.py
src/conductor/test_memory_vectors.py
```

Hook files (19):

```text
src/tooling/hooks/agent/test_crg_gate.py
src/tooling/hooks/agent/test_crg_refresh_state.py
src/tooling/hooks/agent/test_read_budget.py
src/tooling/hooks/claude/test_bash_guard.py
src/tooling/hooks/claude/test_bash_pretooluse_hooks_parity_corpus.py
src/tooling/hooks/claude/test_bash_quiet.py
src/tooling/hooks/claude/test_hook_project_seam.py
src/tooling/hooks/claude/test_post_tool_parity_corpus.py
src/tooling/hooks/claude/test_post_tool_quiet.py
src/tooling/hooks/claude/test_pre_bash_e2e.py
src/tooling/hooks/claude/test_workspace_exposure_parity_corpus.py
src/tooling/hooks/dispatch/conftest.py
src/tooling/hooks/dispatch/test___main__.py
src/tooling/hooks/dispatch/test_doctor.py
src/tooling/hooks/dispatch/test_merge.py
src/tooling/hooks/dispatch/test_native_freshness.py
src/tooling/hooks/dispatch/test_paths.py
src/tooling/hooks/dispatch/test_registry.py
src/tooling/hooks/dispatch/test_runner.py
```

## Completed: A2A compaction boundary

| Retired Python case | Rust contract | Preserved behavior |
| --- | --- | --- |
| `test_a2a_compaction.py::test_mapping_payload_reaches_native_core_and_returns_python_fields` | `python_contracts_agent_a2a_compaction::mapping_payload_normalizes_and_compaction_receipt_is_order_independent` | Mapping normalization, Python dict fields, receipt hash shape, input key-order invariance |
| `test_a2a_compaction.py::test_python_boundary_rejects_non_string_keys_and_non_json_values` | `python_contracts_agent_a2a_compaction::boundary_rejects_nonstring_keys_and_non_json_values` | Typed CompactionError for invalid mapping keys and JSON values |
| `test_a2a_compaction.py::test_native_validation_error_maps_to_compaction_error` | `python_contracts_agent_a2a_compaction::native_errors_map_to_compaction_error_without_losing_reason` | Empty summary and oversized sender errors retain reasons |

The underlying native algorithms remain covered by
`a2a_compaction::tests` in the crate, as documented in
`docs/native-test-migration.md`. The new PyO3 target passed 3/3 tests
with `cargo +1.98.0 test --offline --locked --features python-compat-tests
--test python_contracts_agent_a2a_compaction -- --test-threads=1`.

## Remaining scope

The other 17 Conductor test files and 19 hook-side files in cohort 3 remain
active until their cases are mapped and checked. The dispatch `conftest.py`
will remain until its final Python consumer is retired.

## Completed: A2A retention boundary

| Retired Python case | Rust contract | Preserved behavior |
| --- | --- | --- |
| `test_a2a_retention.py::test_public_api_forwards_exact_scope_and_time_to_native` | `python_contracts_agent_a2a_retention::public_api_forwards_exact_scope_and_time_to_native` | Exact native command, UTC time, grace and limit, output fields, subprocess flags |
| `test_a2a_retention.py::test_bridge_fails_closed_on_invalid_time_and_native_receipt` | `python_contracts_agent_a2a_retention::invalid_time_grace_and_native_authority_fail_closed` | No native call for invalid inputs, typed authority error |
| `test_a2a_retention.py::test_cli_is_preview_by_default_and_preserves_native_errors` | `python_contracts_agent_a2a_retention::cli_previews_by_default_and_preserves_native_error` | Preview default, no apply flag, stderr and exit 2 on native failure |
| `test_a2a_retention.py::test_import_has_no_store_or_scheduler_side_effect` | `python_contracts_agent_a2a_retention::import_does_not_open_store_or_start_scheduler` | Fresh import cannot open SQLite or create state; agent source has no retention scheduler call |

The retention eligibility and transaction logic has separate Forge native
coverage in `native/forge/tests/mailbox_retention.rs`. This Python boundary
target passed 4/4 tests with the pinned offline Cargo and PyO3 environment.

## Native A2A supervisor lifecycle

`forge mailbox supervise` replaces the interpreted supervisor. Its former
PyO3 contract target and Python implementation have been deleted. Ten integration
tests in `native/forge/tests/native_mailbox_supervisor.rs` exercise real native
children and disposable local endpoints: finite expiry, private durable state,
lease exclusion, argument validation, existing versus owned receiver lifetimes,
SIGINT/SIGTERM during flush and receiver readiness, restart budgets, deadline
cancellation under SQLite contention, inotify delivery after an idle period,
queue preservation and unsafe state-path refusal. The transport suite separately
checks explicit sender-lock deferral without consuming queued work. These tests
need no interpreter and send no messages to registered external agents.

## Native presentation and explicit source context

The Python watch, Linux wait wrapper, automatic graph attachment and their two
compatibility targets have been deleted. Eight integration tests in
`native/forge/tests/native_mailbox_watch.rs` verify bounded batches, presentation
versus read state, supersession, explicit replay, signal cancellation, consumer
leases, backpressure, regular-file output and unsafe input refusal using native
processes. Two mixed wrapper tests in `python_contracts_agent_a2a.rs` were
retired after those behaviors moved to the native suite.

Source-context retrieval is an explicit operation through `forge graph refs`
or `forge code inspect`. `native/forge/tests/graph_context.rs` covers contained
references, deduplication, output bounds, symbol context, directed graph edges
and invalid source handling. Watch no longer reads referenced source implicitly.
