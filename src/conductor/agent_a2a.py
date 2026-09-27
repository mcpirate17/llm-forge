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
import hmac
import json
import re
import sqlite3
import subprocess
import uuid
from collections.abc import Iterator, Sequence
from functools import partialmethod
from pathlib import Path
from typing import TYPE_CHECKING, Any, Final

import httpx
from a2a.helpers.proto_helpers import (
    MessageToDict,
    get_data_parts,
    get_message_text,
    new_data_message,
    new_data_part,
    new_text_part,
)
from a2a.server.agent_execution import AgentExecutor, RequestContext
from a2a.server.context import ServerCallContext
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
    SendMessageRequest,
    Task,
)
from a2a.utils.constants import (
    AGENT_CARD_WELL_KNOWN_PATH,
    DEFAULT_RPC_URL,
    PROTOCOL_VERSION_1_0,
    VERSION_HEADER,
    TransportProtocol,
)
from a2a.utils.errors import InvalidParamsError
from starlette.applications import Starlette
from starlette.middleware.base import BaseHTTPMiddleware
from starlette.requests import Request
from starlette.responses import JSONResponse

from conductor.a2a_compaction import (
    CompactionError,
    validate_coordination_v2,
)
from conductor.a2a_registry import (
    BIND_HOST,
    DEFAULT_REAP_FAILURES,
    DEFAULT_STATE_DIR,
    IDENTITY_RE,
    KNOWN_AGENTS,
    LIVENESS_SCHEMA_VERSION,
    PROBE_TIMEOUT_S,
    ROOT,
    SCHEMA_VERSION,
    A2aError,
    AgentRecord,
    _run_native_registry,
    _utc_now,
    fetch_card,
    init_registry,
    list_peers,
    load_registry,
    probe_peer,
)

if TYPE_CHECKING:
    from conductor.a2a_cli import compact_inbox_payload

# Registry symbols remain part of the public transport facade after extraction.
__all__ = [
    "AGENT_CARD_WELL_KNOWN_PATH",
    "BIND_HOST",
    "DATA_KINDS",
    "DEFAULT_COMPACT_MESSAGES",
    "DEFAULT_CONTEXT_CHARS",
    "DEFAULT_PREVIEW_CHARS",
    "DEFAULT_REAP_FAILURES",
    "DEFAULT_STATE_DIR",
    "HEX64_RE",
    "IDENTITY_RE",
    "KNOWN_AGENTS",
    "LIVENESS_SCHEMA_VERSION",
    "MAX_BODY_BYTES",
    "MAX_PREVIEW_ROWS",
    "PROBE_TIMEOUT_S",
    "REVIEW_GATES",
    "ROOT",
    "SCHEMA_VERSION",
    "SEND_TIMEOUT_S",
    "TOKEN_HEADER",
    "A2aError",
    "A2aStore",
    "AgentRecord",
    "InboxExecutor",
    "TokenMiddleware",
    "build_agent_card",
    "build_app",
    "build_parser",
    "compact_inbox_payload",
    "fetch_card",
    "flush_queued",
    "init_registry",
    "list_peers",
    "load_registry",
    "main",
    "probe_peer",
    "reap_registry",
    "render_compact_inbox",
    "send_message",
    "serve",
    "validate_data_payload",
]

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
        from conductor._native import a2a_store_native

        self.dir = state_dir / name
        self.path = self.dir / "store.sqlite"
        try:
            self._native = a2a_store_native(str(state_dir), name)
        except ValueError as exc:
            raise A2aError(str(exc)) from exc

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

    def _record_message(
        self,
        method: str,
        message_id: str,
        sender: str,
        recipient: str,
        body: str,
        data_json: str | None,
        state: dict[str, Any] | None = None,
    ) -> None:
        selected = json.dumps(state, ensure_ascii=False) if state else None
        try:
            getattr(self._native, method)(
                message_id, sender, recipient, body, data_json, _utc_now(), selected
            )
        except ValueError as exc:
            raise A2aError(str(exc)) from exc

    record_inbound = partialmethod(_record_message, "record_inbound")
    record_outbound = partialmethod(_record_message, "record_outbound")

    def mark_outbound(
        self,
        message_id: str,
        status: str,
        reason: str | None,
        received_at: str | None,
    ) -> None:
        if status not in ("delivered", "failed", "queued"):
            raise A2aError(f"unknown outbound status {status!r}")
        try:
            self._native.mark_outbound(
                message_id, status, reason, received_at, _utc_now()
            )
        except ValueError as exc:
            raise A2aError(str(exc)) from exc

    def mark_read(self, message_id: str) -> dict[str, Any]:
        try:
            self._native.mark_read(message_id, _utc_now())
        except ValueError as exc:
            raise A2aError(str(exc)) from exc
        return self.message(message_id)

    def queued_outbound(
        self,
        to_name: str | None = None,
        *,
        limit: int | None = None,
        exclude: Sequence[str] = (),
    ) -> list[dict[str, Any]]:
        """Outbound messages awaiting redelivery, oldest first."""
        try:
            return json.loads(self._native.queued_rows(to_name, list(exclude), limit))
        except ValueError as exc:
            raise A2aError(str(exc)) from exc

    def rows(self, unread_only: bool, limit: int) -> list[dict[str, Any]]:
        try:
            return json.loads(self._native.inbound_rows(unread_only, limit))
        except ValueError as exc:
            raise A2aError(str(exc)) from exc

    def preview_rows(
        self,
        *,
        unread_only: bool,
        unpresented_only: bool = False,
        limit: int,
        preview_chars: int,
    ) -> tuple[list[dict[str, Any]], int]:
        """Read bounded inbox envelopes without materializing full bodies."""
        try:
            rows, total = json.loads(
                self._native.preview_rows(
                    unread_only, unpresented_only, limit, preview_chars
                )
            )
        except ValueError as exc:
            raise A2aError(str(exc)) from exc
        return rows, total

    def message(self, message_id: str, direction: str = "inbound") -> dict[str, Any]:
        try:
            return json.loads(self._native.fetch_row(message_id, direction))
        except ValueError as exc:
            raise A2aError(str(exc)) from exc

    def mark_presented(self, message_ids: Sequence[str]) -> int:
        try:
            return self._native.mark_presented(list(message_ids), _utc_now())
        except ValueError as exc:
            raise A2aError(str(exc)) from exc

    def resolve(self, message_id: str) -> dict[str, Any]:
        """Mark one read inbound message resolved without changing its body."""
        try:
            return json.loads(self._native.resolve(message_id, _utc_now()))
        except ValueError as exc:
            raise A2aError(str(exc)) from exc

    def set_hold(self, message_id: str, reason: str | None) -> None:
        try:
            self._native.set_hold(message_id, reason)
        except ValueError as exc:
            raise A2aError(str(exc)) from exc

    def counts(self) -> dict[str, int]:
        try:
            return json.loads(self._native.counts())
        except ValueError as exc:
            raise A2aError(str(exc)) from exc


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
        body, payloads = _validated_message_parts(message)
        # protobuf Struct has no .get(); convert it before applying dict semantics.
        sender = MessageToDict(message.metadata).get("sender", "unknown")
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


