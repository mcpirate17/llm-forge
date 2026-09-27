"""Python boundary checks for the Rust A2A compaction core.

The protocol, receipt, and thread algorithms are tested directly in
``native/conductor-native/src/a2a_compaction.rs``.
"""

from __future__ import annotations

import json

import pytest

from conductor.a2a_compaction import (
    CompactionError,
    compact_message,
    validate_coordination_v2,
)


def _row() -> dict[str, object]:
    return {
        "message_id": "m1",
        "direction": "inbound",
        "sender": "alice",
        "recipient": "bob",
        "body": "Please inspect the attached result.",
        "data_json": json.dumps(
            {
                "kind": "coordination-v2",
                "thread_id": "thread-1",
                "summary": "Review the bounded evidence",
                "status": "open",
                "requires_response": False,
                "supersedes": [],
            }
        ),
        "created_at": "2026-08-30T12:00:00+00:00",
        "received_at": "2026-08-30T12:00:01+00:00",
        "delivery_status": "delivered",
        "status_reason": None,
        "read_at": None,
    }


def test_mapping_payload_reaches_native_core_and_returns_python_fields() -> None:
    result = validate_coordination_v2(
        {"kind": "coordination-v2", "summary": "  one\n  two  "}
    )
    assert result == {
        "kind": "coordination-v2",
        "thread_id": None,
        "summary": "one two",
        "status": None,
        "requires_response": None,
        "supersedes": [],
    }

    row = _row()
    receipt = compact_message(row)
    assert compact_message(dict(reversed(list(row.items())))) == receipt
    assert receipt["message_id"] == "m1"
    assert receipt["summary"] == "Review the bounded evidence"
    assert receipt["actionable"] is True
    assert isinstance(receipt["receipt_sha256"], str)


def test_python_boundary_rejects_non_string_keys_and_non_json_values() -> None:
    with pytest.raises(CompactionError, match="keys must be strings"):
        validate_coordination_v2({1: "not-a-JSON-key"})
    with pytest.raises(CompactionError, match="not JSON-compatible"):
        validate_coordination_v2({"kind": "coordination-v2", "summary": object()})


def test_native_validation_error_maps_to_compaction_error() -> None:
    with pytest.raises(CompactionError, match="must not be empty"):
        validate_coordination_v2({"kind": "coordination-v2", "summary": "   "})
    row = _row()
    row["sender"] = "é" * 512
    with pytest.raises(CompactionError, match="sender"):
        compact_message(row)
