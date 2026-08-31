"""Bounded presentation and command dispatch for :mod:`conductor.agent_a2a`."""

from __future__ import annotations

import argparse
import json
import sqlite3
import sys
import time
from collections.abc import Callable, Sequence
from pathlib import Path
from typing import Any

from conductor.a2a_compaction import COORDINATION_STATUSES
from conductor.a2a_graph_context import (
    DEFAULT_CODE_CONTEXT_CHARS,
    DEFAULT_CONTEXT_SCAN_CHARS,
    build_bounded_code_context,
    read_context_fragments,
)
from conductor.a2a_registry import (
    DEFAULT_REAP_FAILURES,
    DEFAULT_STATE_DIR,
    A2aError,
    init_registry,
    list_peers,
    load_registry,
)
from conductor.agent_a2a import (
    DEFAULT_COMPACT_MESSAGES,
    DEFAULT_CONTEXT_CHARS,
    DEFAULT_PREVIEW_CHARS,
    MAX_PREVIEW_ROWS,
    A2aStore,
    flush_queued,
    reap_registry,
    send_message,
    serve,
)


def _row_dict(row: sqlite3.Row) -> dict[str, Any]:
    return {key: row[key] for key in row.keys()}  # noqa: SIM118


def _compact_json(value: Any) -> str:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    )


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

    if not 1 <= max_messages <= DEFAULT_COMPACT_MESSAGES:
        raise A2aError(
            f"--max-messages must be between 1 and {DEFAULT_COMPACT_MESSAGES}"
        )
    if not 32 <= preview_chars <= 320:
        raise A2aError("--preview-chars must be between 32 and 320")
    if not 256 <= max_chars <= 10_000:
        raise A2aError("--max-chars must be between 256 and 10000")
    rows, total = store.preview_rows(
        unread_only=unread_only,
        unpresented_only=unpresented_only,
        limit=min(MAX_PREVIEW_ROWS, max_messages),
        preview_chars=preview_chars,
    )
    total_raw_bytes = int(rows[0]["total_raw_bytes"]) if rows else 0

    def message(row: dict[str, Any], summary_chars: int) -> dict[str, Any]:
        summary = " ".join(str(row["summary"]).split())
        if len(summary) > summary_chars:
            summary = summary[: summary_chars - 1].rstrip() + "…"
        return {
            "id": row["message_id"],
            "from": row["sender"],
            "at": row["received_at"] or row["created_at"],
            "thread": row["thread_id"],
            "status": row["protocol_status"],
            "requires_response": bool(row["requires_response"]),
            "summary": summary,
            "raw_bytes": int(row["body_bytes"]) + int(row["data_bytes"]),
        }

    shown_rows = list(rows)
    summary_chars = preview_chars
    while True:
        messages = [message(row, summary_chars) for row in shown_rows]
        payload = {
            "schema_version": 1,
            "authority": "bounded-a2a-inbox",
            "agent": agent,
            "unread_only": unread_only,
            "total": total,
            "shown": len(messages),
            "omitted": total - len(messages),
            "raw_bytes_not_injected": total_raw_bytes,
            "messages": messages,
        }
        if len(_compact_json(payload)) <= max_chars:
            return payload, [str(row["message_id"]) for row in shown_rows]
        if summary_chars > 32:
            summary_chars = max(32, summary_chars // 2)
            continue
        if shown_rows:
            shown_rows.pop()
            continue
        raise A2aError(
            f"--max-chars {max_chars} is too small for the compact inbox envelope"
        )


def render_compact_inbox(payload: dict[str, Any]) -> str:
    lines = [
        (
            f"A2A compact agent={payload['agent']} total={payload['total']} "
            f"shown={payload['shown']} omitted={payload['omitted']}"
        )
    ]
    for message in payload["messages"]:
        response = " response=yes" if message["requires_response"] else ""
        lines.append(
            f"[{message['status']}] {message['id']} from={message['from']}"
            f" thread={message['thread']}{response}\n  {message['summary']}"
        )
    if payload["omitted"]:
        lines.append(f"(+{payload['omitted']} more; rerun or show by message id)")
    if "code_context" in payload:
        lines.append("code_context=" + _compact_json(payload["code_context"]))
    lines.append(
        f"raw bytes withheld from context: {payload['raw_bytes_not_injected']}"
    )
    return "\n".join(lines)


def _add_identity_commands(sub: argparse._SubParsersAction[Any]) -> None:
    init_parser = sub.add_parser(
        "init", help="initialize one named identity without creating a roster"
    )
    init_parser.add_argument("--name", help="identity to initialize")
    init_parser.add_argument(
        "--port", type=int, help="port for a name outside the built-in port catalog"
    )
    serve_parser = sub.add_parser("serve", help="run this agent's A2A endpoint")
    serve_parser.add_argument("--name", required=True)
    serve_parser.add_argument(
        "--port", type=int, help="port for a name not already in the registry"
    )


def _add_send_commands(sub: argparse._SubParsersAction[Any]) -> None:
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
    send_parser.add_argument("--thread-id")
    send_parser.add_argument("--summary")
    send_parser.add_argument("--status", choices=sorted(COORDINATION_STATUSES))
    response_group = send_parser.add_mutually_exclusive_group()
    response_group.add_argument("--requires-response", action="store_true")
    response_group.add_argument("--no-response", action="store_true")
    send_parser.add_argument("--supersedes", action="append", default=[])
    send_parser.add_argument(
        "--no-queue",
        action="store_true",
        help="fail terminally when the peer is unreachable instead of queueing",
    )
    flush_parser = sub.add_parser(
        "flush", help="redeliver queued messages to now-reachable peers"
    )
    flush_parser.add_argument(
        "--as-name", help="flush only this sender's queue (default: all local stores)"
    )
    flush_parser.add_argument("--to", help="flush only messages to this recipient")


def _add_inbox_commands(sub: argparse._SubParsersAction[Any]) -> None:
    inbox_parser = sub.add_parser("inbox", help="list received messages")
    inbox_parser.add_argument("--as-name", required=True)
    inbox_parser.add_argument("--unread", action="store_true")
    inbox_parser.add_argument("--limit", type=int, default=100)
    inbox_parser.add_argument("--json", action="store_true")
    inbox_view = inbox_parser.add_mutually_exclusive_group()
    inbox_view.add_argument(
        "--compact",
        action="store_true",
        help="bounded metadata/summary view (the default)",
    )
    inbox_view.add_argument(
        "--full",
        action="store_true",
        help="explicitly include complete bodies and structured data",
    )
    inbox_parser.add_argument(
        "--max-messages", type=int, default=DEFAULT_COMPACT_MESSAGES
    )
    inbox_parser.add_argument(
        "--preview-chars", type=int, default=DEFAULT_PREVIEW_CHARS
    )
    inbox_parser.add_argument("--max-chars", type=int, default=DEFAULT_CONTEXT_CHARS)

    read_parser = sub.add_parser("read", help="mark one inbound message read")
    read_parser.add_argument("--as-name", required=True)
    read_parser.add_argument("message_id")
    show_parser = sub.add_parser(
        "show", help="explicitly retrieve one complete message"
    )
    show_parser.add_argument("--as-name", required=True)
    show_parser.add_argument(
        "--direction", choices=("inbound", "outbound"), default="inbound"
    )
    show_parser.add_argument("--json", action="store_true")
    show_parser.add_argument("message_id")


def _add_lifecycle_commands(sub: argparse._SubParsersAction[Any]) -> None:
    resolve_parser = sub.add_parser(
        "resolve", help="mark one read inbound message resolved"
    )
    resolve_parser.add_argument("--as-name", required=True)
    resolve_parser.add_argument("message_id")
    hold_parser = sub.add_parser(
        "hold", help="set or clear a retention hold on a protocol message"
    )
    hold_parser.add_argument("--as-name", required=True)
    hold_parser.add_argument("message_id")
    hold_value = hold_parser.add_mutually_exclusive_group(required=True)
    hold_value.add_argument("--reason")
    hold_value.add_argument("--clear", action="store_true")


def _add_watch_and_status_commands(sub: argparse._SubParsersAction[Any]) -> None:
    watch_parser = sub.add_parser(
        "watch", help="emit bounded compact batches for newly presented messages"
    )
    watch_parser.add_argument("--as-name", required=True)
    watch_parser.add_argument("--interval", type=float, default=120.0)
    watch_parser.add_argument("--once", action="store_true")
    watch_parser.add_argument(
        "--max-messages", type=int, default=DEFAULT_COMPACT_MESSAGES
    )
    watch_parser.add_argument(
        "--preview-chars", type=int, default=DEFAULT_PREVIEW_CHARS
    )
    watch_parser.add_argument("--max-chars", type=int, default=DEFAULT_CONTEXT_CHARS)
    watch_parser.add_argument(
        "--code-context",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="attach bounded AST/graph context for concrete Python references",
    )
    watch_parser.add_argument(
        "--max-code-context-chars",
        type=int,
        default=DEFAULT_CODE_CONTEXT_CHARS,
    )
    watch_parser.add_argument(
        "--context-scan-chars", type=int, default=DEFAULT_CONTEXT_SCAN_CHARS
    )
    watch_parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    watch_parser.add_argument("--json", action="store_true")
    sub.add_parser("peers", help="probe every registered agent card")
    reap_parser = sub.add_parser(
        "reap", help="probe peers and remove identities down for N consecutive probes"
    )
    reap_parser.add_argument(
        "--consecutive-failures",
        type=int,
        default=DEFAULT_REAP_FAILURES,
        help="failure streak required before removal (default: %(default)s)",
    )
    sub.add_parser("status", help="local store summary")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="agent_a2a", description="A2A protocol transport for local agents"
    )
    parser.add_argument(
        "--state-dir",
        type=Path,
        default=DEFAULT_STATE_DIR,
        help="registry + per-agent stores (default: %(default)s)",
    )
    sub = parser.add_subparsers(dest="command", required=True)
    _add_identity_commands(sub)
    _add_send_commands(sub)
    _add_inbox_commands(sub)
    _add_lifecycle_commands(sub)
    _add_watch_and_status_commands(sub)
    return parser


