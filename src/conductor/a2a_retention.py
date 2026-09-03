"""Manual, fail-closed retirement of explicitly resolved A2A protocol messages.

This module is intentionally not scheduled or called by ``agent_a2a serve``.
Preview is the default. Applying tombstones requires ``--apply`` plus an exact
``--store`` allowlist. Legacy, unread, unresolved, held, outbound, and pinned
messages are never eligible.
"""

from __future__ import annotations

import argparse
import json
import math
import sqlite3
import sys
from dataclasses import asdict, dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any, Final, cast

from conductor._native import (
    a2a_retention_evidence_native,
    a2a_retention_manifests_native,
)
from conductor.agent_a2a import DEFAULT_STATE_DIR, A2aError

DEFAULT_GRACE: Final = timedelta(hours=48)
MIN_GRACE: Final = timedelta(hours=1)
DEFAULT_BATCH: Final = 100
MAX_BATCH: Final = 1_000
MAX_EVIDENCE_FILES: Final = 512
MAX_EVIDENCE_FILE_BYTES: Final = 2 << 20
MAX_EVIDENCE_TOTAL_BYTES: Final = 32 << 20
MAX_EVIDENCE_NODES: Final = 500_000
POLICY_VERSION: Final = 2
TOMBSTONE_BODY: Final = "[compacted: resolved A2A content retained by digest]"
DEFAULT_EVIDENCE_ROOT: Final = Path(__file__).resolve().parents[1]
EVIDENCE_PATTERNS: Final = (
    "research/reports/**/*gate*.json",
    "research/reports/**/*receipt*.json",
)


@dataclass(frozen=True)
class RetentionResult:
    store: str
    mode: str
    eligible: int
    compacted: int
    original_content_bytes: int
    tombstone_bytes: int
    logical_bytes_removed: int
    evidence_files: int
    evidence_protected: int
    evidence_snapshot_sha256: str
    manifest_sha256: list[str]
    event_sha256: list[str]


@dataclass(frozen=True)
class _EvidenceSnapshot:
    paths: tuple[str, ...]
    protected_message_ids: frozenset[str]
    sha256: str


@dataclass(frozen=True)
class _RetentionBatch:
    candidate_ids: frozenset[str]
    evidence: _EvidenceSnapshot
    rows: tuple[sqlite3.Row, ...]
    manifest_json: tuple[str, ...]
    manifest_sha256: tuple[str, ...]
    event_sha256: tuple[str, ...]
    original_content_bytes: int
    tombstone_bytes: int
    logical_bytes_removed: int


def _evidence_snapshot(
    evidence_root: Path, *, candidate_ids: frozenset[str]
) -> _EvidenceSnapshot:
    """Read a bounded evidence set and find exact candidate-ID references."""

    if not evidence_root.is_dir():
        raise A2aError(f"evidence root is not a directory: {evidence_root}")
    resolved_root = evidence_root.resolve()
    matches = sorted(
        {path for pattern in EVIDENCE_PATTERNS for path in evidence_root.glob(pattern)},
        key=lambda path: path.as_posix(),
    )
    if len(matches) > MAX_EVIDENCE_FILES:
        raise A2aError(
            f"evidence scan found {len(matches)} files; maximum is {MAX_EVIDENCE_FILES}"
        )

    files: list[tuple[str, bytes]] = []
    total_bytes = 0
    for path in matches:
        try:
            resolved = path.resolve(strict=True)
            relative = resolved.relative_to(resolved_root).as_posix()
            if not resolved.is_file():
                raise A2aError(f"evidence path is not a regular file: {path}")
            size = resolved.stat().st_size
            if size > MAX_EVIDENCE_FILE_BYTES:
                raise A2aError(
                    f"evidence file exceeds {MAX_EVIDENCE_FILE_BYTES} bytes: {path}"
                )
            raw = resolved.read_bytes()
        except (OSError, ValueError) as exc:
            raise A2aError(f"cannot safely read evidence file {path}: {exc}") from exc
        if len(raw) != size or len(raw) > MAX_EVIDENCE_FILE_BYTES:
            raise A2aError(f"evidence file changed while being read: {path}")
        total_bytes += len(raw)
        if total_bytes > MAX_EVIDENCE_TOTAL_BYTES:
            raise A2aError(
                f"evidence scan exceeds {MAX_EVIDENCE_TOTAL_BYTES} total bytes"
            )
        files.append((relative, raw))
    try:
        paths, protected_message_ids, snapshot_sha256 = a2a_retention_evidence_native(
            sorted(candidate_ids), files
        )
    except ValueError as exc:
        raise A2aError(str(exc)) from exc
    return _EvidenceSnapshot(
        paths=tuple(paths),
        protected_message_ids=frozenset(protected_message_ids),
        sha256=str(snapshot_sha256),
    )


