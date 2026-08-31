#!/usr/bin/env python3
"""A2A-protocol transport between local agent sessions (codex, Claude, fable).

Replaces ``conductor.agent_mailbox``: each agent identity serves an Agent Card
and a JSON-RPC ``message/send`` endpoint on 127.0.0.1, addressed by a fixed
per-agent port from the shared registry.  Delivery is synchronous-first with
store-and-forward: an unreachable peer queues the message in the sender's own
store (``delivery_status='queued'``) for later redelivery via the ``flush``
command or an automatic sender-scoped flush before each new send to the same
peer (ordering preserved). A peer that is reachable but rejects a message stays a terminal
``failed`` — only transport-level unreachability queues.  Discovery is card
probing, so there is no shared mutable bridge state to clobber.

Channel-of-record invariants are unchanged: structured claims and bounded
``conductor.handoff`` entries coordinate workspace activity, while append-only
review receipts written by the nm_f6 runners remain the only authoritative
verdicts.  This module is transport plus discovery, nothing more; it never
authorizes direct ``.current_work.md`` ingestion.

Trust model: the registry file (0600) holds one token per agent; any local
process holding a recipient's token is inside the fleet trust domain.  Sender
names travel as message metadata and are cooperative, not authenticated.
Structured payloads (``kind``-discriminated data parts) are validated
fail-closed before storage; unknown kinds are rejected.

a2a-sdk 1.1.x wire notes (proto-first redesign; do not "fix" these):
- The JSON-RPC method is the gRPC-style ``SendMessage``, NOT ``message/send``.
- ``Part`` is flat: ``text`` is a plain string, ``data`` a protobuf ``Value``.
  ``Value`` serializes every number as a double, so a JSON integer ``3``
  round-trips as ``3.0`` — integral floats are accepted as integers.
- ``Struct`` in this protobuf build has ``__getitem__``/``__contains__`` but
  no ``.get()``.
- The ``A2A-Version: 1.0`` header is mandatory: a missing header is treated
  as 0.3 and rejected.
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import hmac
import json
import os
import re
import socket
import sqlite3
import stat
import uuid
from collections.abc import Iterator, Sequence
from pathlib import Path
from typing import Any, Final

import httpx
import uvicorn
from a2a.helpers.proto_helpers import (
    MessageToDict,
    get_data_parts,
    get_message_text,
    new_data_message,
    new_data_part,
    new_text_part,
)
from a2a.server.agent_execution import AgentExecutor, RequestContext
from a2a.server.events import EventQueue
from a2a.server.request_handlers import DefaultRequestHandler
from a2a.server.routes import create_agent_card_routes, create_jsonrpc_routes
from a2a.server.tasks import InMemoryTaskStore
from a2a.types import (
    AgentCapabilities,
    AgentCard,
    AgentInterface,
    AgentSkill,
    Message,
    Part,
    Role,
)
from a2a.utils.constants import (
    AGENT_CARD_WELL_KNOWN_PATH,
    DEFAULT_RPC_URL,
    PROTOCOL_VERSION_1_0,
    TransportProtocol,
    VERSION_HEADER,
)
from starlette.applications import Starlette
from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request
from starlette.responses import JSONResponse

from conductor.a2a_compaction import (
    CompactionError,
    compact_message,
    validate_coordination_v2,
)
from conductor.a2a_registry import (
    BIND_HOST as BIND_HOST,
    DEFAULT_REAP_FAILURES as DEFAULT_REAP_FAILURES,
    DEFAULT_STATE_DIR as DEFAULT_STATE_DIR,
    IDENTITY_RE as IDENTITY_RE,
    KNOWN_AGENTS as KNOWN_AGENTS,
    LIVENESS_SCHEMA_VERSION as LIVENESS_SCHEMA_VERSION,
    PROBE_TIMEOUT_S as PROBE_TIMEOUT_S,
    ROOT as ROOT,
    SCHEMA_VERSION as SCHEMA_VERSION,
    A2aError as A2aError,
    AgentRecord as AgentRecord,
    _atomic_json,
    _liveness_payload,
    _registration_fingerprint,
    _registry_lock,
    _registry_payload,
    _serve_port,
    _utc_now,
    fetch_card as fetch_card,
    init_registry as init_registry,
    list_peers as list_peers,
    load_registry as load_registry,
    probe_peer as probe_peer,
)

MAX_BODY_BYTES: Final = 1 << 18
SEND_TIMEOUT_S: Final = 10.0
DEFAULT_COMPACT_MESSAGES: Final = 8
DEFAULT_PREVIEW_CHARS: Final = 140
DEFAULT_CONTEXT_CHARS: Final = 1200
MAX_PREVIEW_ROWS: Final = 256
HEX64_RE: Final = re.compile(r"^[0-9a-f]{64}$")
TOKEN_HEADER: Final = "X-A2A-Token"
DATA_KINDS: Final = frozenset(
    {"gate-review-request", "coordination", "coordination-v2"}
)
REVIEW_GATES: Final = frozenset({1, 2, 3, 4, 5, 7})


class A2aStore:
    """Per-agent durable message journal (inbox + outbound terminal status)."""

    def __init__(self, state_dir: Path, name: str) -> None:
        self.dir = state_dir / name
        self.dir.mkdir(parents=True, exist_ok=True)
        os.chmod(self.dir, stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
        self.path = self.dir / "store.sqlite"
        self._migrate()

    @contextlib.contextmanager
    def connect(self) -> Iterator[sqlite3.Connection]:
        connection = sqlite3.connect(self.path, timeout=5.0)
        connection.row_factory = sqlite3.Row
        try:
            connection.execute("PRAGMA busy_timeout=5000")
            connection.execute("PRAGMA foreign_keys=ON")
            connection.execute("PRAGMA journal_mode=WAL")
            yield connection
            connection.commit()
        finally:
            connection.close()

    def _migrate(self) -> None:
        with self.connect() as connection:
            connection.executescript(
                """
                CREATE TABLE IF NOT EXISTS messages (
                    message_id TEXT NOT NULL,
                    direction TEXT NOT NULL CHECK(direction IN ('inbound','outbound')),
                    sender TEXT NOT NULL,
                    recipient TEXT NOT NULL,
                    body TEXT NOT NULL,
                    data_json TEXT,
                    created_at TEXT NOT NULL,
                    received_at TEXT,
                    delivery_status TEXT NOT NULL,
                    status_reason TEXT,
                    read_at TEXT,
                    -- Self-sends journal one outbound and one inbound fact for
                    -- the same message_id; the direction disambiguates them.
                    PRIMARY KEY (direction, message_id)
                );
                CREATE INDEX IF NOT EXISTS messages_inbox
                    ON messages(direction, read_at, created_at);
                CREATE TABLE IF NOT EXISTS message_state (
                    direction TEXT NOT NULL,
                    message_id TEXT NOT NULL,
                    thread_id TEXT NOT NULL,
                    summary TEXT NOT NULL,
                    protocol_status TEXT NOT NULL,
                    requires_response INTEGER NOT NULL
                        CHECK(requires_response IN (0, 1)),
                    retention_class TEXT NOT NULL
                        CHECK(retention_class IN ('pinned', 'operational')),
                    resolved_at TEXT,
                    superseded_at TEXT,
                    hold_reason TEXT,
                    tombstoned_at TEXT,
                    body_sha256 TEXT NOT NULL,
                    body_bytes INTEGER NOT NULL,
                    data_sha256 TEXT,
                    data_bytes INTEGER NOT NULL,
                    PRIMARY KEY (direction, message_id),
                    FOREIGN KEY (direction, message_id)
                        REFERENCES messages(direction, message_id)
                        ON DELETE CASCADE
                );
                CREATE INDEX IF NOT EXISTS message_state_context
                    ON message_state(direction, thread_id);
                CREATE INDEX IF NOT EXISTS message_state_retention
                    ON message_state(
                        retention_class, hold_reason, tombstoned_at,
                        resolved_at, superseded_at
                    );
                CREATE TABLE IF NOT EXISTS retention_events (
                    event_id TEXT PRIMARY KEY,
                    direction TEXT NOT NULL,
                    message_id TEXT NOT NULL,
                    policy_version INTEGER NOT NULL,
                    manifest_json TEXT NOT NULL,
                    manifest_sha256 TEXT NOT NULL,
                    compacted_at TEXT NOT NULL,
                    UNIQUE(direction, message_id),
                    FOREIGN KEY (direction, message_id)
                        REFERENCES messages(direction, message_id)
                        ON DELETE RESTRICT
                );
                CREATE TABLE IF NOT EXISTS message_presentations (
                    direction TEXT NOT NULL,
                    message_id TEXT NOT NULL,
                    presented_at TEXT NOT NULL,
                    PRIMARY KEY (direction, message_id),
                    FOREIGN KEY (direction, message_id)
                        REFERENCES messages(direction, message_id)
                        ON DELETE CASCADE
                );
                """
            )
        os.chmod(self.path, stat.S_IRUSR | stat.S_IWUSR)

    @staticmethod
    def _legacy_state(
        *, message_id: str, sender: str, body: str, data_json: str | None
    ) -> dict[str, Any]:
        collapsed = " ".join(body.split())
        summary = (
            collapsed[:237].rstrip() + "..." if len(collapsed) > 240 else collapsed
        )
        body_encoded = body.encode("utf-8")
        data_encoded = data_json.encode("utf-8") if data_json is not None else b""
        return {
            "thread_id": f"legacy:{sender}",
            "summary": summary or f"message {message_id}",
            "protocol_status": "open",
            "requires_response": 1,
            # Existing/unstructured content may be durable authentication
            # evidence. It is view-compactable but never retention-eligible.
            "retention_class": "pinned",
            "body_sha256": hashlib.sha256(body_encoded).hexdigest(),
            "body_bytes": len(body_encoded),
            "data_sha256": (
                hashlib.sha256(data_encoded).hexdigest()
                if data_json is not None
                else None
            ),
            "data_bytes": len(data_encoded),
        }

    @staticmethod
    def _protocol_state(
        *,
        message_id: str,
        direction: str,
        sender: str,
        recipient: str,
        body: str,
        data_json: str | None,
        created_at: str,
        received_at: str | None,
        delivery_status: str,
    ) -> dict[str, Any]:
        try:
            receipt = compact_message(
                {
                    "message_id": message_id,
                    "direction": direction,
                    "sender": sender,
                    "recipient": recipient,
                    "body": body,
                    "data_json": data_json,
                    "created_at": created_at,
                    "received_at": received_at,
                    "delivery_status": delivery_status,
                    "status_reason": None,
                    "read_at": None,
                }
            )
        except CompactionError as exc:
            raise A2aError(f"message compaction metadata is invalid: {exc}") from exc
        protocol_v2 = receipt["protocol"] == "coordination-v2"
        return {
            "thread_id": receipt["thread_id"],
            "summary": receipt["summary"],
            "protocol_status": receipt["status"] or "open",
            "requires_response": int(receipt["actionable"]),
            "retention_class": "operational" if protocol_v2 else "pinned",
            "body_sha256": receipt["body_sha256"],
            "body_bytes": receipt["raw_body_bytes"],
            "data_sha256": receipt["data_sha256"],
            "data_bytes": receipt["raw_data_bytes"],
            "supersedes": receipt["supersedes"],
        }

    @staticmethod
    def _insert_state(
        connection: sqlite3.Connection,
        *,
        direction: str,
        message_id: str,
        state: dict[str, Any],
    ) -> None:
        connection.execute(
            """
            INSERT INTO message_state (
                direction, message_id, thread_id, summary, protocol_status,
                requires_response, retention_class, body_sha256, body_bytes,
                data_sha256, data_bytes
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                direction,
                message_id,
                state["thread_id"],
                state["summary"],
                state["protocol_status"],
                state["requires_response"],
                state["retention_class"],
                state["body_sha256"],
                state["body_bytes"],
                state["data_sha256"],
                state["data_bytes"],
            ),
        )

    def record_inbound(
        self,
        message_id: str,
        sender: str,
        recipient: str,
        body: str,
        data_json: str | None,
        state: dict[str, Any] | None = None,
    ) -> None:
        now = _utc_now()
        with self.connect() as connection:
            cursor = connection.execute(
                """
                INSERT OR IGNORE INTO messages (
                    message_id, direction, sender, recipient, body, data_json,
                    created_at, received_at, delivery_status
                ) VALUES (?, 'inbound', ?, ?, ?, ?, ?, ?, 'delivered')
                """,
                (
                    message_id,
                    sender,
                    recipient,
                    body,
                    data_json,
                    now,
                    now,
                ),
            )
            if cursor.rowcount == 1:
                selected_state = state or self._protocol_state(
                    message_id=message_id,
                    direction="inbound",
                    sender=sender,
                    recipient=recipient,
                    body=body,
                    data_json=data_json,
                    created_at=now,
                    received_at=now,
                    delivery_status="delivered",
                )
                self._insert_state(
                    connection,
                    direction="inbound",
                    message_id=message_id,
                    state=selected_state,
                )
                for superseded_id in selected_state.get("supersedes", []):
                    superseded = connection.execute(
                        """
                        UPDATE message_state
                        SET superseded_at=COALESCE(superseded_at, ?),
                            protocol_status='superseded'
                        WHERE direction='inbound' AND message_id=?
                          AND thread_id=? AND tombstoned_at IS NULL
                          AND EXISTS (
                              SELECT 1 FROM messages AS prior
                              WHERE prior.direction='inbound'
                                AND prior.message_id=message_state.message_id
                                AND prior.sender=?
                          )
                        """,
                        (
                            now,
                            superseded_id,
                            selected_state["thread_id"],
                            sender,
                        ),
                    )
                    if superseded.rowcount != 1:
                        raise A2aError(
                            f"supersedes target {superseded_id!r} is missing or "
                            "belongs to another sender/thread"
                        )

    def record_outbound(
        self,
        message_id: str,
        sender: str,
        recipient: str,
        body: str,
        data_json: str | None,
        state: dict[str, Any] | None = None,
    ) -> None:
        now = _utc_now()
        with self.connect() as connection:
            connection.execute(
                """
                INSERT INTO messages (
                    message_id, direction, sender, recipient, body, data_json,
                    created_at, delivery_status
                ) VALUES (?, 'outbound', ?, ?, ?, ?, ?, 'pending')
                """,
                (message_id, sender, recipient, body, data_json, now),
            )
            self._insert_state(
                connection,
                direction="outbound",
                message_id=message_id,
                state=state
                or self._protocol_state(
                    message_id=message_id,
                    direction="outbound",
                    sender=sender,
                    recipient=recipient,
                    body=body,
                    data_json=data_json,
                    created_at=now,
                    received_at=None,
                    delivery_status="pending",
                ),
            )

    def mark_outbound(
        self,
        message_id: str,
        status: str,
        reason: str | None,
        received_at: str | None,
    ) -> None:
        if status not in ("delivered", "failed", "queued"):
            raise A2aError(f"unknown outbound status {status!r}")
        with self.connect() as connection:
            cursor = connection.execute(
                """
                UPDATE messages
                SET delivery_status=?, status_reason=?, received_at=?
                WHERE message_id=? AND direction='outbound'
                """,
                (status, reason, received_at, message_id),
            )
            if cursor.rowcount != 1:
                raise A2aError(f"unknown outbound message {message_id!r}")

    def mark_read(self, message_id: str) -> sqlite3.Row:
        with self.connect() as connection:
            cursor = connection.execute(
                """
                UPDATE messages SET read_at=?
                WHERE message_id=? AND direction='inbound' AND read_at IS NULL
                """,
                (_utc_now(), message_id),
            )
            if cursor.rowcount != 1:
                raise A2aError(f"no unread inbound message {message_id!r}")
            return self._fetch(connection, message_id, "inbound")

    @staticmethod
    def _fetch(
        connection: sqlite3.Connection, message_id: str, direction: str
    ) -> sqlite3.Row:
        row = connection.execute(
            "SELECT * FROM messages WHERE message_id=? AND direction=?",
            (message_id, direction),
        ).fetchone()
        if row is None:
            raise A2aError(f"unknown message {message_id!r}")
        return row

    def queued_outbound(self, to_name: str | None = None) -> list[sqlite3.Row]:
        """Outbound messages awaiting redelivery, oldest first."""
        query = (
            "SELECT * FROM messages "
            "WHERE direction='outbound' AND delivery_status='queued'"
        )
        params: tuple[str, ...] = ()
        if to_name is not None:
            query += " AND recipient=?"
            params = (to_name,)
        query += " ORDER BY created_at"
        with self.connect() as connection:
            return list(connection.execute(query, params).fetchall())

    def rows(self, unread_only: bool, limit: int) -> list[sqlite3.Row]:
        query = "SELECT * FROM messages WHERE direction='inbound'"
        if unread_only:
            query += " AND read_at IS NULL"
        query += " ORDER BY created_at DESC LIMIT ?"
        with self.connect() as connection:
            return list(connection.execute(query, (limit,)).fetchall())

    def preview_rows(
        self,
        *,
        unread_only: bool,
        unpresented_only: bool = False,
        limit: int,
        preview_chars: int,
    ) -> tuple[list[dict[str, Any]], int]:
        """Read bounded inbox envelopes without materializing full bodies."""

        where = "m.direction='inbound'"
        if unread_only:
            where += " AND m.read_at IS NULL"
        if unpresented_only:
            where += " AND p.message_id IS NULL"
        query = f"""
            SELECT
                m.message_id, m.direction, m.sender, m.recipient,
                m.created_at, m.received_at, m.delivery_status, m.read_at,
                substr(replace(replace(m.body, char(10), ' '), char(13), ' '),
                       1, ?) AS body,
                length(CAST(m.body AS BLOB)) AS body_bytes,
                COALESCE(s.thread_id, 'legacy:' || m.sender) AS thread_id,
                COALESCE(NULLIF(substr(s.summary, 1, ?), ''),
                         substr(replace(replace(m.body, char(10), ' '), char(13), ' '),
                                1, ?)) AS summary,
                COALESCE(s.protocol_status, 'open') AS protocol_status,
                COALESCE(s.requires_response, 1) AS requires_response,
                COALESCE(s.retention_class, 'pinned') AS retention_class,
                p.presented_at,
                s.resolved_at, s.superseded_at, s.hold_reason,
                COALESCE(s.body_sha256, '') AS body_sha256,
                COALESCE(s.data_bytes, length(CAST(m.data_json AS BLOB)), 0)
                    AS data_bytes,
                SUM(
                    length(CAST(m.body AS BLOB))
                    + COALESCE(length(CAST(m.data_json AS BLOB)), 0)
                ) OVER () AS total_raw_bytes,
                COUNT(*) OVER () AS total_count
            FROM messages AS m
            LEFT JOIN message_state AS s
              ON s.direction=m.direction AND s.message_id=m.message_id
            LEFT JOIN message_presentations AS p
              ON p.direction=m.direction AND p.message_id=m.message_id
            WHERE {where}
            ORDER BY m.created_at DESC, m.message_id DESC
            LIMIT ?
        """
        with self.connect() as connection:
            fetched = connection.execute(
                query, (preview_chars, preview_chars, preview_chars, limit)
            ).fetchall()
        total = int(fetched[0]["total_count"]) if fetched else 0
        return ([{key: row[key] for key in row.keys()} for row in fetched], total)

    def message(self, message_id: str, direction: str = "inbound") -> sqlite3.Row:
        with self.connect() as connection:
            return self._fetch(connection, message_id, direction)

    def mark_presented(self, message_ids: Sequence[str]) -> int:
        if not message_ids:
            return 0
        presented_at = _utc_now()
        with self.connect() as connection:
            before = connection.total_changes
            connection.executemany(
                """
                INSERT OR IGNORE INTO message_presentations (
                    direction, message_id, presented_at
                ) VALUES ('inbound', ?, ?)
                """,
                ((message_id, presented_at) for message_id in message_ids),
            )
            return connection.total_changes - before

    def resolve(self, message_id: str) -> sqlite3.Row:
        """Mark one read inbound message resolved without changing its body."""

        with self.connect() as connection:
            row = self._fetch(connection, message_id, "inbound")
            if row["read_at"] is None:
                raise A2aError(f"cannot resolve unread message {message_id!r}")
            state = connection.execute(
                """
                SELECT 1 FROM message_state
                WHERE direction='inbound' AND message_id=?
                """,
                (message_id,),
            ).fetchone()
            if state is None:
                self._insert_state(
                    connection,
                    direction="inbound",
                    message_id=message_id,
                    state=self._legacy_state(
                        message_id=message_id,
                        sender=row["sender"],
                        body=row["body"],
                        data_json=row["data_json"],
                    ),
                )
            connection.execute(
                """
                UPDATE message_state
                SET resolved_at=COALESCE(resolved_at, ?),
                    protocol_status='resolved'
                WHERE direction='inbound' AND message_id=?
                """,
                (_utc_now(), message_id),
            )
            return self._fetch(connection, message_id, "inbound")

    def set_hold(self, message_id: str, reason: str | None) -> None:
        if reason is not None:
            reason = " ".join(reason.split())
            if not reason or len(reason) > 240:
                raise A2aError("hold reason must be 1..240 characters")
        with self.connect() as connection:
            self._fetch(connection, message_id, "inbound")
            cursor = connection.execute(
                """
                UPDATE message_state SET hold_reason=?
                WHERE direction='inbound' AND message_id=?
                """,
                (reason, message_id),
            )
            if cursor.rowcount != 1:
                raise A2aError(
                    f"message {message_id!r} has no lifecycle metadata; "
                    "legacy messages remain pinned"
                )

    def counts(self) -> dict[str, int]:
        with self.connect() as connection:
            return {
                row["status"]: row["count"]
                for row in connection.execute(
                    """
                    SELECT delivery_status AS status, COUNT(*) AS count
                    FROM messages GROUP BY delivery_status
                    """
                )
            }


