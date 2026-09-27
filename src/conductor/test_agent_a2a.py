from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from starlette.testclient import TestClient

from conductor import agent_a2a as a2a
from conductor.agent_a2a import (
    AGENT_CARD_WELL_KNOWN_PATH,
    PROTOCOL_VERSION_1_0,
    TOKEN_HEADER,
    VERSION_HEADER,
    A2aError,
    A2aStore,
    AgentRecord,
    build_app,
    validate_data_payload,
)

FINGERPRINT = "a" * 64


def _record(name: str = "tester-a", port: int = 7399) -> AgentRecord:
    return AgentRecord(name=name, port=port, token="t" * 24)


def _client(record: AgentRecord, store: A2aStore) -> TestClient:
    return TestClient(build_app(record, store))


def _jsonrpc(message: dict[str, Any]) -> dict[str, Any]:
    return {
        "jsonrpc": "2.0",
        "id": "t1",
        "method": "SendMessage",
        "params": {"message": message},
    }


def _inbound_message(
    message_id: str = "m-1",
    sender: str = "tester-b",
    body: str = "gate 3 please",
    data: dict[str, Any] | None = None,
) -> dict[str, Any]:
    # Part in a2a-sdk 1.1.x: `text` is a scalar string, `data` a protobuf Value.
    parts: list[dict[str, Any]] = [{"text": body}]
    if data is not None:
        parts.append({"data": data})
    return {
        "messageId": message_id,
        "role": "ROLE_USER",
        "parts": parts,
        "metadata": {"sender": sender},
    }


def _gate_payload(gate: float = 3) -> dict[str, Any]:
    return {
        "kind": "gate-review-request",
        "gate": gate,
        "fingerprint": FINGERPRINT,
        "artifact_paths": ["research/reports/x/gate_3.json"],
    }


def _coordination_payload(
    *,
    thread_id: str = "thread-1",
    summary: str = "Bounded status",
    status: str = "open",
    requires_response: bool = True,
    supersedes: list[str] | None = None,
) -> dict[str, Any]:
    return {
        "kind": "coordination-v2",
        "thread_id": thread_id,
        "summary": summary,
        "status": status,
        "requires_response": requires_response,
        "supersedes": supersedes or [],
    }


def _headers(record: AgentRecord) -> dict[str, str]:
    return {TOKEN_HEADER: record.token, VERSION_HEADER: PROTOCOL_VERSION_1_0}


def _assert_card_is_public_and_declares_jsonrpc_interface(tmp_path: Path) -> None:
    record = _record()
    store = A2aStore(tmp_path, record.name)
    with _client(record, store) as client:
        response = client.get(AGENT_CARD_WELL_KNOWN_PATH)
    assert response.status_code == 200
    card = response.json()
    assert card["name"] == record.name
    interfaces = card["supportedInterfaces"]
    assert interfaces and interfaces[0]["protocolBinding"] == "JSONRPC"
    assert interfaces[0]["protocolVersion"] == PROTOCOL_VERSION_1_0
    skills = {s["id"] for s in card["skills"]}
    assert {"coordination", "gate-review-request"} <= skills


@pytest.mark.parametrize("token", [None, "wrong-token"])
def test_rpc_rejects_missing_or_wrong_token(tmp_path: Path, token: str | None) -> None:
    if token is None:
        _assert_card_is_public_and_declares_jsonrpc_interface(tmp_path / "card")
    record = _record()
    store = A2aStore(tmp_path / "auth", record.name)
    headers = {VERSION_HEADER: PROTOCOL_VERSION_1_0}
    if token is not None:
        headers[TOKEN_HEADER] = token
    with _client(record, store) as client:
        response = client.post("/", json=_jsonrpc(_inbound_message()), headers=headers)
    assert response.status_code == 401
    assert "error" in response.json()


def _assert_send_message_stores_inbound_and_echoes_receipt(tmp_path: Path) -> None:
    record = _record()
    store = A2aStore(tmp_path, record.name)
    with _client(record, store) as client:
        response = client.post(
            "/",
            json=_jsonrpc(_inbound_message(data=_gate_payload())),
            headers=_headers(record),
        )
    assert response.status_code == 200
    result = response.json()["result"]
    reply = result["message"]
    receipts = [p["data"] for p in reply["parts"] if "data" in p]
    assert receipts and receipts[0]["kind"] == "delivery-receipt"
    assert receipts[0]["message_id"] == "m-1"
    assert receipts[0]["recipient"] == record.name
    assert set(receipts[0]) == {"kind", "message_id", "recipient"}

    rows = store.rows(unread_only=True, limit=10)
    assert [row["message_id"] for row in rows] == ["m-1"]
    row = rows[0]
    assert row["direction"] == "inbound"
    assert row["sender"] == "tester-b"
    assert row["recipient"] == record.name
    assert row["body"] == "gate 3 please"
    assert json.loads(row["data_json"]) == _gate_payload()
    assert row["read_at"] is None