def _aware_utc(value: datetime, *, field: str) -> datetime:
    if value.tzinfo is None or value.utcoffset() is None:
        raise A2aError(f"{field} must be timezone-aware")
    return value.astimezone(UTC)


def _parse_timestamp(value: str, *, field: str) -> datetime:
    try:
        parsed = datetime.fromisoformat(value)
    except (TypeError, ValueError) as exc:
        raise A2aError(f"{field} is not a valid ISO-8601 timestamp: {value!r}") from exc
    return _aware_utc(parsed, field=field)


def _validate_grace(grace: timedelta) -> None:
    seconds = grace.total_seconds()
    if not math.isfinite(seconds) or grace < MIN_GRACE:
        raise A2aError(
            f"retention grace must be finite and at least {MIN_GRACE.total_seconds() / 3600:g} hour"
        )


def _validate_schema(connection: sqlite3.Connection) -> None:
    required = {
        "messages": {
            "direction",
            "message_id",
            "sender",
            "recipient",
            "body",
            "data_json",
            "created_at",
            "received_at",
            "delivery_status",
            "status_reason",
            "read_at",
        },
        "message_state": {
            "direction",
            "message_id",
            "thread_id",
            "summary",
            "protocol_status",
            "requires_response",
            "retention_class",
            "resolved_at",
            "superseded_at",
            "hold_reason",
            "tombstoned_at",
            "body_sha256",
            "body_bytes",
            "data_sha256",
            "data_bytes",
        },
        "retention_events": {
            "event_id",
            "direction",
            "message_id",
            "policy_version",
            "manifest_json",
            "manifest_sha256",
            "compacted_at",
        },
    }
    for table, expected in required.items():
        columns = {
            str(row["name"])
            for row in connection.execute(
                "SELECT name FROM pragma_table_info(?)", (table,)
            ).fetchall()
        }
        missing = expected - columns
        if missing:
            raise A2aError(
                f"A2A store schema is missing {table} columns: {sorted(missing)}"
            )


def _candidate_rows(
    connection: sqlite3.Connection,
    *,
    cutoff: str,
    limit: int,
) -> list[sqlite3.Row]:
    return list(
        connection.execute(
            """
            SELECT
                m.message_id, m.direction, m.sender, m.recipient, m.body,
                m.data_json, m.created_at, m.received_at, m.delivery_status,
                m.status_reason, m.read_at,
                s.thread_id, s.summary, s.protocol_status,
                s.requires_response, s.retention_class,
                s.resolved_at, s.superseded_at, s.hold_reason,
                s.tombstoned_at, s.body_sha256, s.body_bytes,
                s.data_sha256, s.data_bytes
            FROM messages AS m
            JOIN message_state AS s
              ON s.direction=m.direction AND s.message_id=m.message_id
            WHERE m.direction='inbound'
              AND m.read_at IS NOT NULL
              AND s.retention_class='operational'
              AND s.protocol_status IN ('resolved', 'superseded')
              AND s.requires_response=0
              AND s.hold_reason IS NULL
              AND s.tombstoned_at IS NULL
              AND (s.resolved_at IS NOT NULL OR s.superseded_at IS NOT NULL)
              AND (s.resolved_at IS NULL OR s.resolved_at <= ?)
              AND (s.superseded_at IS NULL OR s.superseded_at <= ?)
              AND NOT EXISTS (
                  SELECT 1 FROM retention_events AS e
                  WHERE e.direction=m.direction AND e.message_id=m.message_id
              )
            ORDER BY
                CASE
                    WHEN s.resolved_at IS NULL THEN s.superseded_at
                    WHEN s.superseded_at IS NULL THEN s.resolved_at
                    WHEN s.resolved_at >= s.superseded_at THEN s.resolved_at
                    ELSE s.superseded_at
                END,
                m.message_id
            LIMIT ?
            """,
            (cutoff, cutoff, limit),
        ).fetchall()
    )


