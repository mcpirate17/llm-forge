# Native agent communication test migration record

This records the historical migration of four focused Python suites to
Rust-owned PyO3 compatibility tests. At migration time, those tests called the
production Python APIs while keeping assertions, input rows, fake callbacks,
local state, and HTTP `TestClient` requests in Rust. No live agent endpoint,
model, or message was used. The original 35 named tests expanded to 43 pytest
invocations, all passing before migration in
`/tmp/forge-cohort-c-python-baseline.log`; the four Rust targets then passed
35/35. These results describe the migration-era Python compatibility tests,
not current coverage. The session-brief adapter and its dedicated compatibility
target have since been retired.

The table is a historical mapping of the retired Python cases to their former
Rust compatibility assertions. The agent A2A, handoff, and local-clerk cases
remain separate from the retired session-brief adapter.

The local-clerk rows below describe the retired Python adapter and its former
compatibility target. The current native interface is documented in
[`native-clerk.md`](native-clerk.md): `forge clerk --host HOST inventory`,
`draft`, and `validate`. The dedicated Python clerk and local-policy targets
have been retired. See the native guide for current validation evidence.

| Retired Python case | Rust case | Contract retained |
| --- | --- | --- |
| `test_agent_a2a.py::test_rpc_rejects_missing_or_wrong_token` | `python_contracts_agent_a2a::rpc_rejects_missing_or_wrong_token_for_both_rows` | Public agent card, skills/interface, missing and wrong token rows, HTTP 401 |
| `test_agent_a2a.py::test_inbound_is_deduplicated_by_message_id` | `python_contracts_agent_a2a::inbound_is_deduplicated_by_message_id` | Receipt fields, inbound row, missing version, legacy method, invalid data, duplicate ID |
| `test_agent_a2a.py::test_default_inbox_is_bounded_json_and_raw_content_requires_full_or_show` | `python_contracts_agent_a2a::default_inbox_is_bounded_json_and_raw_content_requires_full_or_show` | 512-character compact envelope, private body withheld, explicit full and show output |
| `test_agent_a2a.py::test_coordination_v2_state_supersedes_same_thread_message` | `python_contracts_agent_a2a::coordination_v2_state_supersedes_same_thread_message` | Replacement state, thread, response flag, superseded timestamp |
| `test_agent_a2a.py::test_coordination_v2_rejects_cross_thread_supersession_atomically` | `python_contracts_agent_a2a::coordination_v2_rejects_cross_thread_supersession_atomically` | Typed rejection, absent successor, unchanged original state |
| `test_agent_a2a.py::test_resolve_requires_read_and_then_records_lifecycle_state` | `python_contracts_agent_a2a::resolve_requires_read_and_then_records_lifecycle_state` | Read/unread transition, all gate validation rows, invalid payloads, resolve timestamp |
| `test_agent_a2a.py::test_hold_normalizes_reason_and_can_be_cleared` | `python_contracts_agent_a2a::hold_normalizes_reason_and_can_be_cleared` | Whitespace normalization and clearing hold |
| `test_agent_a2a.py::test_watch_once_does_not_present_the_same_message_twice` | `python_contracts_agent_a2a::watch_once_does_not_present_the_same_message_twice` | First presentation, empty second output, unread row retained |
| `test_handoff.py::test_rejects_oversized_body` | `python_contracts_handoff::rejects_oversized_body_for_both_parameter_rows` | At-limit and over-limit rows, typed 12-line error |
| `test_handoff.py::test_rejects_empty_owner` | `python_contracts_handoff::rejects_empty_owner` | Whitespace owner rejection |
| `test_handoff.py::test_append_inserts_newest_first` | `python_contracts_handoff::append_inserts_newest_first` | Header preserved, new entry before old entry |
| `test_handoff.py::test_a_failed_state_refresh_is_reported_not_swallowed` | `python_contracts_handoff::failed_state_refresh_is_reported_without_losing_append` | Durable append and callback with failure class and reason |
| `test_handoff.py::test_a_successful_refresh_reports_nothing` | `python_contracts_handoff::successful_refresh_reports_nothing` | One refresh callback, no error report |
| `test_handoff.py::test_the_cli_separates_a_stale_state_from_a_clean_append` | `python_contracts_handoff::cli_separates_stale_state_from_clean_append` | Distinct stale/clean exit codes and output channels |
| `test_local_clerk.py::test_ollama_endpoint_accepts_only_plain_loopback_origins` | `python_contracts_local_clerk::ollama_endpoint_accepts_only_plain_loopback_origins` | All three loopback origin rows and normalized endpoint tuple |
| `test_local_clerk.py::test_ollama_endpoint_rejects_non_loopback_or_ambient_request_data` | `python_contracts_local_clerk::ollama_endpoint_rejects_non_loopback_or_ambient_request_data` | All five rejected origin and request-data rows |
| `test_local_clerk.py::test_output_paths_and_atomic_writer_never_clobber_existing_files` | `python_contracts_local_clerk::output_paths_and_atomic_writer_never_clobber_existing_files` | Source collision, existing output, byte-preserving failed write |
| `test_local_clerk.py::test_read_source_rejects_a_file_that_changes_during_read` | `python_contracts_local_clerk::read_source_rejects_a_file_that_changes_during_read` | Changed stat identity through the original `fstat` mock seam |
| `test_local_clerk.py::test_multi_source_prompt_is_bounded_complete_and_valid_json` | `python_contracts_local_clerk::multi_source_prompt_is_bounded_complete_and_valid_json` | Prompt size, all source paths, per-source character counts and minimums |
| `test_local_clerk.py::test_draft_provenance_round_trips_and_rejects_tampering` | `python_contracts_local_clerk::draft_provenance_round_trips_and_rejects_tampering` | Fake generation, valid document, tampered digest rejection |
| `test_local_clerk.py::test_clerk_roots_derive_from_home_unless_overridden` | `python_contracts_local_clerk::clerk_roots_derive_from_home_unless_overridden` | Home defaults and explicit path-list override |
| `test_session_brief.py::test_snippet_strips_frontmatter_and_truncates` | `python_contracts_session_brief::snippet_strips_frontmatter_and_truncates` | Frontmatter removal and ellipsis-bound truncation |
| `test_session_brief.py::test_compact_inbox_previews_headers_and_caps_messages` | `python_contracts_session_brief::compact_inbox_previews_headers_and_caps_messages` | Header, preview length, message cap, empty input |
| `test_session_brief.py::test_claims_for_paths_boundary_reports_load_failure` | `python_contracts_session_brief::claims_for_paths_boundary_reports_load_failure` | Failed claim load shown as unavailable |
| `test_session_brief.py::test_claims_for_paths_reports_overlap_or_absence` | `python_contracts_session_brief::claims_for_paths_reports_overlap_or_absence` | Active overlap, unrelated path, empty paths, expired claim |
| `test_session_brief.py::test_inbox_preview_handles_missing_agent_and_failures` | `python_contracts_session_brief::inbox_preview_handles_missing_agent_and_failures` | Exact subprocess request, compact output, empty inbox, nonzero exit |
| `test_session_brief.py::test_inbox_preview_rejects_untrusted_compact_envelopes` | `python_contracts_session_brief::inbox_preview_rejects_untrusted_compact_envelopes` | Every malformed envelope row and invalid JSON refusal |
| `test_session_brief.py::test_inbox_preview_reports_subprocess_failure` | `python_contracts_session_brief::inbox_preview_reports_subprocess_failure` | Timeout rendered as unavailable |
| `test_session_brief.py::test_top_cards_formats_kb_retrieve_hits` | `python_contracts_session_brief::top_cards_formats_kb_retrieve_hits` | Mocked index/query and frontmatter-free card line |
| `test_session_brief.py::test_brief_degrades_cleanly_when_local_indexes_are_missing` | `python_contracts_session_brief::brief_degrades_cleanly_when_local_indexes_are_missing` | Missing KB and memory index with task, mandates, and claims retained |
| `test_session_brief.py::test_kb_and_memory_retrieval_share_one_query_embedding` | `python_contracts_session_brief::kb_and_memory_retrieval_share_one_query_embedding` | One embed call, exact keyword arguments and shared vector identity |
| `test_session_brief.py::test_task_previews_opens_index_read_only` | `python_contracts_session_brief::task_previews_opens_index_read_only` | Read-only SQLite URI and bounded preview |
| `test_session_brief.py::test_build_brief_assembles_sections_and_bounds_size` | `python_contracts_session_brief::build_brief_assembles_sections_and_bounds_size` | Section assembly, truncation, maximum brief length |
| `test_session_brief.py::test_brief_orchestrates_and_main_prints` | `python_contracts_session_brief::brief_orchestrates_and_main_prints` | Mock identity and arguments, CLI brief, stdin compacting, invalid task |
| `test_session_brief.py::test_compact_inbox_passes_already_compact_input_through` | `python_contracts_session_brief::compact_inbox_passes_already_compact_input_through` | Canonical compact form passes without alteration |

