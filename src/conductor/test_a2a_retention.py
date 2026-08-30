"""Tests for conductor.a2a_retention: read + grace-period tombstoning."""

from __future__ import annotations

import sqlite3
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from conductor.a2a_retention import TOMBSTONE_BODY, sweep, tombstone_expired_messages

SCHEMA = """
CREATE TABLE messages (
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
    PRIMARY KEY (direction, message_id)
);
"""

NOW = datetime(2026, 8, 30, 12, 0, 0, tzinfo=timezone.utc)


def _make_store(tmp_path: Path, name: str = "agent") -> Path:
    store_dir = tmp_path / name
    store_dir.mkdir()
    path = store_dir / "store.sqlite"
    conn = sqlite3.connect(path)
    conn.execute(SCHEMA)
    conn.commit()
    conn.close()
    return path


def _insert(
    store: Path,
    *,
    message_id: str,
    direction: str = "inbound",
    sender: str = "alice",
    recipient: str = "bob",
    body: str = "hello",
    read_at: datetime | None,
    created_at: datetime | None = None,
) -> None:
    conn = sqlite3.connect(store)
    conn.execute(
        """
        INSERT INTO messages
            (message_id, direction, sender, recipient, body, data_json,
             created_at, received_at, delivery_status, status_reason, read_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'delivered', NULL, ?)
        """,
        (
            message_id,
            direction,
            sender,
            recipient,
            body,
            '{"k": "v"}',
            (created_at or NOW).isoformat(),
            (created_at or NOW).isoformat(),
            read_at.isoformat() if read_at else None,
        ),
    )
    conn.commit()
    conn.close()


def _row(store: Path, message_id: str) -> sqlite3.Row:
    conn = sqlite3.connect(store)
    conn.row_factory = sqlite3.Row
    row = conn.execute(
        "SELECT * FROM messages WHERE message_id = ?", (message_id,)
    ).fetchone()
    conn.close()
    assert row is not None
    return row


def test_unread_message_is_never_touched_regardless_of_age(tmp_path: Path) -> None:
    store = _make_store(tmp_path)
    _insert(store, message_id="m1", read_at=None, created_at=NOW - timedelta(days=30))

    result = tombstone_expired_messages(store, now=NOW, grace=timedelta(hours=1))

    assert result.tombstoned == 0
    row = _row(store, "m1")
    assert row["body"] == "hello"
    assert row["data_json"] == '{"k": "v"}'


def test_read_message_within_grace_window_is_untouched(tmp_path: Path) -> None:
    store = _make_store(tmp_path)
    _insert(store, message_id="m1", read_at=NOW - timedelta(hours=1))

    result = tombstone_expired_messages(store, now=NOW, grace=timedelta(hours=48))

    assert result.tombstoned == 0
    row = _row(store, "m1")
    assert row["body"] == "hello"


def test_read_message_past_grace_window_is_tombstoned(tmp_path: Path) -> None:
    store = _make_store(tmp_path)
    read_at = NOW - timedelta(hours=49)
    _insert(store, message_id="m1", read_at=read_at)

    result = tombstone_expired_messages(store, now=NOW, grace=timedelta(hours=48))

    assert result.tombstoned == 1
    row = _row(store, "m1")
    assert row["body"] == TOMBSTONE_BODY
    assert row["data_json"] is None
    # identity/timing fields survive the tombstone
    assert row["message_id"] == "m1"
    assert row["sender"] == "alice"
    assert row["recipient"] == "bob"
    assert row["read_at"] == read_at.isoformat()


def test_dry_run_reports_but_does_not_modify(tmp_path: Path) -> None:
    store = _make_store(tmp_path)
    _insert(store, message_id="m1", read_at=NOW - timedelta(hours=49))

    result = tombstone_expired_messages(
        store, now=NOW, grace=timedelta(hours=48), dry_run=True
    )

    assert result.tombstoned == 1
    row = _row(store, "m1")
    assert row["body"] == "hello"  # untouched despite the reported count


def test_already_tombstoned_row_is_skipped_on_rerun(tmp_path: Path) -> None:
    store = _make_store(tmp_path)
    _insert(store, message_id="m1", read_at=NOW - timedelta(hours=49))

    first = tombstone_expired_messages(store, now=NOW, grace=timedelta(hours=48))
    second = tombstone_expired_messages(store, now=NOW, grace=timedelta(hours=48))

    assert first.tombstoned == 1
    assert second.tombstoned == 0
    assert second.scanned == 0


def test_mixed_batch_only_tombstones_the_eligible_row(tmp_path: Path) -> None:
    store = _make_store(tmp_path)
    _insert(store, message_id="unread", read_at=None)
    _insert(store, message_id="fresh-read", read_at=NOW - timedelta(hours=1))
    _insert(store, message_id="stale-read", read_at=NOW - timedelta(hours=100))

    result = tombstone_expired_messages(store, now=NOW, grace=timedelta(hours=48))

    assert result.tombstoned == 1
    assert result.scanned == 2  # only the two read rows are candidates
    assert _row(store, "unread")["body"] == "hello"
    assert _row(store, "fresh-read")["body"] == "hello"
    assert _row(store, "stale-read")["body"] == TOMBSTONE_BODY


def test_missing_store_raises(tmp_path: Path) -> None:
    with pytest.raises(FileNotFoundError):
        tombstone_expired_messages(tmp_path / "nope" / "store.sqlite", now=NOW)


def test_sweep_covers_multiple_stores_independently(tmp_path: Path) -> None:
    a = _make_store(tmp_path, "agent-a")
    b = _make_store(tmp_path, "agent-b")
    _insert(a, message_id="m1", read_at=NOW - timedelta(hours=100))
    _insert(b, message_id="m1", read_at=NOW - timedelta(hours=1))

    results = sweep(tmp_path, now=NOW, grace=timedelta(hours=48))

    by_name = {r.store.parent.name: r for r in results}
    assert by_name["agent-a"].tombstoned == 1
    assert by_name["agent-b"].tombstoned == 0


def test_sweep_only_filter_targets_a_single_store(tmp_path: Path) -> None:
    a = _make_store(tmp_path, "agent-a")
    b = _make_store(tmp_path, "agent-b")
    _insert(a, message_id="m1", read_at=NOW - timedelta(hours=100))
    _insert(b, message_id="m1", read_at=NOW - timedelta(hours=100))

    results = sweep(tmp_path, now=NOW, grace=timedelta(hours=48), only="agent-a")

    assert len(results) == 1
    assert results[0].store.parent.name == "agent-a"