def _message_body(args: argparse.Namespace) -> str:
    if args.body is not None:
        return args.body
    if args.body_file is not None:
        return args.body_file.read_text()
    return sys.stdin.read()


def _cmd_init(args: argparse.Namespace) -> int:
    records = init_registry(args.state_dir, name=args.name, port=args.port)
    print(
        json.dumps(
            {
                "registry": str(args.state_dir / "agents.json"),
                "agents": {
                    name: {"port": record.port}
                    for name, record in sorted(records.items())
                },
            },
            indent=2,
            sort_keys=True,
        )
    )
    return 0


def _cmd_serve(args: argparse.Namespace) -> int:
    serve(args.name, args.state_dir, port=args.port)
    return 0


def _cmd_send(args: argparse.Namespace) -> int:
    data_payload: dict[str, Any] | None = None
    protocol_requested = any(
        (
            args.thread_id is not None,
            args.summary is not None,
            args.status is not None,
            args.requires_response,
            args.no_response,
            bool(args.supersedes),
        )
    )
    if protocol_requested and args.data_file is not None:
        raise A2aError("coordination-v2 flags cannot be combined with --data-file")
    if args.data_file is not None:
        loaded = json.loads(args.data_file.read_text())
        if not isinstance(loaded, dict):
            raise A2aError("--data-file must contain a JSON object")
        data_payload = loaded
    elif protocol_requested:
        data_payload = {"kind": "coordination-v2"}
        if args.thread_id is not None:
            data_payload["thread_id"] = args.thread_id
        if args.summary is not None:
            data_payload["summary"] = args.summary
        if args.status is not None:
            data_payload["status"] = args.status
        if args.requires_response or args.no_response:
            data_payload["requires_response"] = bool(args.requires_response)
        if args.supersedes:
            data_payload["supersedes"] = args.supersedes
    row = send_message(
        args.from_name,
        args.to,
        _message_body(args),
        data_payload,
        args.state_dir,
        queue_on_unreachable=not args.no_queue,
    )
    print(json.dumps(row, ensure_ascii=False, sort_keys=True))
    if row["delivery_status"] == "delivered":
        return 0
    return 3 if row["delivery_status"] == "queued" else 2


