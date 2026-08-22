#!/usr/bin/env python3
"""A2A-protocol transport between local agent sessions (codex, Claude, fable).

Replaces ``conductor.agent_mailbox``: each agent identity serves an Agent Card
and a JSON-RPC ``message/send`` endpoint on 127.0.0.1, addressed by a fixed
per-agent port from the shared registry.  Delivery is synchronous and terminal
— a down peer is reported as ``failed`` immediately instead of burning async
retries.  Discovery is card probing, so there is no shared mutable bridge
state to clobber.

Channel-of-record invariants are unchanged: ``.current_work.md`` remains the
coordination log, and the append-only review receipts written by the nm_f6
runners remain the only authoritative verdicts.  This module is transport
plus discovery, nothing more.

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
import dataclasses
import datetime as dt
import hmac
import json
import os
import re
import secrets
import sqlite3
import stat
import sys
import uuid
from collections.abc import Iterator, Sequence
from concurrent.futures import ThreadPoolExecutor
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

ROOT: Final = Path(__file__).resolve().parents[1]
DEFAULT_STATE_DIR: Final = ROOT / ".agents" / "a2a"
BIND_HOST: Final = "127.0.0.1"
SCHEMA_VERSION: Final = 1
MAX_BODY_BYTES: Final = 1 << 18
PROBE_TIMEOUT_S: Final = 1.0
SEND_TIMEOUT_S: Final = 10.0
IDENTITY_RE: Final = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
HEX64_RE: Final = re.compile(r"^[0-9a-f]{64}$")
TOKEN_HEADER: Final = "X-A2A-Token"
KNOWN_AGENTS: Final[dict[str, int]] = {
    "codex-phase22": 7310,
    "glm-5.3": 7311,
    "fable-nmf6": 7312,
    "claude-opus-5": 7313,
    "antigravity": 7314,
}
DATA_KINDS: Final = frozenset({"gate-review-request", "coordination"})
REVIEW_GATES: Final = frozenset({1, 2, 3, 4, 5, 7})


class A2aError(RuntimeError):
    """A fail-closed transport, registry, or payload error."""


@dataclasses.dataclass(frozen=True)
class AgentRecord:
    """One fleet identity from the shared registry."""

    name: str
    port: int
    token: str = dataclasses.field(repr=False)

    @property
    def base_url(self) -> str:
        return f"http://{BIND_HOST}:{self.port}"


def _utc_now() -> str:
    return dt.datetime.now(dt.UTC).isoformat(timespec="milliseconds")


def _atomic_json(path: Path, payload: dict[str, Any]) -> None:
    tmp = path.with_suffix(f".{os.getpid()}.tmp")
    tmp.write_text(json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True))
    os.chmod(tmp, stat.S_IRUSR | stat.S_IWUSR)
    os.replace(tmp, path)


def load_registry(state_dir: Path) -> dict[str, AgentRecord]:
    path = state_dir / "agents.json"
    if not path.is_file():
        raise A2aError(f"registry {path} missing; run the init command first")
    payload = json.loads(path.read_text())
    if payload.get("schema_version") != SCHEMA_VERSION:
        raise A2aError(f"unsupported registry schema {payload.get('schema_version')!r}")
    records: dict[str, AgentRecord] = {}
    for name, entry in payload.get("agents", {}).items():
        if not IDENTITY_RE.match(name):
            raise A2aError(f"invalid agent name {name!r}")
        token = entry.get("token")
        if not isinstance(token, str) or len(token) < 16:
            raise A2aError(f"agent {name!r} has no usable token")
        records[name] = AgentRecord(name=name, port=int(entry["port"]), token=token)
    if not records:
        raise A2aError(f"registry {path} lists no agents")
    return records


def init_registry(state_dir: Path) -> dict[str, AgentRecord]:
    """Create or extend the registry; existing entries are preserved."""
    state_dir.mkdir(parents=True, exist_ok=True)
    os.chmod(state_dir, stat.S_IRUSR | stat.S_IWUSR | stat.S_IXUSR)
    path = state_dir / "agents.json"
    payload: dict[str, Any] = {"schema_version": SCHEMA_VERSION, "agents": {}}
    if path.is_file():
        payload = json.loads(path.read_text())
        if payload.get("schema_version") != SCHEMA_VERSION:
            raise A2aError(
                f"unsupported registry schema {payload.get('schema_version')!r}"
            )
    agents: dict[str, Any] = payload.setdefault("agents", {})
    for name, port in KNOWN_AGENTS.items():
        entry = agents.setdefault(name, {})
        entry["port"] = int(entry.get("port", port))
        if not isinstance(entry.get("token"), str) or len(entry["token"]) < 16:
            entry["token"] = secrets.token_urlsafe(24)
    _atomic_json(path, payload)
    return load_registry(state_dir)


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
                """
            )
        os.chmod(self.path, stat.S_IRUSR | stat.S_IWUSR)

    def record_inbound(
        self,
        message_id: str,
        sender: str,
        recipient: str,
        body: str,
        data_json: str | None,
    ) -> None:
        with self.connect() as connection:
            connection.execute(
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
                    _utc_now(),
                    _utc_now(),
                ),
            )

    def record_outbound(
        self,
        message_id: str,
        sender: str,
        recipient: str,
        body: str,
        data_json: str | None,
    ) -> None:
        with self.connect() as connection:
            connection.execute(
                """
                INSERT INTO messages (
                    message_id, direction, sender, recipient, body, data_json,
                    created_at, delivery_status
                ) VALUES (?, 'outbound', ?, ?, ?, ?, ?, 'pending')
                """,
                (message_id, sender, recipient, body, data_json, _utc_now()),
            )

    def mark_outbound(
        self,
        message_id: str,
        status: str,
        reason: str | None,
        received_at: str | None,
    ) -> None:
        if status not in ("delivered", "failed"):
            raise A2aError(f"non-terminal outbound status {status!r}")
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

    def rows(self, unread_only: bool, limit: int) -> list[sqlite3.Row]:
        query = "SELECT * FROM messages WHERE direction='inbound'"
        if unread_only:
            query += " AND read_at IS NULL"
        query += " ORDER BY created_at DESC LIMIT ?"
        with self.connect() as connection:
            return list(connection.execute(query, (limit,)).fetchall())

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
    return kind


