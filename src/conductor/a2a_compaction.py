"""Deterministic, model-free compaction receipts for A2A message rows.

This module is deliberately storage-agnostic.  It does not update SQLite,
delete message content, infer task completion, or call a model.  It converts
explicit message rows into bounded JSON-native receipts whose summaries are
either sender-supplied ``coordination-v2`` text or a conservative body prefix.
All omitted raw content remains bound by byte counts and SHA-256 provenance.
"""

from __future__ import annotations

import hashlib
import json
import re
from collections.abc import Iterable, Mapping
from typing import Any, Final, Protocol, cast

SCHEMA_VERSION: Final[int] = 1
AUTHORITY: Final[str] = "deterministic-a2a-compaction"

MAX_PROTOCOL_SUMMARY_BYTES: Final[int] = 1_024
MAX_COMPACT_SUMMARY_BYTES: Final[int] = 320
MAX_IDENTIFIER_CHARS: Final[int] = 128
MAX_SUPERSEDES: Final[int] = 32
MAX_INPUT_MESSAGES: Final[int] = 256
MAX_THREADS: Final[int] = 64
MAX_MESSAGES_PER_THREAD: Final[int] = 8
MAX_DETAILED_MESSAGES: Final[int] = 32
MAX_RAW_FIELD_BYTES: Final[int] = 1 << 20
MAX_METADATA_BYTES: Final[int] = 512
MAX_DATA_KIND_BYTES: Final[int] = 64

COORDINATION_V2_FIELDS: Final[frozenset[str]] = frozenset(
    {
        "kind",
        "thread_id",
        "summary",
        "status",
        "requires_response",
        "supersedes",
    }
)
COORDINATION_V2_OPTIONAL_FIELDS: Final[frozenset[str]] = frozenset(
    COORDINATION_V2_FIELDS - {"kind"}
)
COORDINATION_STATUSES: Final[frozenset[str]] = frozenset(
    {
        "open",
        "in_progress",
        "blocked",
        "resolved",
        "superseded",
        "informational",
    }
)
ACTIONABLE_STATUSES: Final[frozenset[str]] = frozenset(
    {"open", "in_progress", "blocked"}
)

_IDENTIFIER_RE: Final[re.Pattern[str]] = re.compile(
    rf"^[A-Za-z0-9][A-Za-z0-9._:/-]{{0,{MAX_IDENTIFIER_CHARS - 1}}}$"
)
_SPACE_RE: Final[re.Pattern[str]] = re.compile(r"\s+")
_ROW_FIELDS: Final[tuple[str, ...]] = (
    "message_id",
    "direction",
    "sender",
    "recipient",
    "body",
    "data_json",
    "created_at",
    "received_at",
    "delivery_status",
    "status_reason",
    "read_at",
)


class CompactionError(ValueError):
    """An A2A row or compaction request violated the bounded protocol."""


class _RowLike(Protocol):
    def keys(self) -> Iterable[str]: ...

    def __getitem__(self, key: str) -> Any: ...


