#!/usr/bin/env python3
"""Repo guard: keep data files out of the top level of the notes root.

The notes root is the knowledge tree -- the host's ``[tool.conductor]``
``notes_root``, the monorepo default being ``research/notes/`` -- where the KB
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

from conductor.project_paths import DEFAULT_NOTES_ROOT, host_root, notes_relative

FORBIDDEN_SUFFIXES = (".json", ".jsonl", ".csv")


def is_forbidden(path: str, notes_root: PurePosixPath = DEFAULT_NOTES_ROOT) -> bool:
    """True when ``path`` is a data file directly inside the notes root."""
    candidate = PurePosixPath(path)
    if candidate.parent != notes_root:
        return False
    return candidate.suffix in FORBIDDEN_SUFFIXES


def main() -> int:
    configured = notes_relative(host_root())
    bad = [f for f in sys.argv[1:] if is_forbidden(f, configured)]
    if not bad:
        return 0
    for path in bad:
        sys.stderr.write(
            f"data file at the top level of {configured}/: {path} — "
            "research/data/ for persistent inputs, research/reports/ or "
            "tasks/audit/ for tool output\n"
        )
    return 1


if __name__ == "__main__":
    sys.exit(main())
