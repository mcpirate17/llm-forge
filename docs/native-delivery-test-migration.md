# Native A2A delivery test migration

The delivery implementation now lives in Forge's `mailbox send`, `flush`, and
`history` commands. `conductor.a2a_delivery` is the Python compatibility entry
point. The tests below name the behavior each older Python delivery test was
covering; the Rust tests use local HTTP fixtures and real SQLite state rather
than patching Python transport functions that are no longer on the call path.

| Older Python case | Native or boundary assertion |
| --- | --- |
| `test_a2a_delivery.test_structured_offline_send_survives_and_rechecks_capability` | `structured_offline_message_rechecks_capability_and_ack_on_retry` verifies stored data; `structured_offline_message_rechecks_skill_and_records_terminal_failure` verifies a fresh card check, no POST to an unsupported peer, and terminal failure. |
| `test_a2a_delivery.test_pending_send_is_recovered_with_same_message_id` | `crash_left_pending_retries_and_structured_preflight_refuses_unsupported_peer` seeds a pending row and verifies its original ID reaches the peer. |
| `test_a2a_delivery.test_new_send_cannot_overtake_offline_backlog` | `offline_send_is_durable_and_flush_retries_same_ids_in_fifo_order` verifies FIFO retry; `new_send_flushes_older_backlog_before_its_own_delivery` covers the send path once the peer returns. |
| `test_a2a_delivery.test_flush_limit_and_sender_lock_bound_work` | `flush_limit_reports_remaining_and_missing_recipient_fails_old_queued_message` checks the bound and remaining exit code; `sender_lock_rejects_concurrent_flush_without_consuming_queue` checks lock refusal. |
| `test_a2a_delivery.test_invalid_wire_response_is_terminal_and_recorded` | `malformed_peer_json_is_terminal_and_records_failure` checks malformed JSON and persisted failure; `invalid_ack_is_terminal_and_no_queue_policy_closes_offline_send` covers an incorrect acknowledgment. |
| `test_a2a_delivery.test_no_queue_policy_does_not_leave_pending_message` | `invalid_ack_is_terminal_and_no_queue_policy_closes_offline_send` checks the terminal state and absence of retryable rows. |
| `test_a2a_delivery.test_structured_queue_delivers_after_capability_check_and_ack` | `structured_offline_message_rechecks_capability_and_ack_on_retry` verifies stored structured data, card check, acknowledgment, and history. |
| `test_a2a_delivery.test_history_does_not_create_store_and_flush_cli_reports_remaining` | `history_of_uninitialized_identity_does_not_create_a_store` and `flush_limit_reports_remaining_and_missing_recipient_fails_old_queued_message` cover both observations. |
| `test_agent_a2a.test_send_receipt_omits_raw_body_and_structured_data` | `native_send_flush_and_history_preserve_python_receipt_contract` checks the Python API, native receipt, UTF-8 byte count, and stored data. |
| `test_agent_a2a.test_coordination_v2_rejects_unadvertised_peer_before_recording` | `crash_left_pending_retries_and_structured_preflight_refuses_unsupported_peer` checks preflight refusal before insertion. |
| `test_agent_a2a.test_coordination_v2_uses_advertised_peer_card_once` | `structured_send_preflights_once_and_stdin_body_reaches_authenticated_wire` expects one card GET and one POST. |
| `test_agent_a2a.test_legacy_send_does_not_require_coordination_v2_advertisement` | `legacy_send_accepts_card_without_v2_skill` checks delivery with a legacy card. |
| `test_agent_a2a.test_reap_does_not_remove_a_new_registration_generation` live transport tail | The registry generation assertion stays in its Python test. The attached send/queue/self-send scenarios need a dedicated native live-server integration test before retiring that tail. |

`native/forge/tests/mailbox_transport.rs` owns wire, retry, FIFO, and store
assertions. `native/conductor-native/tests/python_contracts_a2a_delivery.rs`
owns the Python entry-point contract through PyO3, including `A2aError`
translation for a failed Forge launch. The `python-compat-tests` feature and a
prebuilt `FORGE_BIN` enable that boundary suite in hosted CI.

The 8 cases in `test_a2a_delivery.py` were retired after the mapped native
transport and PyO3 boundary tests passed locally. The old file could no
longer collect because `sender_lock` was removed; its monkeypatches of
`_deliver_wire` and `fetch_card` also no longer intercepted delivery. The
native tests exercise real loopback HTTP and SQLite state, including the
sender lock, FIFO retry, capability recheck, terminal failures, and CLI exit
status. The four obsolete transport cases from `test_agent_a2a.py` were
retired on the same mapping. The Python `A2aStore`, CLI presentation, and
`build_app` checks remain active in `test_agent_a2a.py`.