At migration time, the focused Cargo test command selected the four exact targets with
`--features python-compat-tests --offline --locked -- --test-threads=1`.
Scoped Clippy with `-D warnings` passed. Historical campaign manifests,
candidate-review node IDs, duplication baselines, and old documentation refer
to the retired names as provenance; no active import or test selector depends
on the Python modules. `src/conductor/conftest.py` remains in use elsewhere.

The migration-era review repairs preserved fixed callback signatures, Python
list and tuple comparisons, exact stored message payloads, and the production
store's connection context. The final four-target run passed all 35 tests after
those repairs, followed by scoped Clippy with warnings denied. Validation used
two Cargo jobs and one test thread after CPU, RAM, VRAM, and process checks.
These commands and results are historical; the retired compatibility target is
not active coverage.

## Current native surfaces

Forge's persistent MCP service runs through native `harness stdio`. It exposes
`code_inspect`, `code_fold`, and `code_search` for inspection, plus
`session_brief` and `workspace_recall` for workspace context. The native CLI
provides session briefs, including `forge session brief --cards-index`, and
mailbox operations such as `forge mailbox show`, `read`, `resolve`, and `watch`.
Native tests now cover these interfaces, including
`native/forge/tests/native_session_brief.rs` and the mailbox lifecycle/watch
tests. They do not invoke the retired Python brief adapter.