def _assert_send_message_without_version_header_fails_closed(tmp_path: Path) -> None:
    record = _record()
    store = A2aStore(tmp_path, record.name)
    with _client(record, store) as client:
        response = client.post(
            "/",
            json=_jsonrpc(_inbound_message()),
            headers={TOKEN_HEADER: record.token},
        )
    payload = response.json()
    assert "error" in payload
    assert store.rows(unread_only=True, limit=10) == []


def _assert_legacy_method_name_is_rejected(tmp_path: Path) -> None:
    record = _record()
    store = A2aStore(tmp_path, record.name)
    request = _jsonrpc(_inbound_message())
    request["method"] = "message/send"
    with _client(record, store) as client:
        response = client.post("/", json=request, headers=_headers(record))
    error = response.json()["error"]
    assert error["code"] == -32601


def _assert_invalid_data_part_is_rejected_and_not_stored(tmp_path: Path) -> None:
    record = _record()
    store = A2aStore(tmp_path, record.name)
    bad = _gate_payload() | {"gate": "three"}
    with _client(record, store) as client:
        response = client.post(
            "/",
            json=_jsonrpc(_inbound_message(data=bad)),
            headers=_headers(record),
        )
    assert "error" in response.json()
    assert store.rows(unread_only=False, limit=10) == []


def test_inbound_is_deduplicated_by_message_id(tmp_path: Path) -> None:
    _assert_send_message_stores_inbound_and_echoes_receipt(tmp_path / "receipt")
    _assert_send_message_without_version_header_fails_closed(tmp_path / "version")
    _assert_legacy_method_name_is_rejected(tmp_path / "legacy-method")
    _assert_invalid_data_part_is_rejected_and_not_stored(tmp_path / "invalid-data")
    record = _record()
    store = A2aStore(tmp_path / "dedup", record.name)
    with _client(record, store) as client:
        for _ in range(2):
            response = client.post(
                "/",
                json=_jsonrpc(_inbound_message()),
                headers=_headers(record),
            )
            assert response.status_code == 200
    rows = store.rows(unread_only=False, limit=10)
    assert [row["message_id"] for row in rows] == ["m-1"]


def _assert_mark_read_clears_unread_view(tmp_path: Path) -> None:
    store = A2aStore(tmp_path, "tester-a")
    store.record_inbound(
        message_id="m-2",
        sender="tester-b",
        recipient="tester-a",
        body="handoff",
        data_json=None,
    )
    assert len(store.rows(unread_only=True, limit=10)) == 1
    store.mark_read("m-2")
    assert store.rows(unread_only=True, limit=10) == []
    assert len(store.rows(unread_only=False, limit=10)) == 1