def build_agent_card(name: str, port: int) -> AgentCard:
    return AgentCard(
        name=name,
        description=f"NM-F6 coordination endpoint for {name}",
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


def serve(name: str, state_dir: Path) -> None:
    records = load_registry(state_dir)
    if name not in records:
        raise A2aError(f"unknown agent {name!r}; registered: {sorted(records)}")
    record = records[name]
    store = A2aStore(state_dir, name)
    app = build_app(record, store)
    print(
        json.dumps(
            {
                "name": record.name,
                "port": record.port,
                "card_url": f"{record.base_url}{AGENT_CARD_WELL_KNOWN_PATH}",
                "rpc_url": f"{record.base_url}{DEFAULT_RPC_URL}",
                "pid": os.getpid(),
            },
            sort_keys=True,
        ),
        flush=True,
    )
    uvicorn.run(
        app,
        host=BIND_HOST,
        port=record.port,
        log_level="warning",
        lifespan="off",
    )


def fetch_card(record: AgentRecord, timeout: float) -> dict[str, Any]:
    response = httpx.get(
        f"{record.base_url}{AGENT_CARD_WELL_KNOWN_PATH}",
        timeout=timeout,
    )
    if response.status_code != 200:
        raise A2aError(f"card fetch returned HTTP {response.status_code}")
    card = response.json()
    if card.get("name") != record.name:
        raise A2aError(
            f"card name {card.get('name')!r} does not match registry {record.name!r}"
        )
    return card


def send_message(
    from_name: str,
    to_name: str,
    body: str,
    data_payload: dict[str, Any] | None,
    state_dir: Path,
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
    message_id = str(uuid.uuid4())
    parts: list[Part] = [new_text_part(body)]
    data_json: str | None = None
    if data_payload is not None:
        parts.append(new_data_part(data_payload))
        data_json = json.dumps(data_payload, ensure_ascii=False, sort_keys=True)
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
    store = A2aStore(state_dir, from_name)
    store.record_outbound(
        message_id=message_id,
        sender=from_name,
        recipient=to_name,
        body=body,
        data_json=data_json,
    )
    try:
        fetch_card(record, timeout=PROBE_TIMEOUT_S)
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
        store.mark_outbound(message_id, "delivered", None, _utc_now())
    except (A2aError, httpx.HTTPError) as exc:
        reason = str(exc) if isinstance(exc, A2aError) else f"peer unreachable: {exc}"
        store.mark_outbound(message_id, "failed", reason[:500], None)
        raise A2aError(reason) from exc
    return _outbound_row(store, message_id)


def _outbound_row(store: A2aStore, message_id: str) -> dict[str, Any]:
    with store.connect() as connection:
        row = store._fetch(connection, message_id, "outbound")  # noqa: SLF001
    return {key: row[key] for key in row.keys()}


def probe_peer(item: tuple[str, AgentRecord]) -> dict[str, Any]:
    name, record = item
    try:
        card = fetch_card(record, timeout=PROBE_TIMEOUT_S)
        return {
            "name": name,
            "port": record.port,
            "status": "up",
            "card_version": card.get("version"),
            "skills": [s.get("id") for s in card.get("skills", [])],
        }
    except (A2aError, httpx.HTTPError) as exc:
        return {
            "name": name,
            "port": record.port,
            "status": "down",
            "reason": str(exc)[:200],
        }


def list_peers(state_dir: Path) -> list[dict[str, Any]]:
    records = load_registry(state_dir)
    with ThreadPoolExecutor(max_workers=8) as pool:
        return list(pool.map(probe_peer, sorted(records.items())))


def _row_dict(row: sqlite3.Row) -> dict[str, Any]:
    return {key: row[key] for key in row.keys()}


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="agent_a2a", description=__doc__)
    parser.add_argument(
        "--state-dir",
        type=Path,
        default=DEFAULT_STATE_DIR,
        help="registry + per-agent stores (default: %(default)s)",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("init", help="create/extend the agent registry")

    serve_parser = sub.add_parser("serve", help="run this agent's A2A endpoint")
    serve_parser.add_argument("--name", required=True)

    send_parser = sub.add_parser("send", help="deliver one message to a peer")
    send_parser.add_argument("--from-name", required=True)
    send_parser.add_argument("--to", required=True)
    body_group = send_parser.add_mutually_exclusive_group(required=True)
    body_group.add_argument("--body")
    body_group.add_argument("--body-file", type=Path)
    body_group.add_argument("--stdin", action="store_true")
    send_parser.add_argument(
        "--data-file",
        type=Path,
        help="optional structured payload (JSON object with a kind)",
    )

    inbox_parser = sub.add_parser("inbox", help="list received messages")
    inbox_parser.add_argument("--as-name", required=True)
    inbox_parser.add_argument("--unread", action="store_true")
    inbox_parser.add_argument("--limit", type=int, default=100)
    inbox_parser.add_argument("--json", action="store_true")

    read_parser = sub.add_parser("read", help="mark one inbound message read")
    read_parser.add_argument("--as-name", required=True)
    read_parser.add_argument("message_id")

    sub.add_parser("peers", help="probe every registered agent card")
    sub.add_parser("status", help="local store summary")
    return parser


def _message_body(args: argparse.Namespace) -> str:
    if args.body is not None:
        return args.body
    if args.body_file is not None:
        return args.body_file.read_text()
    return sys.stdin.read()


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        if args.command == "init":
            records = init_registry(args.state_dir)
            print(
                json.dumps(
                    {
                        "registry": str(args.state_dir / "agents.json"),
                        "agents": {
                            name: {"port": r.port}
                            for name, r in sorted(records.items())
                        },
                    },
                    indent=2,
                    sort_keys=True,
                )
            )
            return 0

        if args.command == "serve":
            serve(args.name, args.state_dir)
            return 0

        if args.command == "send":
            data_payload = None
            if args.data_file is not None:
                loaded = json.loads(args.data_file.read_text())
                if not isinstance(loaded, dict):
                    raise A2aError("--data-file must contain a JSON object")
                data_payload = loaded
            row = send_message(
                args.from_name,
                args.to,
                _message_body(args),
                data_payload,
                args.state_dir,
            )
            print(json.dumps(row, ensure_ascii=False, sort_keys=True))
            return 0 if row["delivery_status"] == "delivered" else 2

        if args.command == "inbox":
            if args.limit < 1 or args.limit > 10_000:
                raise A2aError("--limit must be between 1 and 10000")
            sender_store = A2aStore(args.state_dir, args.as_name)
            rows = sender_store.rows(unread_only=args.unread, limit=args.limit)
            if args.json:
                print(
                    json.dumps(
                        [_row_dict(row) for row in rows],
                        ensure_ascii=False,
                        indent=2,
                        sort_keys=True,
                    )
                )
            else:
                for row in rows:
                    state = "UNREAD" if row["read_at"] is None else "READ"
                    suffix = f" data={row['data_json']}" if row["data_json"] else ""
                    print(
                        f"[{state}] {row['message_id']} from={row['sender']} "
                        f"at={row['received_at']}{suffix}\n{row['body']}\n"
                    )
            return 0

        if args.command == "read":
            sender_store = A2aStore(args.state_dir, args.as_name)
            row = sender_store.mark_read(args.message_id)
            print(json.dumps(_row_dict(row), sort_keys=True))
            return 0

        if args.command == "peers":
            print(json.dumps(list_peers(args.state_dir), indent=2, sort_keys=True))
            return 0

        if args.command == "status":
            records = load_registry(args.state_dir)
            stores = {name: A2aStore(args.state_dir, name).counts() for name in records}
            print(
                json.dumps(
                    {
                        "registry": str(args.state_dir / "agents.json"),
                        "agents": {
                            name: {"port": r.port}
                            for name, r in sorted(records.items())
                        },
                        "stores": stores,
                    },
                    indent=2,
                    sort_keys=True,
                )
            )
            return 0
    except (A2aError, sqlite3.Error, OSError, ValueError) as exc:
        print(f"agent-a2a: {exc}", file=sys.stderr)
        return 2
    raise AssertionError(f"unhandled command {args.command!r}")


if __name__ == "__main__":
    raise SystemExit(main())