def validate_data_payload(payload: Any) -> str:
    """Fail-closed validation of a structured data part; returns its kind."""
    if not isinstance(payload, dict):
        raise A2aError("data part must be a JSON object")
    kind = payload.get("kind")
    if not isinstance(kind, str) or kind not in DATA_KINDS:
        raise A2aError(
            f"unknown data kind {kind!r}; expected one of {sorted(DATA_KINDS)}"
        )
    if kind == "gate-review-request":
        # protobuf Value serializes every number as a double, so a JSON `3`
        # arrives as 3.0; integral floats are the only representable integers.
        gate = payload.get("gate")
        if (
            isinstance(gate, bool)
            or not isinstance(gate, int | float)
            or gate not in REVIEW_GATES
        ):
            raise A2aError(
                "gate-review-request requires integer gate in {1, 2, 3, 4, 5, 7}"
            )
        fingerprint = payload.get("fingerprint")
        if not isinstance(fingerprint, str) or not HEX64_RE.match(fingerprint):
            raise A2aError("gate-review-request requires 64-hex fingerprint")
        paths = payload.get("artifact_paths")
        if (
            not isinstance(paths, list)
            or not paths
            or not all(isinstance(p, str) and p for p in paths)
        ):
            raise A2aError("gate-review-request requires non-empty artifact_paths")
    elif kind == "coordination-v2":
        try:
            validate_coordination_v2(payload)
        except CompactionError as exc:
            raise A2aError(str(exc)) from exc
    return kind


