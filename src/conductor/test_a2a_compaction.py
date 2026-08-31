"""Tests for deterministic, model-free A2A compaction receipts."""

from __future__ import annotations

import json

import pytest

from conductor.a2a_compaction import (
    ACTIONABLE_STATUSES,
    AUTHORITY,
    COORDINATION_STATUSES,
    MAX_COMPACT_SUMMARY_BYTES,
    MAX_DATA_KIND_BYTES,
    MAX_INPUT_MESSAGES,
    MAX_METADATA_BYTES,
    MAX_PROTOCOL_SUMMARY_BYTES,
    MAX_SUPERSEDES,
    CompactionError,
    compact_message,
    compact_threads,
    validate_coordination_v2,
)


def _row(
    message_id: str = "m1",
    *,
    direction: str = "inbound",
    body: str = "Please inspect the attached result.",
    data: object | None = None,
    created_at: str = "2026-08-30T12:00:00+00:00",
    read_at: str | None = None,
) -> dict[str, object]:
    return {
        "message_id": message_id,
        "direction": direction,
        "sender": "alice",
        "recipient": "bob",
        "body": body,
        "data_json": (
            json.dumps(data, ensure_ascii=False, sort_keys=True)
            if data is not None
            else None
        ),
        "created_at": created_at,
        "received_at": "2026-08-30T12:00:01+00:00",
        "delivery_status": "delivered",
        "status_reason": None,
        "read_at": read_at,
    }


def _v2(**overrides: object) -> dict[str, object]:
    payload: dict[str, object] = {
        "kind": "coordination-v2",
        "thread_id": "thread-1",
        "summary": "Compact status",
        "status": "open",
        "requires_response": False,
        "supersedes": [],
    }
    payload.update(overrides)
    return payload


def _assert_validate_coordination_v2_normalizes_only_summary_whitespace() -> None:
    result = validate_coordination_v2(
        _v2(summary="  one\n  two  ", supersedes=["m-old", "m-older"])
    )

    assert result == {
        "kind": "coordination-v2",
        "thread_id": "thread-1",
        "summary": "one two",
        "status": "open",
        "requires_response": False,
        "supersedes": ["m-old", "m-older"],
    }


@pytest.mark.parametrize(
    "payload",
    [
        None,
        [],
        {},
        {1: "not-a-JSON-key"},
        {"kind": "coordination"},
        {"kind": "coordination-v2", "unknown": 1},
        {"kind": "coordination-v2", "thread_id": "bad id"},
        {"kind": "coordination-v2", "summary": "   "},
        {"kind": "coordination-v2", "status": "done"},
        {"kind": "coordination-v2", "requires_response": 1},
        {"kind": "coordination-v2", "supersedes": "m1"},
        {"kind": "coordination-v2", "supersedes": ["bad id"]},
        {"kind": "coordination-v2", "supersedes": ["m1", "m1"]},
    ],
)
def test_validate_coordination_v2_rejects_invalid_protocol(payload: object) -> None:
    with pytest.raises(CompactionError):
        validate_coordination_v2(payload)


def test_validate_coordination_v2_enforces_utf8_summary_and_supersedes_bounds() -> None:
    with pytest.raises(CompactionError, match="UTF-8 bytes"):
        validate_coordination_v2(
            {"kind": "coordination-v2", "summary": "é" * MAX_PROTOCOL_SUMMARY_BYTES}
        )
    with pytest.raises(CompactionError, match="at most"):
        validate_coordination_v2(
            {
                "kind": "coordination-v2",
                "supersedes": [f"m-{index}" for index in range(MAX_SUPERSEDES + 1)],
            }
        )
    _assert_validate_coordination_v2_normalizes_only_summary_whitespace()
    _assert_invalid_v2_shape_cannot_silently_fall_back_to_legacy()