## Native startup and provider installation

The Python `a2a_session_start` and `hook_installer` adapters, their dedicated
compatibility targets, and `test_a2a_identity_defaults.py` are retired. Their
replacement interfaces are `forge mailbox --host HOST startup-hook --provider
PROVIDER` and `forge providers --host HOST --provider PROVIDER install`. Install
is a plan until `--apply` is supplied. Existing recognized Python startup hooks
require explicit `--replace-legacy` when installing their native replacements;
deleting the adapters does not rewrite an installed host's settings.

Native startup tests cover receiver readiness and reuse, occupied listeners,
bounded unread previews, identity selection, provider envelopes, and Grok's
once-per-session behavior. Native provider tests cover settings preservation,
explicit legacy replacement, and rollback. These live in
`native/forge/tests/native_mailbox_startup.rs` and `native_providers.rs`.

The CLI transition changes a few installer options: select one provider per
invocation; the native installer uses a native executable and the host's
mailbox directory instead of Python's `--interpreter` and custom `--state-dir`.
Native installation records a rollback snapshot and offers `rollback`; it has
no separate equivalent of Python's standalone `backup` action. The shared
native `hook_installer` library and the Python registry compatibility target
remain in use. The Python session-preamble adapter also remains because the
workspace-runtime caller still depends on it.

## Native fleet status

The native fleet report is available as:

```sh
forge mailbox --host HOST [--state-dir PATH] fleet --as-name IDENTITY [--json] [--processes]
```

This is the native replacement interface for the deleted `conductor.fleet_status`
CLI. It reads the selected identity's inbox without acknowledging messages,
regenerates claims and headings through the read-only active-state reader, and
probes registered Agent Cards without updating mailbox, claim, or liveness
state. The report is an observation with `bounded-fleet-observation` authority;
it does not grant permission or represent an entire message history.
Card observations expose only name, port, status, and optional version/reason
strings truncated to 200 characters. Skills and arbitrary card fields are
discarded. JSON and text output both fail if the final report exceeds 512 KiB.

Peer probing is capped at 512 registry identities, 200 ms per card, and two
seconds total. Identities left after the total budget are reported explicitly
as `unprobed`. The inbox contributes at most eight compact summaries, each
limited to 160 characters, within a 10,000-character preview budget. “Latest
per sender” means latest within those selected summaries, not across the full
stored history.

Process observations are opt-in with `--processes`. They inspect same-user
`/proc` entries matched against configured worktree paths, with caps of 65,536
entries, 32 MiB total command-line data and 4,096 bytes per command line;
displayed command lines are capped at 140 characters. At most 1,024 matching
rows and 512 groups are returned. Coverage includes `disappeared`,
`permission_denied`, `truncated_cmdlines`, `included_rows`, `omitted_rows`, and
`complete`. Text output also discloses that coverage. The command does not
launch `ps`, Python, or another interpreter. Seven native aggregation tests and
three release CLI tests passed, including empty-PATH execution and preservation
of message acknowledgment, registry and liveness state. Eight transitional
Python compatibility tests also passed before the Python fleet module, its
bindings and dedicated target were deleted.

The unused `luna_fleet_launch.py` was also deleted. It depended on fixed historical
research prompt/shard paths that are absent from the current host and released
receivers through an unchecked PID file. Operators select an explicit registered
identity with `forge mailbox start`, or use finite `forge mailbox supervise
--serve` for owned receiver cleanup. Neither command assumes a research pool or
stops an existing receiver it does not own.
