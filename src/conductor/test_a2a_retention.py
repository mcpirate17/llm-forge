from __future__ import annotations

import hashlib
import json
import os
import sqlite3
import subprocess
import sys
from collections.abc import Callable
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import cast

import pytest

from conductor import a2a_retention as retention
from conductor.agent_a2a import A2aError

NOW = datetime(2026, 8, 30, 12, 0, tzinfo=timezone.utc)
OLD = "2026-08-30T09:00:00.000+00:00"
CUTOFF = "2026-08-30T11:00:00.000+00:00"
NEW = "2026-08-30T11:00:00.001+00:00"
GRACE = timedelta(hours=1)


def _sha256(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _canonical_json(value: object) -> str:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    )


def _make_store(state_dir: Path, name: str = "worker") -> Path:
    store = state_dir / name / "store.sqlite"
    store.parent.mkdir(parents=True)
    with sqlite3.connect(store) as connection:
        connection.executescript(
            """
            CREATE TABLE messages (
                message_id TEXT NOT NULL,
                direction TEXT NOT NULL,
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
            CREATE TABLE message_state (
                direction TEXT NOT NULL,
                message_id TEXT NOT NULL,
                thread_id TEXT NOT NULL,
                summary TEXT NOT NULL,
                protocol_status TEXT NOT NULL,
                requires_response INTEGER NOT NULL,
                retention_class TEXT NOT NULL,
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
            );
            CREATE TABLE retention_events (
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
            );
            """
        )
    return store


def _insert_message(
    store: Path,
    message_id: str,
    *,
    body: str = "completed work",
    data: object | None = None,
    direction: str = "inbound",
    read_at: str | None = OLD,
    resolved_at: str | None = OLD,
    superseded_at: str | None = None,
    retention_class: str = "operational",
    hold_reason: str | None = None,
    tombstoned_at: str | None = None,
    status: str | None = None,
    requires_response: bool = False,
) -> None:
    if data is None:
        data = {
            "kind": "coordination-v2",
            "thread_id": "thread-1",
            "summary": f"summary {message_id}",
            "status": status or ("resolved" if resolved_at else "superseded"),
            "requires_response": requires_response,
            "supersedes": [],
        }
    data_json = _canonical_json(data)
    body_bytes = body.encode("utf-8")
    data_bytes = data_json.encode("utf-8")
    selected_status = status or (
        "superseded" if superseded_at is not None else "resolved"
    )
    with sqlite3.connect(store) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            """
            INSERT INTO messages (
                message_id, direction, sender, recipient, body, data_json,
                created_at, received_at, delivery_status, status_reason, read_at
            ) VALUES (?, ?, 'Luna', 'Codex Efficiency', ?, ?, ?, ?,
                      'delivered', NULL, ?)
            """,
            (message_id, direction, body, data_json, OLD, OLD, read_at),
        )
        connection.execute(
            """
            INSERT INTO message_state (
                direction, message_id, thread_id, summary, protocol_status,
                requires_response, retention_class, resolved_at,
                superseded_at, hold_reason, tombstoned_at, body_sha256,
                body_bytes, data_sha256, data_bytes
            ) VALUES (?, ?, 'thread-1', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            """,
            (
                direction,
                message_id,
                f"summary {message_id}",
                selected_status,
                int(requires_response),
                retention_class,
                resolved_at,
                superseded_at,
                hold_reason,
                tombstoned_at,
                _sha256(body_bytes),
                len(body_bytes),
                _sha256(data_bytes),
                len(data_bytes),
            ),
        )


def _rows(store: Path, table: str) -> list[tuple[object, ...]]:
    statements = {
        "messages": "SELECT * FROM messages ORDER BY 1, 2",
        "message_state": "SELECT * FROM message_state ORDER BY 1, 2",
        "retention_events": "SELECT * FROM retention_events ORDER BY 1, 2",
    }
    with sqlite3.connect(store) as connection:
        return list(connection.execute(statements[table]))


def _snapshot(store: Path) -> dict[str, list[tuple[object, ...]]]:
    return {
        table: _rows(store, table)
        for table in ("messages", "message_state", "retention_events")
    }