def _assert_compact_message_is_deterministic_and_contains_no_raw_payload() -> None:
    row = _row(data=_v2(summary="Review the bounded evidence"))

    first = compact_message(row)
    second = compact_message(dict(reversed(list(row.items()))))

    assert first == second
    assert first["authority"] == AUTHORITY
    assert first["summary"] == "Review the bounded evidence"
    rendered = json.dumps(first, ensure_ascii=False)
    raw_body = row["body"]
    raw_data = row["data_json"]
    assert isinstance(raw_body, str)
    assert isinstance(raw_data, str)
    assert raw_body not in rendered
    assert raw_data not in rendered
    assert len(first["source_sha256"]) == 64
    assert len(first["receipt_sha256"]) == 64


def test_legacy_fallback_is_conservative_bounded_and_model_free() -> None:
    receipt = compact_message(
        _row(body=("  legacy\nmessage  " * 100), data={"kind": "coordination"})
    )

    assert receipt["protocol"] == "legacy"
    assert receipt["summary_source"] == "body-fallback"
    assert len(receipt["summary"].encode("utf-8")) <= MAX_COMPACT_SUMMARY_BYTES
    assert receipt["summary"].endswith("…")
    assert receipt["status"] is None
    assert receipt["requires_response"] is None
    assert receipt["actionable"] is True
    _assert_compact_message_is_deterministic_and_contains_no_raw_payload()
    _assert_empty_legacy_body_uses_deterministic_label()
    _assert_all_emitted_prose_labels_obey_utf8_byte_bounds()


def _assert_empty_legacy_body_uses_deterministic_label() -> None:
    receipt = compact_message(_row(body="", data={"kind": "gate-review-request"}))

    assert receipt["summary"] == "gate-review-request message m1"
    assert receipt["summary_source"] == "deterministic-label"


@pytest.mark.parametrize("status", sorted(COORDINATION_STATUSES))
@pytest.mark.parametrize("requires_response", [False, True])
def test_actionability_preserves_status_and_requires_response_independently(
    status: str, requires_response: bool
) -> None:
    receipt = compact_message(
        _row(data=_v2(status=status, requires_response=requires_response))
    )

    assert receipt["status"] == status
    assert receipt["requires_response"] is requires_response
    assert receipt["actionable"] is (requires_response or status in ACTIONABLE_STATUSES)


def test_missing_v2_status_fails_closed_as_actionable() -> None:
    receipt = compact_message(
        _row(data={"kind": "coordination-v2", "requires_response": False})
    )

    assert receipt["status"] is None
    assert receipt["requires_response"] is False
    assert receipt["actionable"] is True


def _assert_invalid_v2_shape_cannot_silently_fall_back_to_legacy() -> None:
    with pytest.raises(CompactionError, match="explicit kind"):
        compact_message(_row(data={"summary": "looks like v2"}))
    with pytest.raises(CompactionError, match="supersede itself"):
        compact_message(_row(data=_v2(supersedes=["m1"])))


def test_invalid_legacy_json_is_hashed_and_safely_summarized() -> None:
    row = _row()
    row["data_json"] = "{not-json"

    receipt = compact_message(row)

    assert receipt["protocol"] == "legacy"
    assert receipt["data_json_valid"] is False
    assert receipt["raw_data_bytes"] == len("{not-json".encode("utf-8"))
    assert len(receipt["data_sha256"]) == 64
    _assert_unicode_byte_accounting_is_exact()


def _assert_unicode_byte_accounting_is_exact() -> None:
    body = "é🙂"
    data = {"kind": "coordination", "note": "雪"}
    row = _row(body=body, data=data)

    receipt = compact_message(row)

    expected_body = len(body.encode("utf-8"))
    expected_data = len(str(row["data_json"]).encode("utf-8"))
    assert receipt["raw_body_bytes"] == expected_body
    assert receipt["raw_data_bytes"] == expected_data
    assert receipt["omitted_raw_bytes"] == expected_body + expected_data


def _assert_all_emitted_prose_labels_obey_utf8_byte_bounds() -> None:
    receipt = compact_message(
        _row(
            body="雪" * 1_000,
            data={"kind": "分類" * 1_000},
        )
    )

    assert len(receipt["summary"].encode("utf-8")) <= MAX_COMPACT_SUMMARY_BYTES
    assert len(receipt["data_kind"].encode("utf-8")) <= MAX_DATA_KIND_BYTES
    assert receipt["summary"].endswith("…")
    assert receipt["data_kind"].endswith("…")


