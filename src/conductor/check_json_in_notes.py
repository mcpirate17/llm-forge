#!/usr/bin/env python3
"""Repo guard: keep data files out of the top level of research/notes/.

research/notes/ is the knowledge tree -- CLAUDE.md names it as where the KB
cards and durable findings live, and the Laws table points at it by path. What
does not belong there is bulk data: persistent JSON/CSV inputs belong under
research/data/, and disposable tool output belongs under research/reports/ or
tasks/audit/.

Only the top level is guarded. Subdirectories are exempt by design -- they are
curated collections, not loose data drops. Contract recorded in
tasks/cleanup/cleanup_summary.md.
"""

from __future__ import annotations

import sys
from pathlib import PurePosixPath

NOTES_ROOT = PurePosixPath("research/notes")
FORBIDDEN_SUFFIXES = (".json", ".jsonl", ".csv")


def is_forbidden(path: str) -> bool:
    """True when ``path`` is a data file directly inside research/notes/."""
    candidate = PurePosixPath(path)
    if candidate.parent != NOTES_ROOT:
        return False
    return candidate.suffix in FORBIDDEN_SUFFIXES


def main() -> int:
    bad = [f for f in sys.argv[1:] if is_forbidden(f)]
    if not bad:
        return 0
    for path in bad:
        sys.stderr.write(
            f"data file at the top level of research/notes/: {path} — "
            "research/data/ for persistent inputs, research/reports/ or "
            "tasks/audit/ for tool output\n"
        )
    return 1


if __name__ == "__main__":
    sys.exit(main())