def _message(store: Path, message_id: str) -> sqlite3.Row:
    connection = sqlite3.connect(store)
    connection.row_factory = sqlite3.Row
    try:
        row = connection.execute(
            """
            SELECT m.*, s.tombstoned_at, s.hold_reason, s.retention_class
            FROM messages AS m JOIN message_state AS s
              ON s.direction=m.direction AND s.message_id=m.message_id
            WHERE m.message_id=?
            """,
            (message_id,),
        ).fetchone()
        assert row is not None
        return row
    finally:
        connection.close()


def _compact(
    store: Path,
    *,
    now: datetime | None = NOW,
    grace: timedelta = GRACE,
    limit: int = retention.DEFAULT_BATCH,
    apply: bool = False,
    actor: str | None = None,
) -> retention.RetentionResult:
    return retention.compact_resolved_messages(
        store,
        actor=actor if actor is not None else store.parent.name if apply else None,
        evidence_root=store.parents[1],
        now=now,
        grace=grace,
        limit=limit,
        apply=apply,
    )


def test_preview_reports_exact_receipts_without_any_database_mutation(
    tmp_path: Path,
) -> None:
    store = _make_store(tmp_path)
    _insert_message(store, "eligible", body="résolu 🧪")
    with sqlite3.connect(store) as connection:
        assert connection.execute("PRAGMA journal_mode=DELETE").fetchone() == (
            "delete",
        )
    before_rows = _snapshot(store)
    before_bytes = store.read_bytes()

    result = _compact(store)

    assert result.mode == "preview"
    assert result.eligible == 1
    assert result.compacted == 0
    assert len(result.manifest_sha256) == 1
    assert len(result.event_sha256) == 1
    assert _snapshot(store) == before_rows
    assert store.read_bytes() == before_bytes
    with sqlite3.connect(store) as connection:
        assert connection.execute("PRAGMA journal_mode").fetchone() == ("delete",)
    assert not store.with_name("store.sqlite-journal").exists()
    assert not store.with_name("store.sqlite-wal").exists()


def test_apply_requires_allowlist_and_only_mutates_named_store(tmp_path: Path) -> None:
    alpha = _make_store(tmp_path, "alpha")
    beta = _make_store(tmp_path, "beta")
    _insert_message(alpha, "alpha-old")
    _insert_message(beta, "beta-old")

    with pytest.raises(A2aError, match="exactly one explicit --store"):
        retention.sweep(
            tmp_path,
            actor="alpha",
            evidence_root=tmp_path,
            now=NOW,
            grace=GRACE,
            apply=True,
        )
    assert _message(alpha, "alpha-old")["tombstoned_at"] is None
    assert _message(beta, "beta-old")["tombstoned_at"] is None

    with pytest.raises(A2aError, match="--as-name matching"):
        retention.sweep(
            tmp_path,
            stores=["alpha"],
            evidence_root=tmp_path,
            now=NOW,
            grace=GRACE,
            apply=True,
        )
    with pytest.raises(A2aError, match="actor must match"):
        _compact(alpha, actor="beta", apply=True)

    results = retention.sweep(
        tmp_path,
        stores=["alpha"],
        actor="alpha",
        evidence_root=tmp_path,
        now=NOW,
        grace=GRACE,
        apply=True,
    )

    assert [(item.store, item.compacted) for item in results] == [(str(alpha), 1)]
    assert _message(alpha, "alpha-old")["body"] == retention.TOMBSTONE_BODY
    assert _message(beta, "beta-old")["body"] == "completed work"
    _assert_apply_rejects_ambiguous_store_allowlists(tmp_path / "ambiguous")


def _assert_apply_rejects_ambiguous_store_allowlists(tmp_path: Path) -> None:
    cases = [["alpha", "alpha"], ["alpha", "beta"], ["../alpha"], [""]]
    for index, stores in enumerate(cases):
        case_dir = tmp_path / str(index)
        alpha = _make_store(case_dir, "alpha")
        _insert_message(alpha, "kept")

        with pytest.raises(A2aError):
            retention.sweep(
                case_dir,
                stores=stores,
                actor=stores[0] if len(stores) == 1 else None,
                evidence_root=case_dir,
                now=NOW,
                apply=True,
            )

        assert _message(alpha, "kept")["body"] == "completed work"