def build_agent_card(name: str, port: int) -> AgentCard:
    return AgentCard(
        name=name,
        description=f"Bounded local-agent coordination endpoint for {name}",
        version="1.0.0",
        capabilities=AgentCapabilities(streaming=False, push_notifications=False),
        default_input_modes=["application/json"],
        default_output_modes=["application/json"],
        skills=[
            AgentSkill(
                id="coordination",
                name="coordination",
                description="Peer-to-peer status and handoff messages",
            ),
            AgentSkill(
                id="coordination-v2",
                name="coordination-v2",
                description=(
                    "Sender-authored bounded summaries, thread IDs, lifecycle "
                    "status, and supersession edges for context-safe coordination"
                ),
            ),
            AgentSkill(
                id="gate-review-request",
                name="gate-review-request",
                description=(
                    "Structured nm_f6 gate review request (gate, fingerprint, "
                    "artifact_paths); verdicts remain authoritative only via "
                    "record_review receipts"
                ),
            ),
        ],
        supported_interfaces=[
            AgentInterface(
                url=f"http://{BIND_HOST}:{port}{DEFAULT_RPC_URL}",
                protocol_binding=TransportProtocol.JSONRPC.value,
                protocol_version=PROTOCOL_VERSION_1_0,
            )
        ],
    )


