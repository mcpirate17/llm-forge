"""Shared CLI plumbing for attributing NEW analyzer findings to changed files.

``jscpd``, ``pmd-cpd`` and ``vulture`` scan the whole tree and diff against a
committed baseline, so a candidate that never touched the files behind a
pre-existing finding can still be blocked by it (2026-08-29 incident: a
one-module PR was blocked on a vulture finding and a jscpd pair in files it
never opened). ``--changed-file`` / ``--changed-files-from`` let a caller
name the candidate's changed files so the analyzer wrapper can split NEW
findings into CAUSED (names a changed file, blocks) and INHERITED
(pre-existing debt outside the candidate's diff, reported but not blocking).

Omitting both flags entirely keeps the legacy, whole-tree-blocks behavior
unchanged -- that is what a bare CLI invocation (``make dupes-jscpd-check``,
ad hoc CI) gets today and continues to get.
"""

from __future__ import annotations

import argparse
from pathlib import Path


def add_changed_files_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--changed-file",
        action="append",
        default=[],
        metavar="PATH",
        help=(
            "Repo-relative path the candidate changed. Repeatable. When at "
            "least one --changed-file/--changed-files-from is given, a NEW "
            "finding blocks only if it names a changed file (CAUSED); every "
            "other NEW finding is reported as pre-existing INHERITED debt "
            "and does not block. Omit both flags entirely to keep legacy "
            "behavior: every NEW finding blocks."
        ),
    )
    parser.add_argument(
        "--changed-files-from",
        type=Path,
        default=None,
        metavar="FILE",
        help="Read additional --changed-file paths from FILE, one per line.",
    )


def resolve_changed_files(args: argparse.Namespace) -> frozenset[str] | None:
    """Return the candidate's changed-file set, or ``None`` for legacy mode.

    ``None`` means neither flag was passed: callers must treat every NEW
    finding as blocking, exactly as before this attribution split existed.
    """
    if not args.changed_file and args.changed_files_from is None:
        return None
    values = list(args.changed_file)
    if args.changed_files_from is not None:
        values.extend(
            line.strip()
            for line in args.changed_files_from.read_text(encoding="utf-8").splitlines()
            if line.strip()
        )
    return frozenset(value for value in values if value)