def _validated_message_parts(message: Message) -> tuple[str, list[Any]]:
    body = get_message_text(message)
    if len(body.encode()) > MAX_BODY_BYTES:
        raise A2aError(f"body exceeds {MAX_BODY_BYTES} bytes")
    payloads = get_data_parts(message.parts)
    for payload in payloads:
        validate_data_payload(payload)
    return body, payloads


class ValidatingRequestHandler(DefaultRequestHandler):
    """Map rejected message input to the SDK's JSON-RPC invalid-params error."""

    async def on_message_send(
        self, params: SendMessageRequest, context: ServerCallContext
    ) -> Message | Task:
        try:
            if not params.HasField("message"):
                raise A2aError("message/send requires a message")
            _validated_message_parts(params.message)
        except A2aError as exc:
            raise InvalidParamsError(str(exc)) from exc
        return await super().on_message_send(params, context)


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
    handler = ValidatingRequestHandler(
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
    """Keep the public Python entry point while Forge owns the live endpoint."""
    from conductor.a2a_delivery import _forge_binary

    command = [
        str(_forge_binary()),
        "mailbox",
        "--state-dir",
        str(state_dir),
        "serve",
        "--name",
        name,
    ]
    if port is not None:
        command.extend(("--port", str(port)))
    try:
        completed = subprocess.run(
            command, check=False, stderr=subprocess.PIPE, text=True
        )
    except OSError as exc:
        raise A2aError(f"cannot start native A2A serve: {exc}") from exc
    if completed.returncode:
        raise A2aError(
            completed.stderr.strip()
            or f"native A2A serve exited with status {completed.returncode}"
        )


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
    except httpx.TransportError:
        # Capability is checked again at delivery. An offline peer must not
        # prevent the sender from durably queueing a structured message.
        return None
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
    from conductor.a2a_delivery import send_message as implementation

    return implementation(
        from_name, to_name, body, data_payload, state_dir, queue_on_unreachable
    )


def flush_queued(
    state_dir: Path,
    from_name: str | None = None,
    to_name: str | None = None,
    max_messages: int = 100,
) -> list[dict[str, Any]]:
    from conductor.a2a_delivery import flush_queued as implementation

    return implementation(state_dir, from_name, to_name, max_messages)


def reap_registry(
    state_dir: Path, consecutive_failures: int = DEFAULT_REAP_FAILURES
) -> dict[str, Any]:
    """Apply native generation-guarded reaping through the public facade."""

    result = _run_native_registry(
        state_dir, "reap", ["--consecutive-failures", str(consecutive_failures)]
    )
    if not isinstance(result, dict) or not isinstance(result.get("reaped"), list):
        raise A2aError("invalid native A2A reap response")
    return result


def _row_dict(row: sqlite3.Row) -> dict[str, Any]:
    from conductor.a2a_cli import _row_dict as implementation

    return implementation(row)


def _compact_json(value: Any) -> str:
    from conductor.a2a_cli import _compact_json as implementation

    return implementation(value)


def __getattr__(name: str) -> Any:
    """Expose the canonical callable after the CLI finishes its circular import."""
    if name == "compact_inbox_payload":
        from conductor.a2a_cli import compact_inbox_payload

        return compact_inbox_payload
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


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