class InboxExecutor(AgentExecutor):
    """Stores every accepted message and replies with a delivery receipt."""

    def __init__(self, record: AgentRecord, store: A2aStore) -> None:
        self.record = record
        self.store = store

    async def execute(self, context: RequestContext, event_queue: EventQueue) -> None:
        message = context.message
        if message is None:
            raise A2aError("message/send requires a message")
        body = get_message_text(message)
        if len(body.encode()) > MAX_BODY_BYTES:
            raise A2aError(f"body exceeds {MAX_BODY_BYTES} bytes")
        payloads = get_data_parts(message.parts)
        for payload in payloads:
            validate_data_payload(payload)
        # protobuf 6.x upb Struct has __getitem__/__contains__ but no .get()
        sender = (
            message.metadata["sender"] if "sender" in message.metadata else "unknown"
        )
        message_id = message.message_id or str(uuid.uuid4())
        self.store.record_inbound(
            message_id=message_id,
            sender=str(sender),
            recipient=self.record.name,
            body=body,
            data_json=(
                json.dumps(payloads[0], ensure_ascii=False, sort_keys=True)
                if payloads
                else None
            ),
        )
        await event_queue.enqueue_event(
            new_data_message(
                {
                    "kind": "delivery-receipt",
                    "message_id": message_id,
                    "recipient": self.record.name,
                }
            )
        )

    async def cancel(self, context: RequestContext) -> None:
        raise A2aError("message delivery cannot be cancelled")


