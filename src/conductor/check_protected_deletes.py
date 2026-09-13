#!/usr/bin/env python3
from __future__ import annotations

import argparse
import fnmatch
import subprocess
import sys
from pathlib import PurePosixPath
from typing import Sequence

from conductor.audit_root import (
    AuditRootError,
    print_audit_provenance,
    resolve_audit_root,
)
from conductor.project_paths import host_root

ROOT = host_root()
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


def _deleted_paths(from_ref: str | None = None) -> list[str]:
    args = ["git", "diff"]
    if from_ref is None:
        args.append("--cached")
    else:
        args.append(f"{from_ref}...HEAD")
    args.extend(["--name-only", "--diff-filter=D", "-z"])
    proc = subprocess.run(
        args,
        cwd=ROOT,
        capture_output=True,
        check=True,
    )
    return [p for p in proc.stdout.decode("utf-8", "replace").split("\0") if p]


def _is_protected(path: str) -> bool:
    posix = PurePosixPath(path).as_posix()
    return any(fnmatch.fnmatchcase(posix, pattern) for pattern in PROTECTED_PATTERNS)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Block protected data deletions")
    parser.add_argument(
        "--from-ref",
        help="Check deletions from the merge base with REF to HEAD",
    )
    parser.add_argument(
        "--root",
        help=(
            "Repository tree to check. Defaults to the Git worktree containing "
            "the current working directory, never the checkout that supplied "
            "the imported conductor module."
        ),
    )
    args = parser.parse_args(argv)

    global ROOT
    try:
        ROOT = resolve_audit_root(args.root)
    except AuditRootError as exc:
        print(f"ERROR: check-protected-deletes: {exc}", file=sys.stderr)
        return 2
    print_audit_provenance("check-protected-deletes", ROOT)

    blocked = [path for path in _deleted_paths(args.from_ref) if _is_protected(path)]
    if not blocked:
        return 0
    print("BLOCKED protected data deletion in candidate changes:", file=sys.stderr)
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
