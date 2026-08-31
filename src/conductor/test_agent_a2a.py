from __future__ import annotations

import contextlib
import json
import os
import socket
import subprocess
import sys
import time
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import httpx
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
    flush_queued,
    init_registry,
    list_peers,
    load_registry,
    send_message,
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


def _gate_payload(gate: int | float = 3) -> dict[str, Any]:
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


def test_send_receipt_omits_raw_body_and_structured_data(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    init_registry(tmp_path, name="tester-a", port=7398)
    init_registry(tmp_path, name="tester-b", port=7399)
    monkeypatch.setattr(
        a2a,
        "fetch_card",
        lambda _record, timeout: {
            "name": "tester-b",
            "skills": [{"id": "coordination-v2"}],
        },
    )
    monkeypatch.setattr(a2a, "_deliver_wire", lambda *_args, **_kwargs: None)
    private_body = "receipt-must-not-echo-this-body"
    data = _coordination_payload(summary="Public bounded summary")

    receipt = send_message(
        "tester-a",
        "tester-b",
        private_body,
        data,
        tmp_path,
    )
    rendered = json.dumps(receipt, ensure_ascii=False, sort_keys=True)

    assert receipt["authority"] == "a2a-delivery-receipt"
    assert "body" not in receipt
    assert "data_json" not in receipt
    assert private_body not in rendered
    assert receipt["body_bytes"] == len(private_body.encode("utf-8"))
    assert len(receipt["body_sha256"]) == 64


def test_coordination_v2_rejects_unadvertised_peer_before_recording(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    init_registry(tmp_path, name="tester-a", port=7398)
    init_registry(tmp_path, name="tester-b", port=7399)
    monkeypatch.setattr(
        a2a,
        "fetch_card",
        lambda _record, timeout: {
            "name": "tester-b",
            "skills": [{"id": "coordination"}],
        },
    )
    delivered: list[str] = []
    monkeypatch.setattr(
        a2a, "_deliver_wire", lambda *_args, **_kwargs: delivered.append("sent")
    )

    with pytest.raises(A2aError, match="does not advertise coordination-v2"):
        send_message(
            "tester-a",
            "tester-b",
            "bounded body",
            _coordination_payload(),
            tmp_path,
        )

    assert delivered == []
    store = A2aStore(tmp_path, "tester-a")
    with store.connect() as connection:
        assert connection.execute("SELECT 1 FROM messages").fetchone() is None


def test_coordination_v2_uses_advertised_peer_card_once(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    init_registry(tmp_path, name="tester-a", port=7398)
    init_registry(tmp_path, name="tester-b", port=7399)
    card = {
        "name": "tester-b",
        "skills": [{"id": "coordination"}, {"id": "coordination-v2"}],
    }
    fetched: list[str] = []

    def fake_fetch(record: AgentRecord, timeout: float) -> dict[str, Any]:
        fetched.append(record.name)
        return card

    delivered_cards: list[dict[str, Any] | None] = []

    def fake_delivery(*_args: Any, card: dict[str, Any] | None = None) -> None:
        delivered_cards.append(card)

    monkeypatch.setattr(a2a, "fetch_card", fake_fetch)
    monkeypatch.setattr(a2a, "_deliver_wire", fake_delivery)

    receipt = send_message(
        "tester-a",
        "tester-b",
        "bounded body",
        _coordination_payload(),
        tmp_path,
    )

    assert receipt["delivery_status"] == "delivered"
    assert fetched == ["tester-b"]
    assert delivered_cards == [card]


def test_legacy_send_does_not_require_coordination_v2_advertisement(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    init_registry(tmp_path, name="tester-a", port=7398)
    init_registry(tmp_path, name="tester-b", port=7399)
    monkeypatch.setattr(
        a2a,
        "fetch_card",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(
            AssertionError("legacy send must not preflight coordination-v2")
        ),
    )
    delivered_cards: list[dict[str, Any] | None] = []

    def fake_delivery(*_args: Any, card: dict[str, Any] | None = None) -> None:
        delivered_cards.append(card)

    monkeypatch.setattr(a2a, "_deliver_wire", fake_delivery)

    receipt = send_message(
        "tester-a",
        "tester-b",
        "legacy body",
        {"kind": "coordination"},
        tmp_path,
    )

    assert receipt["delivery_status"] == "delivered"
    assert delivered_cards == [None]


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


def _assert_registry_init_is_idempotent_and_private(tmp_path: Path) -> None:
    first = init_registry(tmp_path)
    second = init_registry(tmp_path)
    assert set(first) == set(second)
    for name in first:
        assert first[name].token == second[name].token
        assert first[name].port == second[name].port
    stat = (tmp_path / "agents.json").stat()
    assert stat.st_mode & 0o077 == 0
    # An init-extended registry still parses and preserves custom ports.
    path = tmp_path / "agents.json"
    payload = json.loads(path.read_text())
    payload["agents"]["tester-z"] = {"port": 7397, "token": "z" * 24}
    path.write_text(json.dumps(payload))
    records = load_registry(tmp_path)
    assert records["tester-z"].port == 7397


def test_bind_failure_leaves_existing_registry_byte_exact(tmp_path: Path) -> None:
    _assert_registry_init_is_idempotent_and_private(tmp_path / "registry-init")
    bind_state = tmp_path / "bind-failure"
    with socket.socket() as occupied:
        occupied.bind(("127.0.0.1", 0))
        occupied.listen()
        port = int(occupied.getsockname()[1])
        before_record = init_registry(bind_state, name="busy-agent", port=port)[
            "busy-agent"
        ]
        registry = bind_state / "agents.json"
        before = registry.read_bytes()

        with pytest.raises(A2aError, match="cannot bind"):
            a2a.serve("busy-agent", bind_state)

    assert registry.read_bytes() == before
    after_record = load_registry(bind_state)["busy-agent"]
    assert after_record.generation == before_record.generation
    assert after_record.token == before_record.token


def test_reap_does_not_remove_a_new_registration_generation(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    live_server: tuple[str, int, str, Path],
) -> None:
    name = "restarted-agent"
    port = 7396
    original = init_registry(tmp_path, name=name, port=port)[name]
    old_fingerprint = a2a._registration_fingerprint(original)
    (tmp_path / "liveness.json").write_text(
        json.dumps(
            {
                "schema_version": 1,
                "agents": {
                    name: {
                        "consecutive_failures": 50,
                        "last_probe_at": "2026-08-30T00:00:00+00:00",
                        "registration_fingerprint": old_fingerprint,
                    }
                },
            }
        ),
        encoding="utf-8",
    )
    rotated: list[AgentRecord] = []

    def stale_probe_results(_state_dir: Path) -> list[dict[str, Any]]:
        replacement = init_registry(
            tmp_path,
            name=name,
            port=port,
            renew_generation=True,
        )[name]
        rotated.append(replacement)
        return [
            {
                "name": name,
                "port": port,
                "registration_fingerprint": old_fingerprint,
                "status": "down",
                "reason": "stale failed probe",
                "consecutive_failures": 50,
            }
        ]

    monkeypatch.setattr(a2a, "list_peers", stale_probe_results)

    result = a2a.reap_registry(tmp_path, consecutive_failures=3)

    current = load_registry(tmp_path)[name]
    assert result["reaped"] == []
    assert rotated and current.generation == rotated[0].generation
    assert current.generation != original.generation
    assert current.token == original.token
    monkeypatch.undo()
    _assert_live_transport_scenarios(live_server)


def _free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def _write_registry(state_dir: Path, agents: dict[str, tuple[int, str]]) -> None:
    state_dir.mkdir(parents=True, exist_ok=True)
    payload = {
        "schema_version": 1,
        "agents": {
            name: {"port": port, "token": token}
            for name, (port, token) in agents.items()
        },
    }
    (state_dir / "agents.json").write_text(json.dumps(payload))


@pytest.fixture()
def live_server(tmp_path: Path) -> Iterator[tuple[str, int, str, Path]]:
    """A real serve subprocess on an ephemeral port plus its registry peer."""
    port = _free_port()
    down_port = _free_port()
    _write_registry(
        tmp_path,
        {"alice": (port, "a" * 24), "bob": (down_port, "b" * 24)},
    )
    log = tmp_path / "serve.log"
    process = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "conductor.agent_a2a",
            "--state-dir",
            str(tmp_path),
            "serve",
            "--name",
            "alice",
        ],
        cwd=os.getcwd(),
        stdout=log.open("w"),
        stderr=subprocess.STDOUT,
    )
    deadline = time.monotonic() + 20.0
    url = f"http://127.0.0.1:{port}{AGENT_CARD_WELL_KNOWN_PATH}"
    while time.monotonic() < deadline:
        try:
            if httpx.get(url, timeout=0.5).status_code == 200:
                break
        except httpx.HTTPError:
            time.sleep(0.2)
    else:
        process.terminate()
        raise AssertionError(f"server never came up: {log.read_text()}")
    yield "alice", port, "bob", tmp_path
    process.terminate()
    process.wait(timeout=10)


def _assert_live_transport_scenarios(
    live_server: tuple[str, int, str, Path],
) -> None:
    up, port, down, state_dir = live_server

    peers = {peer["name"]: peer for peer in list_peers(state_dir)}
    assert peers[up]["status"] == "up"
    assert peers[up]["port"] == port
    assert {skill for skill in peers[up]["skills"]} >= {"coordination"}
    assert peers[down]["status"] == "down"
    assert peers[down]["reason"]

    row = send_message(
        from_name=down,
        to_name=up,
        body="gate 3 review please",
        data_payload=_gate_payload(),
        state_dir=state_dir,
    )
    assert row["delivery_status"] == "delivered"
    assert row["status_reason"] is None
    inbox = A2aStore(state_dir, up).rows(unread_only=True, limit=10)
    assert [r["message_id"] for r in inbox] == [row["message_id"]]
    assert inbox[0]["sender"] == down
    assert json.loads(inbox[0]["data_json"]) == _gate_payload()

    self_row = send_message(
        from_name=up,
        to_name=up,
        body="self check",
        data_payload=None,
        state_dir=state_dir,
    )
    assert self_row["delivery_status"] == "delivered"
    store = A2aStore(state_dir, up)
    with store.connect() as connection:
        both = connection.execute(
            "SELECT direction FROM messages WHERE message_id=?",
            (self_row["message_id"],),
        ).fetchall()
    assert {r["direction"] for r in both} == {"inbound", "outbound"}
    unread_ids = {
        message["message_id"] for message in store.rows(unread_only=True, limit=10)
    }
    assert self_row["message_id"] in unread_ids

    with pytest.raises(A2aError, match="peer unreachable"):
        send_message(
            from_name=up,
            to_name=down,
            body="anyone there?",
            data_payload=None,
            state_dir=state_dir,
            queue_on_unreachable=False,
        )
    rows = A2aStore(state_dir, up).counts()
    assert rows.get("failed") == 1
    outbound = A2aStore(state_dir, up)
    with outbound.connect() as connection:
        failed = connection.execute(
            "SELECT status_reason FROM messages WHERE delivery_status='failed'"
        ).fetchone()
    assert failed and failed["status_reason"]
    _assert_live_queue_and_flush(up, down, state_dir, store)


def _assert_live_queue_and_flush(
    up: str, down: str, state_dir: Path, store: A2aStore
) -> None:
    q1 = send_message(up, down, "anyone there?", None, state_dir)
    assert q1["delivery_status"] == "queued"
    assert "peer unreachable" in q1["status_reason"]
    assert A2aStore(state_dir, up).counts().get("queued") == 1

    q2 = send_message(up, down, "queued 2", None, state_dir)
    assert q2["delivery_status"] == "queued"
    down_port = load_registry(state_dir)[down].port
    with _spawn_serve(state_dir, down, down_port):
        results = flush_queued(state_dir, from_name=up)
        assert [result["status"] for result in results] == [
            "delivered",
            "delivered",
        ]
        down_inbox = A2aStore(state_dir, down).rows(unread_only=True, limit=10)
    down_ids = [message["message_id"] for message in down_inbox]
    assert down_ids.index(q1["message_id"]) > down_ids.index(q2["message_id"])
    assert A2aStore(state_dir, up).counts().get("queued") is None

    backlog = send_message(up, down, "backlog", None, state_dir)
    assert backlog["delivery_status"] == "queued"
    with _spawn_serve(state_dir, down, down_port):
        fresh = send_message(up, down, "fresh", None, state_dir)
        assert fresh["delivery_status"] == "delivered"
        final_inbox = A2aStore(state_dir, down).rows(unread_only=True, limit=10)
    with store.connect() as connection:
        statuses = {
            message["message_id"]: message["delivery_status"]
            for message in connection.execute(
                "SELECT message_id, delivery_status FROM messages "
                "WHERE direction='outbound'"
            )
        }
    assert statuses[backlog["message_id"]] == "delivered"
    final_ids = [message["message_id"] for message in final_inbox]
    assert final_ids.index(backlog["message_id"]) > final_ids.index(fresh["message_id"])


@contextlib.contextmanager
def _spawn_serve(state_dir: Path, name: str, port: int) -> Iterator[None]:
    """Run a second live serve subprocess until the context exits."""
    log = state_dir / f"serve-{name}.log"
    process = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "conductor.agent_a2a",
            "--state-dir",
            str(state_dir),
            "serve",
            "--name",
            name,
        ],
        cwd=os.getcwd(),
        stdout=log.open("w"),
        stderr=subprocess.STDOUT,
    )
    try:
        deadline = time.monotonic() + 20.0
        url = f"http://127.0.0.1:{port}{AGENT_CARD_WELL_KNOWN_PATH}"
        while time.monotonic() < deadline:
            try:
                if httpx.get(url, timeout=0.5).status_code == 200:
                    break
            except httpx.HTTPError:
                time.sleep(0.2)
        else:
            raise AssertionError(f"{name} never came up: {log.read_text()}")
        yield
    finally:
        process.terminate()
        process.wait(timeout=10)