class TokenMiddleware(BaseHTTPMiddleware):
    """Rejects unauthenticated writes to the JSON-RPC endpoint; card stays public."""

    def __init__(self, app: Any, token: str) -> None:
        super().__init__(app)
        self.token = token

    async def dispatch(self, request: Request, call_next: Any) -> Any:
        if request.url.path == DEFAULT_RPC_URL and request.method != "GET":
            provided = request.headers.get(TOKEN_HEADER, "")
            if not hmac.compare_digest(provided, self.token):
                return JSONResponse(
                    {
                        "jsonrpc": "2.0",
                        "id": None,
                        "error": {"code": -32001, "message": "invalid agent token"},
                    },
                    status_code=401,
                )
        return await call_next(request)


def build_app(record: AgentRecord, store: A2aStore) -> Starlette:
    handler = DefaultRequestHandler(
        agent_executor=InboxExecutor(record, store),
        task_store=InMemoryTaskStore(),
        agent_card=build_agent_card(record.name, record.port),
    )
    routes = [
        *create_agent_card_routes(build_agent_card(record.name, record.port)),
        *create_jsonrpc_routes(handler, DEFAULT_RPC_URL),
    ]
    app = Starlette(routes=routes)
    app.add_middleware(TokenMiddleware, token=record.token)
    return app


