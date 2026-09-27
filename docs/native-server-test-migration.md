# Native A2A server and registry test migration

`forge mailbox serve` owns the loopback Agent Card, authenticated JSON-RPC
`SendMessage`, and inbound SQLite transaction. `forge mailbox init`, `peers`,
and `reap` own registry writes and liveness state. Python `serve`,
`init_registry`, `list_peers`, and `reap_registry` remain callable entry points
that launch the native command. Session startup launches Forge directly.

| Older Python case | Native or boundary assertion |
| --- | --- |
| `test_agent_a2a.test_rpc_rejects_missing_or_wrong_token` and its card helper | `card_auth_version_method_and_deduplicated_inbound_receipt` checks public card interfaces/skills, missing and wrong token HTTP 401. |
| `test_agent_a2a.test_inbound_is_deduplicated_by_message_id` and receipt/version/method/invalid-data helpers | `card_auth_version_method_and_deduplicated_inbound_receipt` checks the wire response, absent version, rejected legacy method and one inbound row after duplicate sends; `invalid_data_is_rejected_and_supersession_is_atomic` checks rejection before storage. |
| `test_agent_a2a.test_coordination_v2_state_supersedes_same_thread_message` and `test_coordination_v2_rejects_cross_thread_supersession_atomically` | `invalid_data_is_rejected_and_supersession_is_atomic` checks a valid same-thread update and transaction rollback on a different sender. |
| `test_agent_a2a.test_bind_failure_leaves_existing_registry_byte_exact` | `bind_failure_preserves_registration_generation_and_permissions` verifies that a port collision leaves the exact registry bytes in place. |
| `test_agent_a2a.test_reap_does_not_remove_a_new_registration_generation` | `serving_rotates_generation_and_stale_liveness_cannot_reap_it` checks token continuity, generation rotation after bind, and preservation against prior-generation liveness. The native `reap` implementation also compares the current registry and liveness fingerprints while holding the registry lock. |
| `test_agent_a2a` registry idempotency/private-file helper | `native_init_preserves_public_records_permissions_ports_and_rotation` checks the Python facade and 0600 registry/lock files. |
| `test_agent_a2a` live self-send and transport tail | `native_sender_self_send_keeps_two_directional_facts` checks native send to the live native server, with outbound and inbound rows under the same ID. The outbound retry and queue assertions remain mapped in `native-delivery-test-migration.md`. |
| `test_a2a_session_start.test_ensure_serve_lifecycle` detached command | `detached_serve_command_uses_forge_binary_directly` checks the Python command boundary; the native bind/card tests check the actual endpoint. |
| Python registry failure and peer/reap result behavior | `native_registry_launch_failures_translate_to_a2a_error` and `native_peers_and_reap_keep_python_result_schema_and_streak` check native errors and liveness data through public Python APIs. `registry_reap_requires_failure_streak_and_keeps_mailbox` tests the native CLI. |
| `test_a2a_session_start.test_flush_is_always_sender_scoped` | `identity_commands_and_bounds_keep_startup_scoped` checks exact flush argv; `real_compact_preview_withholds_raw_body_and_flushes_only_sender` verifies a queued sender gets exit 3 through the real command. |
| `test_a2a_session_start.test_preview_failure_never_falls_back_to_full_inbox` | `failed_compact_command_never_requests_full_inbox` records one compact subprocess call and checks the error; `real_compact_preview_withholds_raw_body_and_flushes_only_sender` checks the actual bounded output; `preview_enforces_serialized_character_budget` checks 500/256 character limits. |
| `test_a2a_session_start.test_compact_envelope_rejects_untrusted_shapes` and `test_compact_envelope_rejects_raw_content_keys_recursively` | `compact_envelope_rejects_malformed_metadata_and_nested_raw_content` exercises the valid schema, every malformed field class, count/raw-byte invariants, and all three nested raw-content keys. |
| `test_a2a_session_start.test_ensure_serve_lifecycle` | `ensure_serve_starts_native_endpoint_reuses_it_and_rejects_occupied_port` starts a real Forge endpoint, reuses it, checks native launch argv, and rejects an occupied invalid endpoint. |
| `test_a2a_session_start.test_first_invocation_marks_only_a_successful_session` | `once_per_session_marks_success_and_retries_failed_entry` checks marker creation and retry after an exception; `grok_hook_json_uses_user_prompt_event_without_raw_preview` checks the provider event output. |

The mapped native server, registry, and Python boundary tests passed locally.
The old bind and reap cases, including their live-transport tail, were retired
from `test_agent_a2a.py`. Its `build_app` TestClient fixtures still test the
public Python ASGI builder, and its `A2aStore`/CLI cases still test live Python
behavior. Those cases remain until equivalent Rust assertions pass. The
session-start command and endpoint lifecycle now have a separate Rust PyO3
contract suite in `python_contracts_a2a_session.rs`. All eight session contracts
passed against the isolated current extension and Forge binary. The six mapped
Python session test functions were then retired after an import/helper audit.
The endpoint fixture reaps its own child on both success and failed assertions.