def test_apply_rejects_allowlisted_store_symlink_escape(tmp_path: Path) -> None:
    state_dir = tmp_path / "state"
    outside = _make_store(tmp_path / "outside", "worker")
    _insert_message(outside, "kept")
    state_dir.mkdir()
    (state_dir / "escaped").symlink_to(outside.parent, target_is_directory=True)

    with pytest.raises(A2aError, match="escapes the state directory"):
        retention.sweep(
            state_dir,
            stores=["escaped"],
            actor="escaped",
            evidence_root=tmp_path,
            now=NOW,
            apply=True,
        )

    assert _message(outside, "kept")["body"] == "completed work"


def test_grace_is_inclusive_and_uses_latest_terminal_transition(
    tmp_path: Path,
) -> None:
    store = _make_store(tmp_path)
    _insert_message(store, "at-cutoff", resolved_at=CUTOFF)
    _insert_message(store, "too-new", resolved_at=NEW)
    _insert_message(store, "old-resolved-new-superseded", superseded_at=NEW)
    _insert_message(store, "superseded-old", resolved_at=None, superseded_at=OLD)

    result = _compact(store, apply=True)

    assert result.compacted == 2
    assert _message(store, "at-cutoff")["tombstoned_at"] is not None
    assert _message(store, "superseded-old")["tombstoned_at"] is not None
    assert _message(store, "too-new")["tombstoned_at"] is None
    assert _message(store, "old-resolved-new-superseded")["tombstoned_at"] is None
    _assert_batch_limit_is_bounded_and_deterministic(tmp_path / "batch")
    _assert_invalid_grace_fails_closed(tmp_path / "invalid-grace")
    _assert_naive_or_future_timestamps_fail_closed(tmp_path / "timestamps")


def _assert_batch_limit_is_bounded_and_deterministic(tmp_path: Path) -> None:
    store = _make_store(tmp_path)
    _insert_message(store, "charlie")
    _insert_message(store, "alpha")
    _insert_message(store, "bravo")

    result = _compact(store, limit=2, apply=True)

    assert result.compacted == 2
    assert _message(store, "alpha")["tombstoned_at"] is not None
    assert _message(store, "bravo")["tombstoned_at"] is not None
    assert _message(store, "charlie")["tombstoned_at"] is None


def _assert_invalid_batch_limit_fails_closed(tmp_path: Path) -> None:
    for index, limit in enumerate([False, 0, -1, retention.MAX_BATCH + 1, 1.5]):
        store = _make_store(tmp_path, f"case-{index}")
        _insert_message(store, "kept")

        with pytest.raises(A2aError, match="retention limit"):
            _compact(store, limit=cast(int, limit), apply=True)

        assert _message(store, "kept")["tombstoned_at"] is None


def _assert_invalid_grace_fails_closed(tmp_path: Path) -> None:
    cases = [timedelta(0), timedelta(minutes=59), timedelta(hours=-1), timedelta.max]
    for index, grace in enumerate(cases):
        store = _make_store(tmp_path, f"case-{index}")
        _insert_message(store, "kept")

        with pytest.raises(A2aError, match="retention grace"):
            _compact(store, grace=grace, apply=True)

        assert _message(store, "kept")["tombstoned_at"] is None


def _assert_naive_or_future_timestamps_fail_closed(tmp_path: Path) -> None:
    store = _make_store(tmp_path)
    _insert_message(store, "future-read", read_at="2026-08-30T13:00:00+00:00")

    with pytest.raises(A2aError, match="future A2A timestamps"):
        _compact(store, apply=True)
    with pytest.raises(A2aError, match="timezone-aware"):
        _compact(store, now=NOW.replace(tzinfo=None))

    assert _message(store, "future-read")["tombstoned_at"] is None