def serve(name: str, state_dir: Path, port: int | None = None) -> None:
    # Bind first. A port collision must never leave a fresh identity in the
    # registry. The generation rotates only after the endpoint owns its socket,
    # allowing liveness/reap to reject stale probe results across restarts.
    selected_port = _serve_port(state_dir, name, port)
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    try:
        listener.bind((BIND_HOST, selected_port))
        listener.listen(socket.SOMAXCONN)
    except OSError as exc:
        listener.close()
        raise A2aError(
            f"cannot bind {name!r} to {BIND_HOST}:{selected_port}: {exc}"
        ) from exc

    try:
        records = init_registry(
            state_dir,
            name=name,
            port=selected_port,
            renew_generation=True,
        )
        record = records[name]
        store = A2aStore(state_dir, name)
        app = build_app(record, store)
        print(
            json.dumps(
                {
                    "name": record.name,
                    "port": record.port,
                    "generation": record.generation,
                    "card_url": f"{record.base_url}{AGENT_CARD_WELL_KNOWN_PATH}",
                    "rpc_url": f"{record.base_url}{DEFAULT_RPC_URL}",
                    "pid": os.getpid(),
                },
                sort_keys=True,
            ),
            flush=True,
        )
        config = uvicorn.Config(app, log_level="warning", lifespan="off")
        uvicorn.Server(config).run(sockets=[listener])
    finally:
        listener.close()


def _require_coordination_v2_skill(record: AgentRecord, card: dict[str, Any]) -> None:
    """Reject v2 delivery unless the recipient explicitly advertises support."""

    skills = card.get("skills")
    advertised = isinstance(skills, list) and any(
        isinstance(skill, dict) and skill.get("id") == "coordination-v2"
        for skill in skills
    )
    if not advertised:
        raise A2aError(
            f"peer {record.name!r} does not advertise coordination-v2; "
            "refusing structured send"
        )


def _coordination_v2_card(
    record: AgentRecord, data_payload: dict[str, Any] | None
) -> dict[str, Any] | None:
    """Preflight v2 capability before the caller records an outbound fact."""

    if data_payload is None or data_payload.get("kind") != "coordination-v2":
        return None
    try:
        card = fetch_card(record, timeout=PROBE_TIMEOUT_S)
    except httpx.HTTPError as exc:
        raise A2aError(
            f"cannot verify coordination-v2 support for peer {record.name!r}: {exc}"
        ) from exc
    _require_coordination_v2_skill(record, card)
    return card