def _canonical_bytes(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")


def _sha256(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _normalized_text(value: str) -> str:
    return _SPACE_RE.sub(" ", value).strip()


def _fit_utf8(value: str, max_bytes: int) -> str:
    """Fit text to a UTF-8 byte budget without splitting a code point."""

    if max_bytes < len("…".encode("utf-8")):
        raise ValueError("max_bytes is too small for the truncation marker")
    if len(value.encode("utf-8")) <= max_bytes:
        return value
    marker = "…"
    marker_bytes = len(marker.encode("utf-8"))
    low = 0
    high = len(value)
    best = ""
    while low <= high:
        midpoint = (low + high) // 2
        prefix = value[:midpoint].rstrip()
        if len(prefix.encode("utf-8")) + marker_bytes <= max_bytes:
            best = prefix
            low = midpoint + 1
        else:
            high = midpoint - 1
    return best + marker


def _validated_identifier(value: Any, *, field: str) -> str:
    if not isinstance(value, str) or not _IDENTIFIER_RE.fullmatch(value):
        raise CompactionError(
            f"{field} must be a non-empty ASCII identifier of at most "
            f"{MAX_IDENTIFIER_CHARS} characters"
        )
    return value


def validate_coordination_v2(value: Any) -> dict[str, Any]:
    """Validate and normalize one strict ``coordination-v2`` data object.

    All extension fields are optional, but ``kind`` is required so malformed
    v2-shaped legacy payloads cannot silently fall back to legacy handling.
    """

    if not isinstance(value, Mapping):
        raise CompactionError("coordination-v2 payload must be a JSON object")
    keys = set(value)
    if not all(isinstance(key, str) for key in keys):
        raise CompactionError("coordination-v2 payload keys must be strings")
    unexpected = keys - COORDINATION_V2_FIELDS
    if unexpected:
        raise CompactionError(
            f"coordination-v2 payload has unexpected fields: {sorted(unexpected)}"
        )
    if value.get("kind") != "coordination-v2":
        raise CompactionError("coordination-v2 payload requires kind='coordination-v2'")

    thread_id: str | None = None
    if "thread_id" in value:
        thread_id = _validated_identifier(value["thread_id"], field="thread_id")

    summary: str | None = None
    if "summary" in value:
        raw_summary = value["summary"]
        if not isinstance(raw_summary, str):
            raise CompactionError("coordination-v2 summary must be a string")
        summary = _normalized_text(raw_summary)
        if not summary:
            raise CompactionError("coordination-v2 summary must not be empty")
        summary_bytes = len(summary.encode("utf-8"))
        if summary_bytes > MAX_PROTOCOL_SUMMARY_BYTES:
            raise CompactionError(
                "coordination-v2 summary exceeds "
                f"{MAX_PROTOCOL_SUMMARY_BYTES} UTF-8 bytes"
            )

    status: str | None = None
    if "status" in value:
        raw_status = value["status"]
        if not isinstance(raw_status, str) or raw_status not in COORDINATION_STATUSES:
            raise CompactionError(
                f"coordination-v2 status must be one of {sorted(COORDINATION_STATUSES)}"
            )
        status = raw_status

    requires_response: bool | None = None
    if "requires_response" in value:
        raw_requires_response = value["requires_response"]
        if not isinstance(raw_requires_response, bool):
            raise CompactionError("coordination-v2 requires_response must be a boolean")
        requires_response = raw_requires_response

    supersedes: list[str] = []
    if "supersedes" in value:
        raw_supersedes = value["supersedes"]
        if not isinstance(raw_supersedes, list):
            raise CompactionError("coordination-v2 supersedes must be a list")
        if len(raw_supersedes) > MAX_SUPERSEDES:
            raise CompactionError(
                f"coordination-v2 supersedes accepts at most {MAX_SUPERSEDES} IDs"
            )
        for index, raw_message_id in enumerate(raw_supersedes):
            supersedes.append(
                _validated_identifier(raw_message_id, field=f"supersedes[{index}]")
            )
        if len(supersedes) != len(set(supersedes)):
            raise CompactionError("coordination-v2 supersedes contains duplicate IDs")

    return {
        "kind": "coordination-v2",
        "thread_id": thread_id,
        "summary": summary,
        "status": status,
        "requires_response": requires_response,
        "supersedes": supersedes,
    }


def _row_mapping(row: Any) -> dict[str, Any]:
    if isinstance(row, Mapping):
        return dict(row)
    keys = getattr(row, "keys", None)
    if callable(keys):
        row_like = cast(_RowLike, row)
        try:
            return {str(key): row_like[key] for key in row_like.keys()}
        except (KeyError, TypeError) as exc:
            raise CompactionError("A2A row could not be read as a mapping") from exc
    raise CompactionError("A2A row must be a mapping or sqlite3.Row-like object")


def _required_string(
    row: Mapping[str, Any], field: str, *, max_bytes: int = MAX_METADATA_BYTES
) -> str:
    value = row.get(field)
    if not isinstance(value, str) or not value:
        raise CompactionError(f"A2A row field {field!r} must be a non-empty string")
    if len(value.encode("utf-8")) > max_bytes:
        raise CompactionError(f"A2A row field {field!r} exceeds {max_bytes} bytes")
    return value


def _optional_string(
    row: Mapping[str, Any], field: str, *, max_bytes: int = MAX_METADATA_BYTES
) -> str | None:
    value = row.get(field)
    if value is None:
        return None
    if not isinstance(value, str):
        raise CompactionError(f"A2A row field {field!r} must be a string or null")
    if len(value.encode("utf-8")) > max_bytes:
        raise CompactionError(f"A2A row field {field!r} exceeds {max_bytes} bytes")
    return value


def _validated_row(row: Any) -> dict[str, Any]:
    raw = _row_mapping(row)
    message_id = _validated_identifier(raw.get("message_id"), field="message_id")
    direction = raw.get("direction")
    if direction not in {"inbound", "outbound"}:
        raise CompactionError("A2A row direction must be 'inbound' or 'outbound'")
    body = raw.get("body")
    if not isinstance(body, str):
        raise CompactionError("A2A row body must be a string")
    if len(body.encode("utf-8")) > MAX_RAW_FIELD_BYTES:
        raise CompactionError(f"A2A row body exceeds {MAX_RAW_FIELD_BYTES} bytes")
    normalized = {
        "message_id": message_id,
        "direction": direction,
        "sender": _required_string(raw, "sender"),
        "recipient": _required_string(raw, "recipient"),
        "body": body,
        "data_json": _optional_string(raw, "data_json", max_bytes=MAX_RAW_FIELD_BYTES),
        "created_at": _required_string(raw, "created_at"),
        "received_at": _optional_string(raw, "received_at"),
        "delivery_status": _required_string(raw, "delivery_status"),
        "status_reason": _optional_string(raw, "status_reason"),
        "read_at": _optional_string(raw, "read_at"),
    }
    return normalized


def _parsed_data(data_json: str | None) -> tuple[Any, bool]:
    if data_json is None:
        return None, True
    try:
        return json.loads(data_json), True
    except json.JSONDecodeError:
        return None, False


def _actionable(status: str | None, requires_response: bool | None) -> bool:
    if requires_response is True:
        return True
    if status is None:
        return True
    return status in ACTIONABLE_STATUSES


def _legacy_thread_id(direction: str, message_id: str) -> str:
    identity = f"{direction}\0{message_id}".encode("utf-8")
    return f"legacy-{_sha256(identity)[:24]}"


def _data_kind(data: Any) -> str | None:
    if not isinstance(data, Mapping):
        return None
    value = data.get("kind")
    if not isinstance(value, str):
        return None
    normalized = _normalized_text(value)
    return _fit_utf8(normalized, MAX_DATA_KIND_BYTES) if normalized else None


def compact_message(row: Any) -> dict[str, Any]:
    """Return a compact, source-bound receipt for one A2A message row."""

    normalized = _validated_row(row)
    data_json = normalized["data_json"]
    data, data_json_valid = _parsed_data(data_json)

    protocol = "legacy"
    coordination: dict[str, Any] | None = None
    if isinstance(data, Mapping) and data.get("kind") == "coordination-v2":
        coordination = validate_coordination_v2(data)
        protocol = "coordination-v2"
    elif isinstance(data, Mapping) and COORDINATION_V2_OPTIONAL_FIELDS.intersection(
        data
    ):
        raise CompactionError(
            "coordination-v2 fields require an explicit kind='coordination-v2'"
        )

    message_id = normalized["message_id"]
    if coordination is not None and message_id in coordination["supersedes"]:
        raise CompactionError("a coordination-v2 message cannot supersede itself")

    explicit_summary = coordination["summary"] if coordination is not None else None
    body_summary = _normalized_text(normalized["body"])
    if explicit_summary is not None:
        summary = explicit_summary
        summary_source = "coordination-v2.summary"
    elif body_summary:
        summary = body_summary
        summary_source = "body-fallback"
    else:
        data_kind = _data_kind(data)
        label = data_kind or "legacy"
        summary = f"{label} message {message_id}"
        summary_source = "deterministic-label"
    summary = _fit_utf8(summary, MAX_COMPACT_SUMMARY_BYTES)

    body_bytes = normalized["body"].encode("utf-8")
    data_bytes = data_json.encode("utf-8") if data_json is not None else b""
    source_payload = {field: normalized[field] for field in _ROW_FIELDS}
    source_sha256 = _sha256(_canonical_bytes(source_payload))
    content_sha256 = _sha256(
        b"body\0"
        + body_bytes
        + (b"\0data:null" if data_json is None else b"\0data:" + data_bytes)
    )

    status = coordination["status"] if coordination is not None else None
    requires_response = (
        coordination["requires_response"] if coordination is not None else None
    )
    thread_id = (
        coordination["thread_id"]
        if coordination is not None and coordination["thread_id"] is not None
        else _legacy_thread_id(normalized["direction"], message_id)
    )
    supersedes = coordination["supersedes"] if coordination is not None else []
    receipt: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "authority": AUTHORITY,
        "message_id": message_id,
        "direction": normalized["direction"],
        "sender": normalized["sender"],
        "recipient": normalized["recipient"],
        "created_at": normalized["created_at"],
        "received_at": normalized["received_at"],
        "read_at": normalized["read_at"],
        "delivery_status": normalized["delivery_status"],
        "protocol": protocol,
        "data_kind": _data_kind(data),
        "data_json_valid": data_json_valid,
        "thread_id": thread_id,
        "summary": summary,
        "summary_source": summary_source,
        "status": status,
        "requires_response": requires_response,
        "actionable": _actionable(status, requires_response),
        "supersedes": list(supersedes),
        "raw_body_bytes": len(body_bytes),
        "raw_data_bytes": len(data_bytes),
        "omitted_raw_bytes": len(body_bytes) + len(data_bytes),
        "body_sha256": _sha256(body_bytes),
        "data_sha256": _sha256(data_bytes) if data_json is not None else None,
        "content_sha256": content_sha256,
        "source_sha256": source_sha256,
    }
    receipt["receipt_sha256"] = _sha256(_canonical_bytes(receipt))
    return receipt


def _positive_bound(value: Any, *, name: str, maximum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise CompactionError(f"{name} must be an integer")
    if value < 1 or value > maximum:
        raise CompactionError(f"{name} must be between 1 and {maximum}")
    return value


def _receipt_sort_key(receipt: Mapping[str, Any]) -> tuple[str, str, str, str]:
    return (
        str(receipt["created_at"]),
        str(receipt["message_id"]),
        str(receipt["direction"]),
        str(receipt["source_sha256"]),
    )


def _unique_receipts(rows: Iterable[Any]) -> list[dict[str, Any]]:
    receipts: list[dict[str, Any]] = []
    identities: dict[tuple[str, str], str] = {}
    for row in rows:
        if len(receipts) >= MAX_INPUT_MESSAGES:
            raise CompactionError(
                f"compaction accepts at most {MAX_INPUT_MESSAGES} distinct messages"
            )
        receipt = compact_message(row)
        identity = (receipt["direction"], receipt["message_id"])
        previous = identities.get(identity)
        if previous is not None:
            if previous != receipt["source_sha256"]:
                raise CompactionError(
                    f"conflicting duplicate A2A row for {identity[0]}:{identity[1]}"
                )
            continue
        identities[identity] = receipt["source_sha256"]
        receipts.append(receipt)
    return receipts


def _group_receipts(
    receipts: Iterable[dict[str, Any]], *, max_threads: int
) -> dict[str, list[dict[str, Any]]]:
    groups: dict[str, list[dict[str, Any]]] = {}
    for receipt in receipts:
        groups.setdefault(receipt["thread_id"], []).append(receipt)
    if len(groups) > max_threads:
        raise CompactionError(
            f"compaction found {len(groups)} threads, exceeding max_threads={max_threads}"
        )
    return groups


def _selected_receipts(
    ordered: list[dict[str, Any]],
    *,
    thread_id: str,
    max_messages_per_thread: int,
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    actionable = [receipt for receipt in ordered if receipt["actionable"]]
    if len(actionable) > max_messages_per_thread:
        raise CompactionError(
            f"thread {thread_id!r} has {len(actionable)} actionable messages; "
            f"detail cap is {max_messages_per_thread}"
        )
    actionable_hashes = {receipt["receipt_sha256"] for receipt in actionable}
    remaining_slots = max_messages_per_thread - len(actionable)
    non_actionable = [
        receipt
        for receipt in ordered
        if receipt["receipt_sha256"] not in actionable_hashes
    ]
    selected_non_actionable = (
        non_actionable[-remaining_slots:] if remaining_slots else []
    )
    selected_hashes = actionable_hashes | {
        receipt["receipt_sha256"] for receipt in selected_non_actionable
    }
    selected = [
        receipt for receipt in ordered if receipt["receipt_sha256"] in selected_hashes
    ]
    return actionable, selected


def _thread_digest(
    thread_id: str,
    receipts: list[dict[str, Any]],
    *,
    max_messages_per_thread: int,
) -> dict[str, Any]:
    ordered = sorted(receipts, key=_receipt_sort_key)
    actionable, selected = _selected_receipts(
        ordered,
        thread_id=thread_id,
        max_messages_per_thread=max_messages_per_thread,
    )
    supersession_edges = [
        {"message_id": receipt["message_id"], "supersedes": target}
        for receipt in ordered
        for target in receipt["supersedes"]
    ]
    provenance = [
        {
            "message_id": receipt["message_id"],
            "direction": receipt["direction"],
            "source_sha256": receipt["source_sha256"],
            "receipt_sha256": receipt["receipt_sha256"],
        }
        for receipt in ordered
    ]
    digest_payload = {
        "thread_id": thread_id,
        "provenance": provenance,
        "supersession_edges": supersession_edges,
    }
    return {
        "thread_id": thread_id,
        "message_count": len(ordered),
        "message_ids": [receipt["message_id"] for receipt in ordered],
        "actionable_count": len(actionable),
        "actionable_message_ids": [receipt["message_id"] for receipt in actionable],
        "omitted_raw_bytes": sum(receipt["omitted_raw_bytes"] for receipt in ordered),
        "provenance": provenance,
        "supersession_edges": supersession_edges,
        "messages": selected,
        "omitted_message_details": len(ordered) - len(selected),
        "thread_sha256": _sha256(_canonical_bytes(digest_payload)),
    }


def compact_threads(
    rows: Iterable[Any],
    *,
    max_threads: int = MAX_THREADS,
    max_messages_per_thread: int = MAX_MESSAGES_PER_THREAD,
) -> dict[str, Any]:
    """Group rows into deterministic bounded thread digests.

    Every message ID, source hash, actionable ID, supersession edge, and raw
    byte count is preserved.  Detailed receipts are bounded; if the detail cap
    cannot retain every actionable message, compaction fails closed.
    """

    max_threads = _positive_bound(max_threads, name="max_threads", maximum=MAX_THREADS)
    max_messages_per_thread = _positive_bound(
        max_messages_per_thread,
        name="max_messages_per_thread",
        maximum=MAX_DETAILED_MESSAGES,
    )
    receipts = _unique_receipts(rows)
    groups = _group_receipts(receipts, max_threads=max_threads)
    threads = [
        _thread_digest(
            thread_id,
            groups[thread_id],
            max_messages_per_thread=max_messages_per_thread,
        )
        for thread_id in sorted(groups)
    ]

    result: dict[str, Any] = {
        "schema_version": SCHEMA_VERSION,
        "authority": AUTHORITY,
        "thread_count": len(threads),
        "message_count": len(receipts),
        "actionable_count": sum(thread["actionable_count"] for thread in threads),
        "omitted_raw_bytes": sum(thread["omitted_raw_bytes"] for thread in threads),
        "threads": threads,
    }
    result["compaction_sha256"] = _sha256(_canonical_bytes(result))
    return result