def _cmd_flush(args: argparse.Namespace) -> int:
    results = flush_queued(args.state_dir, from_name=args.as_name, to_name=args.to)
    print(json.dumps(results, ensure_ascii=False, indent=2, sort_keys=True))
    return 3 if any(row["status"] == "queued" for row in results) else 0


def _cmd_inbox(args: argparse.Namespace) -> int:
    if args.limit < 1 or args.limit > 10_000:
        raise A2aError("--limit must be between 1 and 10000")
    store = A2aStore(args.state_dir, args.as_name)
    if args.full:
        rows = store.rows(unread_only=args.unread, limit=args.limit)
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
    payload, _ = compact_inbox_payload(
        store,
        agent=args.as_name,
        unread_only=args.unread,
        unpresented_only=False,
        max_messages=args.max_messages,
        preview_chars=args.preview_chars,
        max_chars=args.max_chars,
    )
    print(_compact_json(payload) if args.json else render_compact_inbox(payload))
    return 0


def _cmd_read(args: argparse.Namespace) -> int:
    row = A2aStore(args.state_dir, args.as_name).mark_read(args.message_id)
    print(
        _compact_json(
            {
                "message_id": row["message_id"],
                "sender": row["sender"],
                "read_at": row["read_at"],
                "state": "read",
            }
        )
    )
    return 0