def _deliver_wire(
    record: AgentRecord,
    from_name: str,
    message_id: str,
    body: str,
    data_payload: dict[str, Any] | None,
    *,
    card: dict[str, Any] | None = None,
) -> None:
    """Push one message over the wire; raises on any non-delivery.

    ``httpx.TransportError`` means the peer is unreachable (queueable);
    ``A2aError`` means the peer answered and rejected (terminal).
    """
    parts: list[Part] = [new_text_part(body)]
    if data_payload is not None:
        parts.append(new_data_part(data_payload))
    message = Message(
        message_id=message_id,
        role=Role.ROLE_USER,
        parts=parts,
    )
    message.metadata.update({"sender": from_name})
    request = {
        "jsonrpc": "2.0",
        "id": str(uuid.uuid4()),
        # a2a-sdk 1.1.x dispatches gRPC-style method names, not "message/send".
        "method": "SendMessage",
        "params": {"message": MessageToDict(message)},
    }
    delivery_card = card or fetch_card(record, timeout=PROBE_TIMEOUT_S)
    if data_payload is not None and data_payload.get("kind") == "coordination-v2":
        _require_coordination_v2_skill(record, delivery_card)
    response = httpx.post(
        f"{record.base_url}{DEFAULT_RPC_URL}",
        json=request,
        headers={
            TOKEN_HEADER: record.token,
            VERSION_HEADER: PROTOCOL_VERSION_1_0,
        },
        timeout=SEND_TIMEOUT_S,
    )
    payload = response.json()
    if response.status_code != 200 or "error" in payload:
        reason = payload.get("error", {}).get("message", response.text[:200])
        raise A2aError(f"peer rejected message: {reason}")
    # Result is a SendMessageResponse; the reply message sits under "message".
    result = payload.get("result", {}).get("message", {})
    receipts = [
        p
        for p in result.get("parts", [])
        if isinstance(p.get("data"), dict)
        and p["data"].get("kind") == "delivery-receipt"
    ]
    if not receipts or receipts[0]["data"].get("message_id") != message_id:
        raise A2aError("peer ack did not echo the message_id")


def send_message(
    from_name: str,
    to_name: str,
    body: str,
    data_payload: dict[str, Any] | None,
    state_dir: Path,
    queue_on_unreachable: bool = True,
) -> dict[str, Any]:
    records = load_registry(state_dir)
    if from_name not in records:
        raise A2aError(f"unknown sender {from_name!r}; registered: {sorted(records)}")
    if to_name not in records:
        raise A2aError(f"unknown recipient {to_name!r}; registered: {sorted(records)}")
    if len(body.encode()) > MAX_BODY_BYTES:
        raise A2aError(f"body exceeds {MAX_BODY_BYTES} bytes")
    if data_payload is not None:
        validate_data_payload(data_payload)
    record = records[to_name]
    delivery_card = _coordination_v2_card(record, data_payload)
    message_id = str(uuid.uuid4())
    data_json: str | None = None
    if data_payload is not None:
        data_json = json.dumps(data_payload, ensure_ascii=False, sort_keys=True)
    store = A2aStore(state_dir, from_name)
    # Preserve ordering: anything already queued for this peer goes first.
    if store.queued_outbound(to_name):
        flush_queued(state_dir, from_name=from_name, to_name=to_name)
    store.record_outbound(
        message_id=message_id,
        sender=from_name,
        recipient=to_name,
        body=body,
        data_json=data_json,
    )
    try:
        _deliver_wire(
            record,
            from_name,
            message_id,
            body,
            data_payload,
            card=delivery_card,
        )
        store.mark_outbound(message_id, "delivered", None, _utc_now())
    except httpx.TransportError as exc:
        reason = f"peer unreachable: {exc}"
        if queue_on_unreachable:
            store.mark_outbound(message_id, "queued", reason[:500], None)
        else:
            store.mark_outbound(message_id, "failed", reason[:500], None)
            raise A2aError(reason) from exc
    except (A2aError, httpx.HTTPError) as exc:
        reason = str(exc) if isinstance(exc, A2aError) else f"peer error: {exc}"
        store.mark_outbound(message_id, "failed", reason[:500], None)
        raise A2aError(reason) from exc
    return _outbound_row(store, message_id)


