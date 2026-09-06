"""Thin Python boundary for deterministic native A2A compaction.

Mapping-like SQLite rows are normalized at the ecosystem boundary. The bounded
validation, hashing, deduplication, ordering, and thread state machine are owned
by :mod:`conductor_native`.
"""

from __future__ import annotations

import json
from collections.abc import Iterable, Mapping
from typing import Any, Final, Protocol, cast

from conductor._native import (
    a2a_compact_message_native,
    a2a_validate_coordination_v2_native,
)

AUTHORITY: Final[str] = "deterministic-a2a-compaction"

MAX_PROTOCOL_SUMMARY_BYTES: Final[int] = 1_024
MAX_COMPACT_SUMMARY_BYTES: Final[int] = 320
MAX_SUPERSEDES: Final[int] = 32
MAX_METADATA_BYTES: Final[int] = 512
MAX_DATA_KIND_BYTES: Final[int] = 64

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


def _row_mapping(row: Any) -> dict[str, Any]:
    if isinstance(row, Mapping):
        return dict(row)
    keys = getattr(row, "keys", None)
    if callable(keys):
        row_like = cast(_RowLike, row)
        try:
            # sqlite3.Row iterates values, so `.keys()` is required here.
            return {str(key): row_like[key] for key in row_like.keys()}  # noqa: SIM118
        except (KeyError, TypeError) as exc:
            raise CompactionError("A2A row could not be read as a mapping") from exc
    raise CompactionError("A2A row must be a mapping or sqlite3.Row-like object")


def _json_input(value: Any) -> str:
    try:
        return json.dumps(
            value,
            ensure_ascii=False,
            separators=(",", ":"),
            allow_nan=False,
        )
    except (TypeError, ValueError, OverflowError) as exc:
        # fmt: off
        raise CompactionError(f"A2A compaction input is not JSON-compatible: {exc}") from exc
        # fmt: on


def _native_result(function: Any, value: Any) -> dict[str, Any]:
    try:
        result = function(_json_input(value))
    except ValueError as exc:
        raise CompactionError(str(exc)) from exc
    return cast(dict[str, Any], json.loads(result))


def _row_payload(row: Any) -> dict[str, Any]:
    mapping = _row_mapping(row)
    return {field: mapping.get(field) for field in _ROW_FIELDS}


def validate_coordination_v2(value: Any) -> dict[str, Any]:
    """Validate and normalize one strict ``coordination-v2`` data object."""

    if isinstance(value, Mapping):
        keys = set(value)
        if not all(isinstance(key, str) for key in keys):
            raise CompactionError("coordination-v2 payload keys must be strings")
        value = dict(value)
    return _native_result(a2a_validate_coordination_v2_native, value)


def compact_message(row: Any) -> dict[str, Any]:
    """Return a compact, source-bound receipt for one A2A message row."""

    return _native_result(a2a_compact_message_native, _row_payload(row))