def _assert_oversized_row_metadata_fails_closed() -> None:
    row = _row()
    row["sender"] = "é" * MAX_METADATA_BYTES

    with pytest.raises(CompactionError, match="sender"):
        compact_message(row)


def test_thread_grouping_is_order_independent_and_preserves_provenance() -> None:
    rows = [
        _row(
            "m2",
            data=_v2(summary="second", status="resolved"),
            created_at="2026-08-30T12:02:00+00:00",
        ),
        _row(
            "m1",
            data=_v2(summary="first", status="open"),
            created_at="2026-08-30T12:01:00+00:00",
        ),
        _row("legacy", body="standalone"),
    ]

    forward = compact_threads(rows)
    reverse = compact_threads(reversed(rows))

    assert forward == reverse
    assert forward["thread_count"] == 2
    assert forward["message_count"] == 3
    thread = next(t for t in forward["threads"] if t["thread_id"] == "thread-1")
    assert thread["message_ids"] == ["m1", "m2"]
    assert thread["actionable_message_ids"] == ["m1"]
    assert len(thread["provenance"]) == 2
    assert len(thread["thread_sha256"]) == 64
    _assert_thread_detail_bound_never_omits_an_actionable_receipt()
    _assert_supersession_edges_are_preserved_without_inferring_resolution()


def _assert_thread_detail_bound_never_omits_an_actionable_receipt() -> None:
    rows = [
        _row(
            f"m{index}",
            data=_v2(
                summary=f"message {index}",
                status="open" if index < 2 else "resolved",
            ),
            created_at=f"2026-08-30T12:0{index}:00+00:00",
        )
        for index in range(4)
    ]

    digest = compact_threads(rows, max_messages_per_thread=2)
    thread = digest["threads"][0]

    assert [message["message_id"] for message in thread["messages"]] == ["m0", "m1"]
    assert thread["message_ids"] == ["m0", "m1", "m2", "m3"]
    assert thread["actionable_count"] == 2
    assert thread["omitted_message_details"] == 2
    assert len(thread["provenance"]) == 4


def test_thread_detail_bound_fails_closed_when_actionable_messages_do_not_fit() -> None:
    rows = [
        _row(f"m{index}", data=_v2(summary=str(index), status="blocked"))
        for index in range(3)
    ]

    with pytest.raises(CompactionError, match="actionable messages"):
        compact_threads(rows, max_messages_per_thread=2)


def _assert_supersession_edges_are_preserved_without_inferring_resolution() -> None:
    rows = [
        _row("old", data=_v2(summary="old", status="open")),
        _row(
            "new",
            data=_v2(
                summary="replacement",
                status="informational",
                supersedes=["old", "external"],
            ),
            created_at="2026-08-30T12:01:00+00:00",
        ),
    ]

    thread = compact_threads(rows)["threads"][0]

    assert thread["actionable_message_ids"] == ["old"]
    assert thread["supersession_edges"] == [
        {"message_id": "new", "supersedes": "old"},
        {"message_id": "new", "supersedes": "external"},
    ]


def _assert_duplicate_rows_are_deduplicated_but_conflicts_fail_closed() -> None:
    row = _row()
    assert compact_threads([row, dict(row)])["message_count"] == 1

    conflict = dict(row)
    conflict["body"] = "different"
    with pytest.raises(CompactionError, match="conflicting duplicate"):
        compact_threads([row, conflict])


def test_global_and_requested_bounds_fail_closed() -> None:
    with pytest.raises(CompactionError, match="max_threads"):
        compact_threads([_row("m1")], max_threads=0)
    with pytest.raises(CompactionError, match="max_messages_per_thread"):
        compact_threads([_row("m1")], max_messages_per_thread=True)
    with pytest.raises(CompactionError, match="exceeding max_threads"):
        compact_threads([_row("m1"), _row("m2")], max_threads=1)
    with pytest.raises(CompactionError, match="at most"):
        compact_threads(_row(f"m-{index}") for index in range(MAX_INPUT_MESSAGES + 1))
    _assert_oversized_row_metadata_fails_closed()
    _assert_duplicate_rows_are_deduplicated_but_conflicts_fail_closed()
