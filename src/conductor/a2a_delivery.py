"""Serialized, recoverable outbound delivery for the local A2A transport."""

from __future__ import annotations

import contextlib
import fcntl
import json
import os
import sqlite3
import uuid
from collections.abc import Iterator
from pathlib import Path
from typing import TYPE_CHECKING, Any

import httpx

from conductor.a2a_registry import A2aError, AgentRecord, _utc_now, load_registry

if TYPE_CHECKING:
    from conductor.agent_a2a import A2aStore


@contextlib.contextmanager
def sender_lock(store: A2aStore) -> Iterator[None]:
    """One in-flight sender; process exit releases the lock, including crashes."""
    with (store.dir / ".delivery.lock").open("a+") as handle:
        os.chmod(handle.name, 0o600)
        try:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise A2aError(f"sender {store.dir.name!r} has an active delivery") from exc
        try:
            yield
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def _attempt(
    store: A2aStore,
    record: AgentRecord,
    row: sqlite3.Row,
    *,
    card: dict[str, Any] | None = None,
) -> str:
    from conductor import agent_a2a as transport

    data = json.loads(row["data_json"]) if row["data_json"] else None
    try:
        transport._deliver_wire(
            record, row["sender"], row["message_id"], row["body"], data, card=card
        )
    except httpx.TransportError as exc:
        status, reason = "queued", f"peer unreachable: {exc}"
    except (A2aError, httpx.HTTPError, ValueError) as exc:
        status, reason = (
            "failed",
            f"peer rejected or returned an invalid response: {exc}",
        )
    else:
        status, reason = "delivered", None
    store.mark_outbound(
        row["message_id"],
        status,
        reason[:500] if reason else None,
        _utc_now() if status == "delivered" else None,
    )
    return status


def _flush_store(
    store: A2aStore, records: dict[str, AgentRecord], to_name: str | None, limit: int
) -> list[dict[str, Any]]:
    results: list[dict[str, Any]] = []
    unreachable: set[str] = set()
    while len(results) < limit:
        pending = store.queued_outbound(to_name, limit=1, exclude=sorted(unreachable))
        if not pending:
            break
        row = pending[0]
        recipient = row["recipient"]
        if recipient not in records:
            store.mark_outbound(
                row["message_id"], "failed", "recipient no longer registered", None
            )
            status = "failed"
        else:
            status = _attempt(store, records[recipient], row)
        if status == "queued":
            unreachable.add(recipient)
        results.append(
            {
                "message_id": row["message_id"],
                "sender": row["sender"],
                "recipient": recipient,
                "status": status,
            }
        )
    return results


def send_message(
    from_name: str,
    to_name: str,
    body: str,
    data_payload: dict[str, Any] | None,
    state_dir: Path,
    queue_on_unreachable: bool = True,
) -> dict[str, Any]:
    from conductor import agent_a2a as transport

    records = load_registry(state_dir)
    for role, name in (("sender", from_name), ("recipient", to_name)):
        if name not in records:
            raise A2aError(f"unknown {role} {name!r}; registered: {sorted(records)}")
    if len(body.encode()) > transport.MAX_BODY_BYTES:
        raise A2aError(f"body exceeds {transport.MAX_BODY_BYTES} bytes")
    if data_payload is not None:
        transport.validate_data_payload(data_payload)
    card = transport._coordination_v2_card(records[to_name], data_payload)
    store = transport.A2aStore(state_dir, from_name)
    with sender_lock(store):
        # Bounded recovery first. New messages never bypass an older queued or
        # crash-left pending message, even when the peer recovers mid-flush.
        _flush_store(store, records, to_name, 100)
        waiting = bool(store.queued_outbound(to_name, limit=1))
        message_id = str(uuid.uuid4())
        store.record_outbound(
            message_id,
            from_name,
            to_name,
            body,
            json.dumps(data_payload, ensure_ascii=False, sort_keys=True)
            if data_payload is not None
            else None,
        )
        if waiting:
            status = "queued"
            store.mark_outbound(
                message_id, status, "waiting for earlier outbound messages", None
            )
        else:
            status = _attempt(
                store,
                records[to_name],
                store.message(message_id, "outbound"),
                card=card,
            )
        if status == "queued" and not queue_on_unreachable:
            reason = str(store.message(message_id, "outbound")["status_reason"])
            reason += "; queue disabled"
            store.mark_outbound(message_id, "failed", reason, None)
            raise A2aError(reason)
        if status == "failed":
            raise A2aError(str(store.message(message_id, "outbound")["status_reason"]))
        return transport._outbound_row(store, message_id)


def flush_queued(
    state_dir: Path,
    from_name: str | None = None,
    to_name: str | None = None,
    max_messages: int = 100,
) -> list[dict[str, Any]]:
    """Retry a bounded batch, including crash-left pending sends, oldest first."""
    from conductor import agent_a2a as transport

    if not 1 <= max_messages <= 1000:
        raise A2aError("max_messages must be between 1 and 1000")
    records = load_registry(state_dir)
    if from_name is not None and from_name not in records:
        raise A2aError(f"unknown sender {from_name!r}")
    senders = (
        [from_name]
        if from_name
        else sorted(
            name for name in records if (state_dir / name / "store.sqlite").is_file()
        )
    )
    results: list[dict[str, Any]] = []
    for sender in senders:
        if len(results) >= max_messages:
            break
        store = transport.A2aStore(state_dir, sender)
        with sender_lock(store):
            results.extend(
                _flush_store(store, records, to_name, max_messages - len(results))
            )
    return results


def history(
    state_dir: Path, identity: str, *, message_id: str | None = None, limit: int = 20
) -> dict[str, Any]:
    """Read bounded delivery evidence without creating or migrating a store."""
    if not 1 <= limit <= 1000:
        raise A2aError("history limit must be between 1 and 1000")
    if identity not in load_registry(state_dir):
        raise A2aError(f"unknown identity {identity!r}")
    path = state_dir / identity / "store.sqlite"
    if not path.is_file():
        return {"schema_version": 1, "available": False, "events": []}
    with contextlib.closing(
        sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True)
    ) as connection:
        connection.row_factory = sqlite3.Row
        if not connection.execute(
            "SELECT 1 FROM sqlite_master WHERE name='delivery_events'"
        ).fetchone():
            return {"schema_version": 1, "available": False, "events": []}
        query = "SELECT event_id, message_id, occurred_at, status, reason FROM delivery_events"
        params: list[Any] = []
        if message_id:
            query += " WHERE message_id=?"
            params.append(message_id)
        query += " ORDER BY event_id DESC LIMIT ?"
        params.append(limit)
        events = [dict(row) for row in connection.execute(query, params)]
    return {"schema_version": 1, "available": True, "events": events}