def flush_queued(
    state_dir: Path,
    from_name: str | None = None,
    to_name: str | None = None,
) -> list[dict[str, Any]]:
    """Redeliver queued outbound messages whose recipients are now reachable.

    Per recipient, messages go oldest-first; the first transport failure
    stops that recipient's flush so ordering is never violated.  A peer that
    answers and rejects marks that message terminally ``failed``.  Returns a
    summary row per attempted message.
    """
    records = load_registry(state_dir)
    if from_name is not None:
        senders = [from_name]
    else:
        senders = sorted(
            d.name
            for d in state_dir.iterdir()
            if d.is_dir() and d.name in records and (d / "store.sqlite").is_file()
        )
    results: list[dict[str, Any]] = []
    for sender in senders:
        store = A2aStore(state_dir, sender)
        unreachable: set[str] = set()
        for row in store.queued_outbound(to_name):
            recipient = row["recipient"]
            if recipient in unreachable:
                continue
            if recipient not in records:
                store.mark_outbound(
                    row["message_id"], "failed", "recipient no longer registered", None
                )
                results.append({"message_id": row["message_id"], "status": "failed"})
                continue
            data_payload = json.loads(row["data_json"]) if row["data_json"] else None
            try:
                _deliver_wire(
                    records[recipient],
                    sender,
                    row["message_id"],
                    row["body"],
                    data_payload,
                )
                store.mark_outbound(row["message_id"], "delivered", None, _utc_now())
                status = "delivered"
            except httpx.TransportError as exc:
                store.mark_outbound(
                    row["message_id"],
                    "queued",
                    f"still unreachable at {_utc_now()}: {exc}"[:500],
                    None,
                )
                unreachable.add(recipient)
                status = "queued"
            except (A2aError, httpx.HTTPError) as exc:
                store.mark_outbound(row["message_id"], "failed", str(exc)[:500], None)
                status = "failed"
            results.append(
                {
                    "message_id": row["message_id"],
                    "sender": sender,
                    "recipient": recipient,
                    "status": status,
                }
            )
    return results


def _outbound_row(store: A2aStore, message_id: str) -> dict[str, Any]:
    with store.connect() as connection:
        row = connection.execute(
            """
            SELECT m.message_id, m.sender, m.recipient, m.created_at,
                   m.received_at, m.delivery_status, m.status_reason,
                   s.thread_id, s.summary, s.protocol_status,
                   s.requires_response, s.body_sha256, s.body_bytes,
                   s.data_sha256, s.data_bytes
            FROM messages AS m
            JOIN message_state AS s
              ON s.direction=m.direction AND s.message_id=m.message_id
            WHERE m.message_id=? AND m.direction='outbound'
            """,
            (message_id,),
        ).fetchone()
    if row is None:
        raise A2aError(f"unknown outbound message {message_id!r}")
    return {
        "schema_version": 1,
        "authority": "a2a-delivery-receipt",
        **{key: row[key] for key in row.keys()},
    }


def reap_registry(
    state_dir: Path, consecutive_failures: int = DEFAULT_REAP_FAILURES
) -> dict[str, Any]:
    """Probe peers and remove identities down for the requested failure streak."""

    if consecutive_failures < 1:
        raise A2aError("--consecutive-failures must be at least 1")
    peers = list_peers(state_dir)
    candidates = {
        item["name"]: item["registration_fingerprint"]
        for item in peers
        if item.get("status") == "down"
        and item.get("consecutive_failures", 0) >= consecutive_failures
    }
    registry_path = state_dir / "agents.json"
    liveness_path = state_dir / "liveness.json"
    removed: list[str] = []
    with _registry_lock(state_dir):
        payload = _registry_payload(registry_path)
        agents: dict[str, Any] = payload["agents"]
        records = load_registry(state_dir)
        liveness = _liveness_payload(liveness_path)
        removed = sorted(
            name
            for name, fingerprint in candidates.items()
            if name in agents
            and name in records
            and _registration_fingerprint(records[name]) == fingerprint
            and isinstance(liveness["agents"].get(name), dict)
            and liveness["agents"][name].get("registration_fingerprint") == fingerprint
            and liveness["agents"][name].get("consecutive_failures", 0)
            >= consecutive_failures
        )
        for name in removed:
            del agents[name]
        if removed:
            _atomic_json(registry_path, payload)
            for name in removed:
                liveness["agents"].pop(name, None)
            _atomic_json(liveness_path, liveness)
    return {
        "consecutive_failures": consecutive_failures,
        "probed": peers,
        "reaped": removed,
    }


def _row_dict(row: sqlite3.Row) -> dict[str, Any]:
    from conductor.a2a_cli import _row_dict as implementation

    return implementation(row)


def _compact_json(value: Any) -> str:
    from conductor.a2a_cli import _compact_json as implementation

    return implementation(value)


def compact_inbox_payload(
    store: A2aStore,
    *,
    agent: str,
    unread_only: bool,
    unpresented_only: bool,
    max_messages: int,
    preview_chars: int,
    max_chars: int,
) -> tuple[dict[str, Any], list[str]]:
    """Build one structurally valid, character-bounded inbox envelope."""

    from conductor.a2a_cli import compact_inbox_payload as implementation

    return implementation(
        store,
        agent=agent,
        unread_only=unread_only,
        unpresented_only=unpresented_only,
        max_messages=max_messages,
        preview_chars=preview_chars,
        max_chars=max_chars,
    )


def render_compact_inbox(payload: dict[str, Any]) -> str:
    from conductor.a2a_cli import render_compact_inbox as implementation

    return implementation(payload)


def build_parser() -> argparse.ArgumentParser:
    from conductor.a2a_cli import build_parser as implementation

    return implementation()


def main(argv: Sequence[str] | None = None) -> int:
    from conductor.a2a_cli import main as implementation

    return implementation(argv)


if __name__ == "__main__":
    raise SystemExit(main())
