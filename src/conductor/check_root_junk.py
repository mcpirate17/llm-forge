#!/usr/bin/env python3
"""Pre-commit hook: reject junk files at the repo root.

Forbidden patterns at the repo root (NOT in subdirs):
- *PLAN*.md, *HANDOFF*.md           — plan/handoff docs belong in tasks/
- *.log, metrics.jsonl              — ephemeral, never commit
- unused.*                          — placeholder cruft

User-blessed `*DO_NOT_DELETE*.txt` files at root are explicitly allowed
(personal command/key reference notes). See feedback_do_not_delete_marker
memory and tasks/cleanup/cleanup_summary.md.
"""

from __future__ import annotations

import fnmatch
import sys
from pathlib import PurePosixPath

FORBIDDEN_GLOBS = ("*PLAN*.md", "*HANDOFF*.md", "*.log", "metrics.jsonl", "unused.*")


def is_forbidden(path: str) -> bool:
    p = PurePosixPath(path)
    if p.parent != PurePosixPath("."):
        return False
    name = p.name
    if "DO_NOT_DELETE" in name:
        return False
    return any(fnmatch.fnmatchcase(name, g) for g in FORBIDDEN_GLOBS)


def main() -> int:
    bad = [f for f in sys.argv[1:] if is_forbidden(f)]
    if not bad:
        return 0
    for f in bad:
        sys.stderr.write(
            f"forbidden at repo root: {f} — move to tasks/ or research/reports/\n"
        )
    return 1


if __name__ == "__main__":
    sys.exit(main())