_RETENTION_ROW_FIELDS: Final[tuple[str, ...]] = (
    "message_id",
    "direction",
    "sender",
    "recipient",
    "body",
    "data_json",
    "created_at",
    "received_at",
    "read_at",
    "resolved_at",
    "superseded_at",
    "thread_id",
    "summary",
    "protocol_status",
    "requires_response",
    "retention_class",
    "body_sha256",
    "body_bytes",
    "data_sha256",
    "data_bytes",
)
_NativeManifest = tuple[str, str, str, str, int, int]
_NativeManifestBatch = tuple[list[_NativeManifest], int, int, int]


def _retention_row_payload(row: sqlite3.Row) -> dict[str, Any]:
    payload = {field: row[field] for field in _RETENTION_ROW_FIELDS}
    payload["body"] = str(payload["body"])
    if payload["data_json"] is not None:
        payload["data_json"] = str(payload["data_json"])
    payload["requires_response"] = bool(payload["requires_response"])
    return payload


def _native_manifests(
    rows: tuple[sqlite3.Row, ...], *, compacted_at: str
) -> _NativeManifestBatch:
    try:
        return cast(
            _NativeManifestBatch,
            a2a_retention_manifests_native(
                [_retention_row_payload(row) for row in rows], compacted_at
            ),
        )
    except ValueError as exc:
        raise A2aError(str(exc)) from exc


def _validate_candidate_timestamps(
    row: sqlite3.Row, *, now_utc: datetime, grace: timedelta
) -> None:
    lifecycle_values = [
        _parse_timestamp(value, field="lifecycle timestamp")
        for value in (row["resolved_at"], row["superseded_at"])
        if value is not None
    ]
    if not lifecycle_values:
        raise A2aError("retention candidate has no terminal lifecycle timestamp")
    lifecycle = max(lifecycle_values)
    read_at = _parse_timestamp(row["read_at"], field="read_at")
    if lifecycle > now_utc or read_at > now_utc:
        raise A2aError("future A2A timestamps are not retention-eligible")
    if lifecycle > now_utc - grace:
        raise A2aError("retention candidate is newer than the grace cutoff")


def _prepare_batch(
    connection: sqlite3.Connection,
    *,
    evidence_root: Path,
    cutoff: str,
    limit: int,
    now_utc: datetime,
    grace: timedelta,
    compacted_at: str,
) -> _RetentionBatch:
    candidate_rows = _candidate_rows(connection, cutoff=cutoff, limit=MAX_BATCH)
    candidate_ids = frozenset(str(row["message_id"]) for row in candidate_rows)
    evidence = _evidence_snapshot(evidence_root, candidate_ids=candidate_ids)
    rows = tuple(
        row
        for row in candidate_rows
        if row["message_id"] not in evidence.protected_message_ids
    )[:limit]
    for row in rows:
        _validate_candidate_timestamps(row, now_utc=now_utc, grace=grace)
    items, original_bytes, tombstone_bytes, logical_bytes_removed = _native_manifests(
        rows, compacted_at=compacted_at
    )
    return _RetentionBatch(
        candidate_ids,
        evidence,
        rows,
        tuple(str(item[1]) for item in items),
        tuple(str(item[2]) for item in items),
        tuple(str(item[3]) for item in items),
        int(original_bytes),
        int(tombstone_bytes),
        int(logical_bytes_removed),
    )


def _insert_retention_event(
    connection: sqlite3.Connection,
    *,
    row: sqlite3.Row,
    manifest_json: str,
    manifest_sha256: str,
    event_id: str,
    compacted_at: str,
) -> None:
    connection.execute(
        """
        INSERT INTO retention_events (
            event_id, direction, message_id, policy_version,
            manifest_json, manifest_sha256, compacted_at
        ) VALUES (?, 'inbound', ?, ?, ?, ?, ?)
        """,
        (
            event_id,
            row["message_id"],
            POLICY_VERSION,
            manifest_json,
            manifest_sha256,
            compacted_at,
        ),
    )