def test_default_inbox_is_bounded_json_and_raw_content_requires_full_or_show(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    store = A2aStore(tmp_path, "tester-a")
    private_body = "raw-private-body-" * 100
    data = _coordination_payload(summary="Safe bounded summary")
    store.record_inbound(
        message_id="compact-1",
        sender="tester-b",
        recipient="tester-a",
        body=private_body,
        data_json=json.dumps(data, sort_keys=True),
    )
    base = ["--state-dir", str(tmp_path), "inbox", "--as-name", "tester-a"]

    assert a2a.main([*base, "--json", "--max-chars", "512"]) == 0
    compact_text = capsys.readouterr().out.strip()
    compact = json.loads(compact_text)
    assert len(compact_text) <= 512
    assert compact["authority"] == "bounded-a2a-inbox"
    assert compact["messages"][0]["summary"] == "Safe bounded summary"
    assert private_body not in compact_text
    assert "data_json" not in compact["messages"][0]

    assert a2a.main([*base, "--full", "--json"]) == 0
    full = json.loads(capsys.readouterr().out)
    assert full[0]["body"] == private_body
    assert json.loads(full[0]["data_json"]) == data

    assert (
        a2a.main(
            [
                "--state-dir",
                str(tmp_path),
                "show",
                "--as-name",
                "tester-a",
                "--json",
                "compact-1",
            ]
        )
        == 0
    )
    shown = json.loads(capsys.readouterr().out)
    assert shown["body"] == private_body
    assert json.loads(shown["data_json"]) == data


def test_coordination_v2_state_supersedes_same_thread_message(tmp_path: Path) -> None:
    store = A2aStore(tmp_path, "tester-a")
    store.record_inbound(
        "old-message",
        "tester-b",
        "tester-a",
        "old raw body",
        json.dumps(_coordination_payload(summary="Old status")),
    )
    store.record_inbound(
        "new-message",
        "tester-b",
        "tester-a",
        "new raw body",
        json.dumps(
            _coordination_payload(
                summary="Replacement status",
                status="in_progress",
                supersedes=["old-message"],
            )
        ),
    )

    with store.connect() as connection:
        states = {
            row["message_id"]: row
            for row in connection.execute(
                "SELECT * FROM message_state ORDER BY message_id"
            ).fetchall()
        }

    assert states["new-message"]["thread_id"] == "thread-1"
    assert states["new-message"]["summary"] == "Replacement status"
    assert states["new-message"]["protocol_status"] == "in_progress"
    assert states["new-message"]["requires_response"] == 1
    assert states["old-message"]["protocol_status"] == "superseded"
    assert states["old-message"]["superseded_at"] is not None


def test_coordination_v2_rejects_cross_thread_supersession_atomically(
    tmp_path: Path,
) -> None:
    store = A2aStore(tmp_path, "tester-a")
    store.record_inbound(
        "other-thread-message",
        "tester-b",
        "tester-a",
        "other",
        json.dumps(_coordination_payload(thread_id="thread-other")),
    )

    with pytest.raises(A2aError, match="another sender/thread"):
        store.record_inbound(
            "invalid-successor",
            "tester-b",
            "tester-a",
            "invalid",
            json.dumps(
                _coordination_payload(
                    thread_id="thread-1",
                    supersedes=["other-thread-message"],
                )
            ),
        )

    with store.connect() as connection:
        invalid = connection.execute(
            "SELECT 1 FROM messages WHERE message_id='invalid-successor'"
        ).fetchone()
        original = connection.execute(
            "SELECT protocol_status, superseded_at FROM message_state "
            "WHERE message_id='other-thread-message'"
        ).fetchone()
    assert invalid is None
    assert original["protocol_status"] == "open"
    assert original["superseded_at"] is None


def test_resolve_requires_read_and_then_records_lifecycle_state(tmp_path: Path) -> None:
    _assert_mark_read_clears_unread_view(tmp_path / "mark-read")
    _assert_data_payload_validation_contracts()
    store = A2aStore(tmp_path / "resolve", "tester-a")
    store.record_inbound(
        "resolve-me",
        "tester-b",
        "tester-a",
        "please resolve",
        json.dumps(_coordination_payload()),
    )

    with pytest.raises(A2aError, match="cannot resolve unread"):
        store.resolve("resolve-me")
    store.mark_read("resolve-me")
    store.resolve("resolve-me")

    with store.connect() as connection:
        state = connection.execute(
            "SELECT protocol_status, resolved_at FROM message_state "
            "WHERE message_id='resolve-me'"
        ).fetchone()
    assert state["protocol_status"] == "resolved"
    assert state["resolved_at"] is not None


def test_hold_normalizes_reason_and_can_be_cleared(tmp_path: Path) -> None:
    store = A2aStore(tmp_path, "tester-a")
    store.record_inbound(
        "held-message",
        "tester-b",
        "tester-a",
        "retain this",
        json.dumps(_coordination_payload()),
    )

    store.set_hold("held-message", "  awaiting\n independent   review ")
    with store.connect() as connection:
        held = connection.execute(
            "SELECT hold_reason FROM message_state WHERE message_id='held-message'"
        ).fetchone()
    assert held["hold_reason"] == "awaiting independent review"

    store.set_hold("held-message", None)
    with store.connect() as connection:
        cleared = connection.execute(
            "SELECT hold_reason FROM message_state WHERE message_id='held-message'"
        ).fetchone()
    assert cleared["hold_reason"] is None


def test_watch_once_does_not_present_the_same_message_twice(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    store = A2aStore(tmp_path, "tester-a")
    store.record_inbound(
        "watch-once",
        "tester-b",
        "tester-a",
        "watch body",
        json.dumps(_coordination_payload(summary="Watch summary")),
    )
    command = [
        "--state-dir",
        str(tmp_path),
        "watch",
        "--as-name",
        "tester-a",
        "--once",
        "--json",
    ]

    assert a2a.main(command) == 0
    first = json.loads(capsys.readouterr().out)
    assert [message["id"] for message in first["messages"]] == ["watch-once"]

    assert a2a.main(command) == 0
    assert capsys.readouterr().out == ""
    assert len(store.rows(unread_only=True, limit=10)) == 1


def _assert_data_payload_validation_contracts() -> None:
    for gate in [1, 2, 3, 4, 5, 7, 7.0]:
        assert validate_data_payload(_gate_payload(gate)) == "gate-review-request"
    for gate in [0, 6, 6.0, 8, -1, 6.5]:
        with pytest.raises(A2aError, match="requires integer gate"):
            validate_data_payload(_gate_payload(gate))
    assert validate_data_payload({"kind": "coordination"}) == "coordination"
    invalid_payloads: list[Any] = [
        "not-a-dict",
        {"kind": "unknown-kind"},
        {"kind": "gate-review-request"},
        {
            "kind": "gate-review-request",
            "gate": True,
            "fingerprint": FINGERPRINT,
            "artifact_paths": ["a"],
        },
        {
            "kind": "gate-review-request",
            "gate": 3,
            "fingerprint": "a" * 63,
            "artifact_paths": ["a"],
        },
        {
            "kind": "gate-review-request",
            "gate": 3,
            "fingerprint": FINGERPRINT,
            "artifact_paths": [],
        },
        {
            "kind": "gate-review-request",
            "gate": 3,
            "fingerprint": FINGERPRINT,
            "artifact_paths": [3],
        },
    ]
    for payload in invalid_payloads:
        with pytest.raises(A2aError):
            validate_data_payload(payload)