def test_only_read_terminal_unheld_operational_inbound_rows_compact(
    tmp_path: Path,
) -> None:
    store = _make_store(tmp_path)
    _insert_message(store, "resolved")
    _insert_message(store, "superseded", resolved_at=None, superseded_at=OLD)
    _insert_message(store, "unread", read_at=None)
    _insert_message(store, "unresolved", resolved_at=None, status="open")
    _insert_message(store, "blocked-with-stale-resolution", status="blocked")
    _insert_message(store, "response-required", requires_response=True)
    _insert_message(store, "pinned", retention_class="pinned")
    _insert_message(store, "held", hold_reason="preserve audit context")
    _insert_message(store, "outbound", direction="outbound")

    result = _compact(store, apply=True)

    assert result.eligible == result.compacted == 2
    assert {
        message_id
        for message_id in (
            "resolved",
            "superseded",
            "unread",
            "unresolved",
            "blocked-with-stale-resolution",
            "response-required",
            "pinned",
            "held",
            "outbound",
        )
        if _message(store, message_id)["tombstoned_at"] is not None
    } == {"resolved", "superseded"}
    _assert_invalid_batch_limit_fails_closed(tmp_path / "invalid-limit")


def test_gate_and_unstructured_evidence_remain_pinned(tmp_path: Path) -> None:
    store = _make_store(tmp_path)
    _insert_message(
        store,
        "gate",
        data={
            "kind": "gate-review",
            "gate": "promotion",
            "fingerprint": "f" * 64,
            "artifact_paths": ["receipts/gate.json"],
        },
        retention_class="pinned",
    )
    _insert_message(
        store,
        "legacy-evidence",
        data={"kind": "message", "receipt": "immutable evidence"},
        retention_class="pinned",
    )

    result = _compact(store, apply=True)

    assert result.eligible == result.compacted == 0
    assert _message(store, "gate")["data_json"] is not None
    assert _message(store, "legacy-evidence")["data_json"] is not None
    assert _rows(store, "retention_events") == []


def test_evidence_referenced_message_ids_are_mechanically_excluded(
    tmp_path: Path,
) -> None:
    store = _make_store(tmp_path)
    _insert_message(store, "evidence-bound")
    _insert_message(store, "ordinary")
    reports = tmp_path / "research" / "reports" / "nested"
    reports.mkdir(parents=True)
    (reports / "promotion_gate_receipt.json").write_text(
        json.dumps(
            {
                "schema": "arbitrary-evidence-shape",
                "deep": [{"references": ["evidence-bound"]}],
            }
        )
    )
    (reports / "not-evidence.json").write_text(json.dumps({"message_id": "ordinary"}))

    result = _compact(store, apply=True)

    assert result.compacted == 1
    assert result.evidence_files == 1
    assert result.evidence_protected == 1
    assert len(result.evidence_snapshot_sha256) == 64
    assert _message(store, "evidence-bound")["tombstoned_at"] is None
    assert _message(store, "ordinary")["tombstoned_at"] is not None


@pytest.mark.parametrize("failure", ["malformed", "oversized", "nonfile"])
def test_evidence_scan_fails_closed_on_unsafe_inputs(
    tmp_path: Path, failure: str
) -> None:
    store = _make_store(tmp_path)
    _insert_message(store, "kept")
    reports = tmp_path / "research" / "reports"
    reports.mkdir(parents=True)
    path = reports / "unsafe_receipt.json"
    if failure == "malformed":
        path.write_text("{not-json")
    elif failure == "oversized":
        path.write_text(
            json.dumps({"padding": "x" * retention.MAX_EVIDENCE_FILE_BYTES})
        )
    else:
        path.mkdir()
    before = _snapshot(store)

    with pytest.raises(A2aError, match="evidence"):
        _compact(store, apply=True)

    assert _snapshot(store) == before