def _tombstone_message(connection: sqlite3.Connection, row: sqlite3.Row) -> None:
    cursor = connection.execute(
        """
        UPDATE messages SET body=?, data_json=NULL
        WHERE direction='inbound' AND message_id=?
          AND body=?
          AND ((data_json IS NULL AND ? IS NULL) OR data_json=?)
          AND sender=? AND recipient=? AND created_at=?
          AND received_at IS ? AND delivery_status=?
          AND status_reason IS ?
          AND read_at=?
        """,
        (
            TOMBSTONE_BODY,
            row["message_id"],
            row["body"],
            row["data_json"],
            row["data_json"],
            row["sender"],
            row["recipient"],
            row["created_at"],
            row["received_at"],
            row["delivery_status"],
            row["status_reason"],
            row["read_at"],
        ),
    )
    if cursor.rowcount != 1:
        raise A2aError(f"message {row['message_id']!r} changed during retention")


def _tombstone_state(
    connection: sqlite3.Connection, row: sqlite3.Row, *, compacted_at: str
) -> None:
    cursor = connection.execute(
        """
        UPDATE message_state SET tombstoned_at=?
        WHERE direction='inbound' AND message_id=?
          AND thread_id=? AND summary=? AND protocol_status=?
          AND requires_response=? AND retention_class=?
          AND hold_reason IS ? AND tombstoned_at IS ?
          AND body_sha256=?
          AND body_bytes=? AND data_sha256 IS ? AND data_bytes=?
          AND resolved_at IS ? AND superseded_at IS ?
        """,
        (
            compacted_at,
            row["message_id"],
            row["thread_id"],
            row["summary"],
            row["protocol_status"],
            row["requires_response"],
            row["retention_class"],
            row["hold_reason"],
            row["tombstoned_at"],
            row["body_sha256"],
            row["body_bytes"],
            row["data_sha256"],
            row["data_bytes"],
            row["resolved_at"],
            row["superseded_at"],
        ),
    )
    if cursor.rowcount != 1:
        raise A2aError(
            f"message {row['message_id']!r} lifecycle changed during retention"
        )


def _apply_batch(
    connection: sqlite3.Connection, batch: _RetentionBatch, *, compacted_at: str
) -> int:
    for row, manifest_json, manifest_sha256, event_id in zip(
        batch.rows,
        batch.manifest_json,
        batch.manifest_sha256,
        batch.event_sha256,
        strict=True,
    ):
        _insert_retention_event(
            connection,
            row=row,
            manifest_json=manifest_json,
            manifest_sha256=manifest_sha256,
            event_id=event_id,
            compacted_at=compacted_at,
        )
        _tombstone_message(connection, row)
        _tombstone_state(connection, row, compacted_at=compacted_at)
    return len(batch.rows)


def _retention_result(
    store: Path,
    *,
    apply: bool,
    compacted: int,
    batch: _RetentionBatch,
) -> RetentionResult:
    return RetentionResult(
        store=str(store),
        mode="apply" if apply else "preview",
        eligible=len(batch.rows),
        compacted=compacted,
        original_content_bytes=batch.original_content_bytes,
        tombstone_bytes=batch.tombstone_bytes,
        logical_bytes_removed=batch.logical_bytes_removed,
        evidence_files=len(batch.evidence.paths),
        evidence_protected=len(batch.evidence.protected_message_ids),
        evidence_snapshot_sha256=batch.evidence.sha256,
        manifest_sha256=list(batch.manifest_sha256),
        event_sha256=list(batch.event_sha256),
    )


