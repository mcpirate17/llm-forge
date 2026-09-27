"""Durable delivery regressions without network listeners or real peers."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import httpx
import pytest

from conductor import agent_a2a as a2a
from conductor.a2a_delivery import history, sender_lock


@pytest.fixture
def state(tmp_path: Path) -> Path:
    a2a.init_registry(tmp_path, name="sender", port=7401)
    a2a.init_registry(tmp_path, name="peer", port=7402)
    return tmp_path


def payload() -> dict[str, Any]:
    return {
        "kind": "coordination-v2",
        "thread_id": "task-1",
        "summary": "status",
        "status": "open",
        "requires_response": True,
        "supersedes": [],
    }


def offline(*args: Any, **kwargs: Any) -> Any:
    raise httpx.ConnectError("offline")


def test_structured_offline_send_survives_and_rechecks_capability(state, monkeypatch):
    monkeypatch.setattr(a2a, "fetch_card", offline)
    sent = a2a.send_message("sender", "peer", "private-body", payload(), state)
    assert sent["delivery_status"] == "queued"
    store = a2a.A2aStore(state, "sender")
    assert (
        json.loads(store.message(sent["message_id"], "outbound")["data_json"])
        == payload()
    )
    monkeypatch.setattr(
        a2a, "fetch_card", lambda *a, **kw: {"skills": [{"id": "coordination"}]}
    )
    monkeypatch.setattr(
        a2a.httpx,
        "post",
        lambda *a, **kw: pytest.fail("unsupported peer must not receive"),
    )
    rows = a2a.flush_queued(state, from_name="sender")
    assert rows[0]["status"] == "failed"
    with store.connect() as connection:
        events = connection.execute(
            "SELECT status, reason FROM delivery_events ORDER BY event_id"
        ).fetchall()
    assert [row["status"] for row in events] == ["queued", "failed"]
    assert "coordination-v2" in events[-1]["reason"]


def test_pending_send_is_recovered_with_same_message_id(state, monkeypatch):
    store = a2a.A2aStore(state, "sender")
    store.record_outbound("crash-left", "sender", "peer", "body", None)
    delivered = []
    monkeypatch.setattr(
        a2a,
        "_deliver_wire",
        lambda record, sender, mid, *a, **kw: delivered.append(mid),
    )
    assert a2a.flush_queued(state, "sender")[0]["status"] == "delivered"
    assert delivered == ["crash-left"]
    assert not store.queued_outbound()


def test_new_send_cannot_overtake_offline_backlog(state, monkeypatch):
    store = a2a.A2aStore(state, "sender")
    store.record_outbound("earlier", "sender", "peer", "one", None)
    attempted = []

    def delivery(record, sender, mid, *args, **kwargs):
        attempted.append(mid)
        if mid == "earlier":
            raise httpx.ConnectError("peer still down")

    monkeypatch.setattr(a2a, "_deliver_wire", delivery)
    sent = a2a.send_message("sender", "peer", "two", None, state)
    assert sent["delivery_status"] == "queued"
    assert attempted == ["earlier"]
    assert [row["message_id"] for row in store.queued_outbound()] == [
        "earlier",
        sent["message_id"],
    ]


def test_flush_limit_and_sender_lock_bound_work(state, monkeypatch):
    store = a2a.A2aStore(state, "sender")
    for number in range(3):
        store.record_outbound(str(number), "sender", "peer", "body", None)
    monkeypatch.setattr(a2a, "_deliver_wire", lambda *a, **kw: None)
    assert len(a2a.flush_queued(state, "sender", max_messages=2)) == 2
    assert len(store.queued_outbound()) == 1
    with sender_lock(store), pytest.raises(a2a.A2aError, match="active delivery"):
        a2a.flush_queued(state, "sender")
    assert len(a2a.flush_queued(state, "sender")) == 1


def test_invalid_wire_response_is_terminal_and_recorded(state, monkeypatch):
    def malformed(*args, **kwargs):
        raise ValueError("malformed peer JSON")

    monkeypatch.setattr(a2a, "_deliver_wire", malformed)
    with pytest.raises(a2a.A2aError, match="invalid response"):
        a2a.send_message("sender", "peer", "body", None, state)
    with a2a.A2aStore(state, "sender").connect() as connection:
        assert (
            connection.execute("SELECT status FROM delivery_events").fetchone()[0]
            == "failed"
        )


def test_no_queue_policy_does_not_leave_pending_message(state, monkeypatch):
    monkeypatch.setattr(a2a, "_deliver_wire", offline)
    with pytest.raises(a2a.A2aError, match="queue disabled"):
        a2a.send_message(
            "sender", "peer", "body", None, state, queue_on_unreachable=False
        )
    assert not a2a.A2aStore(state, "sender").queued_outbound()


def test_structured_queue_delivers_after_capability_check_and_ack(state, monkeypatch):
    monkeypatch.setattr(a2a, "fetch_card", offline)
    sent = a2a.send_message("sender", "peer", "body", payload(), state)
    checked = []

    def card(record, timeout):
        checked.append(record.name)
        return {"skills": [{"id": "coordination-v2"}]}

    monkeypatch.setattr(a2a, "fetch_card", card)

    def post(url, **kwargs):
        assert checked == ["peer"]
        return httpx.Response(
            200,
            json={
                "result": {
                    "message": {
                        "parts": [
                            {
                                "data": {
                                    "kind": "delivery-receipt",
                                    "message_id": sent["message_id"],
                                }
                            }
                        ]
                    }
                }
            },
        )

    monkeypatch.setattr(a2a.httpx, "post", post)
    assert a2a.flush_queued(state, "sender")[0]["status"] == "delivered"
    events = history(state, "sender", message_id=sent["message_id"], limit=1)
    assert events["available"] and len(events["events"]) == 1
    assert events["events"][0]["status"] == "delivered"
    assert "body" not in events["events"][0]


def test_history_does_not_create_store_and_flush_cli_reports_remaining(
    state, monkeypatch, capsys
):
    assert history(state, "sender") == {
        "schema_version": 1,
        "available": False,
        "events": [],
    }
    assert not (state / "sender").exists()
    store = a2a.A2aStore(state, "sender")
    for mid in ("first", "second"):
        store.record_outbound(mid, "sender", "peer", "body", None)
    monkeypatch.setattr(a2a, "_deliver_wire", lambda *a, **kw: None)
    result = a2a.main(
        [
            "--state-dir",
            str(state),
            "flush",
            "--as-name",
            "sender",
            "--max-messages",
            "1",
        ]
    )
    assert result == 3
    assert len(json.loads(capsys.readouterr().out)) == 1