def test_evidence_drift_aborts_and_rolls_back_the_batch(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    store = _make_store(tmp_path)
    _insert_message(store, "kept")
    reports = tmp_path / "research" / "reports"
    reports.mkdir(parents=True)
    before = _snapshot(store)
    original = retention._evidence_snapshot
    calls = 0

    def drifting_evidence(
        evidence_root: Path, *, candidate_ids: frozenset[str]
    ) -> retention._EvidenceSnapshot:
        nonlocal calls
        snapshot = original(evidence_root, candidate_ids=candidate_ids)
        calls += 1
        if calls == 1:
            (reports / "late_gate_receipt.json").write_text(
                json.dumps({"message_id": "kept"})
            )
        return snapshot

    monkeypatch.setattr(retention, "_evidence_snapshot", drifting_evidence)

    with pytest.raises(A2aError, match="evidence changed"):
        _compact(store, apply=True)

    assert calls == 2
    assert _snapshot(store) == before


def test_manifest_hash_event_and_unicode_byte_accounting(tmp_path: Path) -> None:
    store = _make_store(tmp_path)
    body = "完了 ✅ é"
    data = {
        "kind": "coordination-v2",
        "thread_id": "unicode-thread",
        "summary": "résumé 完了",
        "status": "resolved",
        "requires_response": False,
        "supersedes": ["prior-message"],
        "private_unretained_field": "must not survive",
    }
    _insert_message(store, "unicode", body=body, data=data)
    data_json = _canonical_json(data)

    result = _compact(store, apply=True)

    with sqlite3.connect(store) as connection:
        connection.row_factory = sqlite3.Row
        event = connection.execute("SELECT * FROM retention_events").fetchone()
    assert event is not None
    manifest = json.loads(event["manifest_json"])
    manifest_hash = manifest.pop("manifest_sha256")
    assert manifest_hash == _sha256(_canonical_json(manifest).encode("utf-8"))
    assert event["manifest_sha256"] == manifest_hash
    expected_event = _sha256(
        f"{retention.POLICY_VERSION}\0inbound\0unicode\0{manifest_hash}".encode()
    )
    assert event["event_id"] == expected_event
    assert result.manifest_sha256 == [manifest_hash]
    assert result.event_sha256 == [expected_event]
    assert manifest["body_sha256"] == _sha256(body.encode("utf-8"))
    assert manifest["body_bytes"] == len(body.encode("utf-8"))
    assert manifest["data_sha256"] == _sha256(data_json.encode("utf-8"))
    assert manifest["data_bytes"] == len(data_json.encode("utf-8"))
    assert result.original_content_bytes == len(body.encode("utf-8")) + len(
        data_json.encode("utf-8")
    )
    assert "private_unretained_field" not in manifest["structured"]
    assert _message(store, "unicode")["data_json"] is None


def test_apply_is_idempotent_and_tombstone_text_is_not_an_eligibility_marker(
    tmp_path: Path,
) -> None:
    store = _make_store(tmp_path)
    _insert_message(store, "collision", body=retention.TOMBSTONE_BODY)

    first = _compact(store, apply=True)
    second = _compact(store, now=NOW + timedelta(hours=1), apply=True)

    assert first.compacted == 1
    assert second.eligible == second.compacted == 0
    assert len(_rows(store, "retention_events")) == 1
    assert _message(store, "collision")["tombstoned_at"] == NOW.isoformat(
        timespec="milliseconds"
    )


def _with_candidate_drift(
    monkeypatch: pytest.MonkeyPatch,
    mutation: Callable[[sqlite3.Connection], object],
) -> None:
    original = retention._candidate_rows

    def drifting_candidates(
        connection: sqlite3.Connection, *, cutoff: str, limit: int
    ) -> list[sqlite3.Row]:
        rows = original(connection, cutoff=cutoff, limit=limit)
        mutation(connection)
        return rows

    monkeypatch.setattr(retention, "_candidate_rows", drifting_candidates)


def test_content_drift_aborts_and_rolls_back_entire_batch(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    store = _make_store(tmp_path)
    _insert_message(store, "first")
    _insert_message(store, "second")
    before = _snapshot(store)
    _with_candidate_drift(
        monkeypatch,
        lambda connection: connection.execute(
            "UPDATE messages SET body='concurrent edit' WHERE message_id='second'"
        ),
    )

    with pytest.raises(A2aError, match="changed during retention"):
        _compact(store, apply=True)

    assert _snapshot(store) == before


@pytest.mark.parametrize(
    ("statement", "parameters"),
    [
        (
            "UPDATE message_state SET summary=? WHERE message_id='state-drift'",
            ("changed summary",),
        ),
        (
            "UPDATE message_state SET protocol_status=? WHERE message_id='state-drift'",
            ("blocked",),
        ),
        (
            "UPDATE message_state SET requires_response=? WHERE message_id='state-drift'",
            (1,),
        ),
        (
            "UPDATE message_state SET data_sha256=? WHERE message_id='state-drift'",
            ("0" * 64,),
        ),
        (
            "UPDATE message_state SET data_bytes=data_bytes + 1 "
            "WHERE message_id='state-drift'",
            (),
        ),
    ],
)
def test_state_drift_aborts_and_rolls_back_atomically(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    statement: str,
    parameters: tuple[object, ...],
) -> None:
    store = _make_store(tmp_path)
    _insert_message(store, "state-drift")
    before = _snapshot(store)
    _with_candidate_drift(
        monkeypatch,
        lambda connection: connection.execute(statement, parameters),
    )

    with pytest.raises(A2aError, match="lifecycle changed during retention"):
        _compact(store, apply=True)

    assert _snapshot(store) == before


@pytest.mark.parametrize(
    "drift_column", ["body_sha256", "body_bytes", "data_sha256", "data_bytes"]
)
def test_preexisting_content_metadata_drift_fails_before_writes(
    tmp_path: Path, drift_column: str
) -> None:
    store = _make_store(tmp_path)
    _insert_message(store, "bad-metadata")
    with sqlite3.connect(store) as connection:
        statements = {
            "body_sha256": "UPDATE message_state SET body_sha256=?",
            "body_bytes": "UPDATE message_state SET body_bytes=body_bytes + 1",
            "data_sha256": "UPDATE message_state SET data_sha256=?",
            "data_bytes": "UPDATE message_state SET data_bytes=data_bytes + 1",
        }
        if drift_column.endswith("sha256"):
            connection.execute(statements[drift_column], ("0" * 64,))
        else:
            connection.execute(statements[drift_column])
    before = _snapshot(store)

    with pytest.raises(A2aError, match="content drifted"):
        _compact(store, apply=True)

    assert _snapshot(store) == before


def test_cli_defaults_to_preview_and_apply_requires_store(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    store = _make_store(tmp_path, "cli-worker")
    _insert_message(store, "cli-message")

    assert (
        retention.main(
            [
                "--state-dir",
                str(tmp_path),
                "--evidence-root",
                str(tmp_path),
                "--grace-hours",
                "1",
            ]
        )
        == 0
    )
    preview = json.loads(capsys.readouterr().out)
    assert preview["automatic"] is False
    assert preview["mode"] == "preview"
    assert preview["results"][0]["eligible"] == 1
    assert _message(store, "cli-message")["tombstoned_at"] is None

    assert retention.main(["--state-dir", str(tmp_path), "--apply"]) == 2
    assert "explicit --store" in capsys.readouterr().err

    assert (
        retention.main(
            [
                "--state-dir",
                str(tmp_path),
                "--store",
                "cli-worker",
                "--as-name",
                "cli-worker",
                "--evidence-root",
                str(tmp_path),
                "--grace-hours",
                "1",
                "--apply",
            ]
        )
        == 0
    )
    applied = json.loads(capsys.readouterr().out)
    assert applied["mode"] == "apply"
    assert applied["results"][0]["compacted"] == 1
    _assert_cli_rejects_unsafe_grace(tmp_path / "unsafe-grace", capsys)


def _assert_cli_rejects_unsafe_grace(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    for index, grace in enumerate(["nan", "inf", "-inf", "-1"]):
        case_dir = tmp_path / str(index)
        _make_store(case_dir, "cli-worker")

        assert (
            retention.main(["--state-dir", str(case_dir), f"--grace-hours={grace}"])
            == 2
        )
        assert "grace" in capsys.readouterr().err


def test_import_has_no_store_or_scheduler_side_effect(tmp_path: Path) -> None:
    package_root = Path(retention.__file__).resolve().parents[1]
    state_dir = tmp_path / "must-not-exist"
    script = (
        "import sqlite3; "
        "sqlite3.connect=lambda *a, **k: (_ for _ in ()).throw("
        "AssertionError('import opened sqlite')); "
        "import conductor.a2a_retention"
    )
    environment = {
        **os.environ,
        "PYTHONPATH": str(package_root),
        "PYTHONDONTWRITEBYTECODE": "1",
        "A2A_STATE_DIR": str(state_dir),
    }

    completed = subprocess.run(
        [sys.executable, "-c", script],
        cwd=tmp_path,
        env=environment,
        check=False,
        capture_output=True,
        text=True,
    )

    assert completed.returncode == 0, completed.stderr
    assert not state_dir.exists()
    agent_source = (package_root / "conductor" / "agent_a2a.py").read_text()
    assert "a2a_retention" not in agent_source
    assert "compact_resolved_messages" not in agent_source