def compact_resolved_messages(
    store: Path,
    *,
    actor: str | None = None,
    evidence_root: Path = DEFAULT_EVIDENCE_ROOT,
    now: datetime | None = None,
    grace: timedelta = DEFAULT_GRACE,
    limit: int = DEFAULT_BATCH,
    apply: bool = False,
) -> RetentionResult:
    """Preview or atomically tombstone one bounded batch from one exact store."""

    _validate_grace(grace)
    if (
        isinstance(limit, bool)
        or not isinstance(limit, int)
        or not 1 <= limit <= MAX_BATCH
    ):
        raise A2aError(f"retention limit must be an integer in 1..{MAX_BATCH}")
    if not store.is_file():
        raise FileNotFoundError(store)
    if apply and (not actor or actor != store.parent.name):
        raise A2aError("retention apply actor must match the exact store name")
    now_utc = _aware_utc(now or datetime.now(UTC), field="now")
    try:
        cutoff = (now_utc - grace).isoformat(timespec="milliseconds")
    except OverflowError as exc:
        raise A2aError("retention grace exceeds the supported timestamp range") from exc
    compacted_at = now_utc.isoformat(timespec="milliseconds")
    connection = sqlite3.connect(store, timeout=5.0, isolation_level=None)
    connection.row_factory = sqlite3.Row
    try:
        connection.execute("PRAGMA busy_timeout=5000")
        connection.execute("PRAGMA foreign_keys=ON")
        _validate_schema(connection)
        connection.execute("BEGIN IMMEDIATE" if apply else "BEGIN")
        batch = _prepare_batch(
            connection,
            evidence_root=evidence_root,
            cutoff=cutoff,
            limit=limit,
            now_utc=now_utc,
            grace=grace,
            compacted_at=compacted_at,
        )
        if apply:
            compacted = _apply_batch(connection, batch, compacted_at=compacted_at)
            evidence_after = _evidence_snapshot(
                evidence_root, candidate_ids=batch.candidate_ids
            )
            if evidence_after != batch.evidence:
                raise A2aError("retention evidence changed during compaction")
            connection.commit()
        else:
            compacted = 0
            connection.rollback()
        return _retention_result(store, apply=apply, compacted=compacted, batch=batch)
    except Exception:
        if connection.in_transaction:
            connection.rollback()
        raise
    finally:
        connection.close()


def sweep(
    state_dir: Path,
    *,
    stores: list[str] | None = None,
    actor: str | None = None,
    evidence_root: Path = DEFAULT_EVIDENCE_ROOT,
    now: datetime | None = None,
    grace: timedelta = DEFAULT_GRACE,
    limit: int = DEFAULT_BATCH,
    apply: bool = False,
) -> list[RetentionResult]:
    """Preview all stores or apply only to an explicit store allowlist."""

    if apply:
        if stores is None or len(stores) != 1:
            raise A2aError("--apply requires exactly one explicit --store")
        if not actor or actor != stores[0]:
            raise A2aError("--apply requires --as-name matching the exact --store")
    names = stores or sorted(
        path.parent.name for path in state_dir.glob("*/store.sqlite") if path.is_file()
    )
    if len(names) != len(set(names)):
        raise A2aError("duplicate --store values are not allowed")
    results: list[RetentionResult] = []
    for name in names:
        if not name or Path(name).name != name:
            raise A2aError(f"invalid store name {name!r}")
        store_path = state_dir / name / "store.sqlite"
        if not store_path.is_file():
            raise A2aError(f"requested A2A store does not exist: {name!r}")
        try:
            store_path.resolve().relative_to(state_dir.resolve())
        except ValueError as exc:
            raise A2aError(
                f"requested A2A store escapes the state directory: {name!r}"
            ) from exc
        results.append(
            compact_resolved_messages(
                store_path,
                actor=actor,
                evidence_root=evidence_root,
                now=now,
                grace=grace,
                limit=limit,
                apply=apply,
            )
        )
    return results


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-dir", type=Path, default=DEFAULT_STATE_DIR)
    parser.add_argument("--evidence-root", type=Path, default=DEFAULT_EVIDENCE_ROOT)
    parser.add_argument(
        "--as-name",
        dest="actor",
        help="acting agent identity; must equal the sole --store with --apply",
    )
    parser.add_argument(
        "--store",
        action="append",
        dest="stores",
        help="exact agent store; repeatable and required with --apply",
    )
    parser.add_argument("--grace-hours", type=float, default=48.0)
    parser.add_argument("--limit", type=int, default=DEFAULT_BATCH)
    parser.add_argument(
        "--apply",
        action="store_true",
        help="apply logical tombstones; preview is the default",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        if not math.isfinite(args.grace_hours):
            raise A2aError("--grace-hours must be finite")
        results = sweep(
            args.state_dir,
            stores=args.stores,
            actor=args.actor,
            evidence_root=args.evidence_root,
            grace=timedelta(hours=args.grace_hours),
            limit=args.limit,
            apply=args.apply,
        )
        print(
            json.dumps(
                {
                    "schema_version": 1,
                    "authority": "deterministic-a2a-retention",
                    "automatic": False,
                    "mode": "apply" if args.apply else "preview",
                    "results": [asdict(result) for result in results],
                },
                ensure_ascii=False,
                indent=2,
                sort_keys=True,
            )
        )
        return 0
    except (A2aError, FileNotFoundError, OSError, sqlite3.Error, ValueError) as exc:
        print(f"a2a-retention: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