def _cmd_show(args: argparse.Namespace) -> int:
    row = A2aStore(args.state_dir, args.as_name).message(
        args.message_id, args.direction
    )
    if args.json:
        print(json.dumps(_row_dict(row), ensure_ascii=False, sort_keys=True))
    else:
        print(
            f"{row['direction']} {row['message_id']} from={row['sender']} "
            f"to={row['recipient']} at={row['created_at']}\n{row['body']}"
        )
        if row["data_json"]:
            print(f"data={row['data_json']}")
    return 0


def _cmd_resolve(args: argparse.Namespace) -> int:
    row = A2aStore(args.state_dir, args.as_name).resolve(args.message_id)
    print(
        _compact_json(
            {
                "message_id": row["message_id"],
                "sender": row["sender"],
                "state": "resolved",
            }
        )
    )
    return 0


def _cmd_hold(args: argparse.Namespace) -> int:
    store = A2aStore(args.state_dir, args.as_name)
    store.set_hold(args.message_id, None if args.clear else args.reason)
    print(
        _compact_json(
            {
                "message_id": args.message_id,
                "hold": None if args.clear else args.reason,
            }
        )
    )
    return 0


def _cmd_watch(args: argparse.Namespace) -> int:
    if not 5.0 <= args.interval <= 3600.0:
        raise A2aError("--interval must be between 5 and 3600 seconds")
    store = A2aStore(args.state_dir, args.as_name)
    while True:
        payload, message_ids = compact_inbox_payload(
            store,
            agent=args.as_name,
            unread_only=True,
            unpresented_only=True,
            max_messages=args.max_messages,
            preview_chars=args.preview_chars,
            max_chars=args.max_chars,
        )
        if payload["total"]:
            if args.code_context:
                fragments = read_context_fragments(
                    store.path,
                    message_ids,
                    scan_chars=args.context_scan_chars,
                )
                code_context = build_bounded_code_context(
                    args.repo_root,
                    fragments,
                    max_chars=args.max_code_context_chars,
                )
                if code_context["contexts"]:
                    payload["code_context"] = code_context
            print(
                _compact_json(payload) if args.json else render_compact_inbox(payload),
                flush=True,
            )
            store.mark_presented(message_ids)
        if args.once:
            return 0
        time.sleep(args.interval)


def _cmd_peers(args: argparse.Namespace) -> int:
    print(json.dumps(list_peers(args.state_dir), indent=2, sort_keys=True))
    return 0


def _cmd_reap(args: argparse.Namespace) -> int:
    result = reap_registry(
        args.state_dir, consecutive_failures=args.consecutive_failures
    )
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


def _cmd_status(args: argparse.Namespace) -> int:
    records = load_registry(args.state_dir)
    stores = {name: A2aStore(args.state_dir, name).counts() for name in records}
    print(
        json.dumps(
            {
                "registry": str(args.state_dir / "agents.json"),
                "agents": {
                    name: {"port": record.port}
                    for name, record in sorted(records.items())
                },
                "stores": stores,
            },
            indent=2,
            sort_keys=True,
        )
    )
    return 0


CommandHandler = Callable[[argparse.Namespace], int]
COMMANDS: dict[str, CommandHandler] = {
    "init": _cmd_init,
    "serve": _cmd_serve,
    "send": _cmd_send,
    "flush": _cmd_flush,
    "inbox": _cmd_inbox,
    "read": _cmd_read,
    "show": _cmd_show,
    "resolve": _cmd_resolve,
    "hold": _cmd_hold,
    "watch": _cmd_watch,
    "peers": _cmd_peers,
    "reap": _cmd_reap,
    "status": _cmd_status,
}


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return COMMANDS[args.command](args)
    except (A2aError, sqlite3.Error, OSError, ValueError) as exc:
        print(f"agent-a2a: {exc}", file=sys.stderr)
        return 2
