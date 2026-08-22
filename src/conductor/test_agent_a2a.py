from __future__ import annotations

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

from conductor.agent_a2a import (
    AGENT_CARD_WELL_KNOWN_PATH,
    PROTOCOL_VERSION_1_0,
    TOKEN_HEADER,
    VERSION_HEADER,
    A2aError,
    A2aStore,
    AgentRecord,
    build_app,
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


def _headers(record: AgentRecord) -> dict[str, str]:
    return {TOKEN_HEADER: record.token, VERSION_HEADER: PROTOCOL_VERSION_1_0}


def test_card_is_public_and_declares_jsonrpc_interface(tmp_path: Path) -> None:
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
    record = _record()
    store = A2aStore(tmp_path, record.name)
    headers = {VERSION_HEADER: PROTOCOL_VERSION_1_0}
    if token is not None:
        headers[TOKEN_HEADER] = token
    with _client(record, store) as client:
        response = client.post("/", json=_jsonrpc(_inbound_message()), headers=headers)
    assert response.status_code == 401
    assert "error" in response.json()


def test_send_message_stores_inbound_and_echoes_receipt(tmp_path: Path) -> None:
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

    rows = store.rows(unread_only=True, limit=10)
    assert [row["message_id"] for row in rows] == ["m-1"]
    row = rows[0]
    assert row["direction"] == "inbound"
    assert row["sender"] == "tester-b"
    assert row["recipient"] == record.name
    assert row["body"] == "gate 3 please"
    assert json.loads(row["data_json"]) == _gate_payload()
    assert row["read_at"] is None


def test_send_message_without_version_header_fails_closed(tmp_path: Path) -> None:
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


def test_legacy_method_name_is_rejected(tmp_path: Path) -> None:
    record = _record()
    store = A2aStore(tmp_path, record.name)
    request = _jsonrpc(_inbound_message())
    request["method"] = "message/send"
    with _client(record, store) as client:
        response = client.post("/", json=request, headers=_headers(record))
    error = response.json()["error"]
    assert error["code"] == -32601


def test_invalid_data_part_is_rejected_and_not_stored(tmp_path: Path) -> None:
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
    record = _record()
    store = A2aStore(tmp_path, record.name)
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


def test_mark_read_clears_unread_view(tmp_path: Path) -> None:
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


@pytest.mark.parametrize("gate", [1, 2, 3, 4, 5, 7, 7.0])
def test_validate_data_payload_accepts_supported_gate_review_requests(
    gate: int | float,
) -> None:
    assert validate_data_payload(_gate_payload(gate)) == "gate-review-request"


@pytest.mark.parametrize("gate", [0, 6, 6.0, 8, -1, 6.5])
def test_validate_data_payload_rejects_unsupported_gate_review_requests(
    gate: int | float,
) -> None:
    with pytest.raises(A2aError, match="requires integer gate"):
        validate_data_payload(_gate_payload(gate))


def test_validate_data_payload_accepts_coordination() -> None:
    assert validate_data_payload({"kind": "coordination"}) == "coordination"


@pytest.mark.parametrize(
    "payload",
    [
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
    ],
)
def test_validate_data_payload_fails_closed(payload: Any) -> None:
    with pytest.raises(A2aError):
        validate_data_payload(payload)


def test_registry_init_is_idempotent_and_private(tmp_path: Path) -> None:
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


def test_live_send_roundtrip_delivers_and_journals(
    live_server: tuple[str, int, str, Path],
) -> None:
    up, _port, down, state_dir = live_server
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


def test_live_self_send_journals_both_directions(
    live_server: tuple[str, int, str, Path],
) -> None:
    up, _port, _down, state_dir = live_server
    row = send_message(
        from_name=up,
        to_name=up,
        body="self check",
        data_payload=None,
        state_dir=state_dir,
    )
    assert row["delivery_status"] == "delivered"
    store = A2aStore(state_dir, up)
    with store.connect() as connection:
        both = connection.execute(
            "SELECT direction FROM messages WHERE message_id=?",
            (row["message_id"],),
        ).fetchall()
    assert {r["direction"] for r in both} == {"inbound", "outbound"}
    assert len(store.rows(unread_only=True, limit=10)) == 1


def test_live_offline_peer_fails_fast_with_reason(
    live_server: tuple[str, int, str, Path],
) -> None:
    up, _port, down, state_dir = live_server
    with pytest.raises(A2aError, match="peer unreachable"):
        send_message(
            from_name=up,
            to_name=down,
            body="anyone there?",
            data_payload=None,
            state_dir=state_dir,
        )
    rows = A2aStore(state_dir, up).counts()
    assert rows.get("failed") == 1
    outbound = A2aStore(state_dir, up)
    with outbound.connect() as connection:
        failed = connection.execute(
            "SELECT status_reason FROM messages WHERE delivery_status='failed'"
        ).fetchone()
    assert failed and failed["status_reason"]


def test_live_peers_probe_reports_up_and_down(
    live_server: tuple[str, int, str, Path],
) -> None:
    up, port, down, state_dir = live_server
    peers = {p["name"]: p for p in list_peers(state_dir)}
    assert peers[up]["status"] == "up"
    assert peers[up]["port"] == port
    assert {s for s in peers[up]["skills"]} >= {"coordination"}
    assert peers[down]["status"] == "down"
    assert peers[down]["reason"]
