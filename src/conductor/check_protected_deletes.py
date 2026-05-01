#!/usr/bin/env python3
from __future__ import annotations

import fnmatch
import subprocess
import sys
from pathlib import PurePosixPath


PROTECTED_PATTERNS = (
    "*.db",
    "*.db-wal",
    "*.db-shm",
    "lab_notebook*",
    "*/lab_notebook*",
    "db_backups/*",
    "*/db_backups/*",
    "research/runtime_events/*.ndjson",
    "research/scientist/runtime_events/*.ndjson",
    "research/perf_artifacts/*",
    "*.parquet",
    "*.feather",
    "research/.continuous_paused",
    "research/runtime/champion_*.json",
    "research/runtime/*_status.json",
    "research/runtime/*_inventory.json",
)


def _staged_deletes() -> list[str]:
    proc = subprocess.run(
        ["git", "diff", "--cached", "--name-only", "--diff-filter=D", "-z"],
        capture_output=True,
        check=True,
    )
    return [p for p in proc.stdout.decode("utf-8", "replace").split("\0") if p]


def _is_protected(path: str) -> bool:
    posix = PurePosixPath(path).as_posix()
    return any(fnmatch.fnmatchcase(posix, pattern) for pattern in PROTECTED_PATTERNS)


def main() -> int:
    blocked = [path for path in _staged_deletes() if _is_protected(path)]
    if not blocked:
        return 0
    print("BLOCKED protected data deletion in staged changes:", file=sys.stderr)
    for path in blocked:
        print(f"  - {path}", file=sys.stderr)
    print(
        "Unstage the deletion. If this is an intentional data-retention change, "
        "handle it outside the normal commit path with an explicit backup/restore plan.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
