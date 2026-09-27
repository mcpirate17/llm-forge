"""Manual A2A retention CLI compatibility for the native Forge implementation.

The evidence scan, eligibility policy, manifest, and SQLite transaction run in
``forge mailbox retention``. Preview is the default; apply needs one exact
store and a matching actor. This module never schedules retention.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import subprocess
import sys
from dataclasses import asdict, dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any, Final

from conductor.a2a_registry import DEFAULT_STATE_DIR, A2aError
from conductor.project_init import resolve_forge_binary
from conductor.project_paths import host_root

DEFAULT_GRACE: Final = timedelta(hours=48)
MIN_GRACE: Final = timedelta(hours=1)
DEFAULT_BATCH: Final = 100
MAX_BATCH: Final = 1_000
DEFAULT_EVIDENCE_ROOT: Final = host_root()


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


def _forge_binary() -> Path:
    explicit = os.environ.get("FORGE_BIN")
    if explicit:
        path = Path(explicit)
        if not path.is_file() or not os.access(path, os.X_OK):
            raise A2aError(f"FORGE_BIN is not an executable file: {path}")
        return path
    found = resolve_forge_binary(host_root())
    if found is None:
        raise A2aError("forge binary is required for A2A retention")
    return found


def _invoke(
    state_dir: Path,
    *,
    stores: list[str] | None,
    actor: str | None,
    evidence_root: Path,
    now: datetime | None,
    grace: timedelta,
    limit: int,
    apply: bool,
) -> list[RetentionResult]:
    if not math.isfinite(grace.total_seconds()) or grace < MIN_GRACE:
        raise A2aError("retention grace must be finite and at least 1 hour")
    if (
        isinstance(limit, bool)
        or not isinstance(limit, int)
        or not 1 <= limit <= MAX_BATCH
    ):
        raise A2aError(f"retention limit must be an integer in 1..{MAX_BATCH}")
    if now is not None and (now.tzinfo is None or now.utcoffset() is None):
        raise A2aError("now must be timezone-aware")
    command = [
        str(_forge_binary()),
        "mailbox",
        "--state-dir",
        str(state_dir),
        "retention",
        "--evidence-root",
        str(evidence_root),
        "--grace-hours",
        str(grace.total_seconds() / 3600),
        "--limit",
        str(limit),
    ]
    for store in stores or []:
        command.extend(("--store", store))
    if actor is not None:
        command.extend(("--as-name", actor))
    if now is not None:
        command.extend(("--now", now.astimezone(UTC).isoformat()))
    if apply:
        command.append("--apply")
    completed = subprocess.run(command, capture_output=True, text=True, check=False)
    if completed.returncode != 0:
        raise A2aError(completed.stderr.strip() or "native retention command failed")
    try:
        payload: dict[str, Any] = json.loads(completed.stdout)
        if payload["authority"] != "deterministic-a2a-retention":
            raise ValueError("wrong native retention authority")
        return [RetentionResult(**item) for item in payload["results"]]
    except (KeyError, TypeError, ValueError) as exc:
        raise A2aError(f"invalid native retention response: {exc}") from exc


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
    """Preview or atomically tombstone one exact store through native Forge."""
    results = _invoke(
        store.parent.parent,
        stores=[store.parent.name],
        actor=actor,
        evidence_root=evidence_root,
        now=now,
        grace=grace,
        limit=limit,
        apply=apply,
    )
    return results[0]


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
    """Preview all stores, or apply to one explicit allowlisted store."""
    return _invoke(
        state_dir,
        stores=stores,
        actor=actor,
        evidence_root=evidence_root,
        now=now,
        grace=grace,
        limit=limit,
        apply=apply,
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-dir", type=Path, default=DEFAULT_STATE_DIR)
    parser.add_argument("--evidence-root", type=Path, default=DEFAULT_EVIDENCE_ROOT)
    parser.add_argument("--as-name", dest="actor")
    parser.add_argument("--store", action="append", dest="stores")
    parser.add_argument("--grace-hours", type=float, default=48.0)
    parser.add_argument("--limit", type=int, default=DEFAULT_BATCH)
    parser.add_argument("--apply", action="store_true")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
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
    except (A2aError, OSError, OverflowError, ValueError) as exc:
        print(f"a2a-retention: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
