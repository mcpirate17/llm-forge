"""Changed line ranges for a candidate file, for line-scoped analyzers.

Whole-file formatters and linters are the wrong shape for a gate that fires
ahead of a commit: the first person to touch a long-unformatted file inherits
every prior line's drift as a blocking finding, and the honest fix -- reformat
the tree once -- is a change nobody reviewed. clang-format takes `--lines` and
clang-tidy takes `-line-filter`, so both can be pointed at the lines the
candidate actually wrote.

The ranges come from `git diff` inside the review snapshot, whose working tree
holds the candidate and whose object store holds the base.
"""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

# Anchoring is `HUNK.match`'s job below; a leading `^` here would be a second
# copy of the same guard, and either one alone still keeps hunk-shaped source
# out of the ranges.
HUNK = re.compile(r"@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")


class DiffError(RuntimeError):
    """`git diff` could not be run against the review base."""


def parse_hunks(diff: str) -> list[tuple[int, int]]:
    """New-side inclusive line ranges from a `git diff --unified=0` body."""
    ranges: list[tuple[int, int]] = []
    for line in diff.splitlines():
        match = HUNK.match(line)
        if match is None:
            continue
        start = int(match.group(1))
        count = 1 if match.group(2) is None else int(match.group(2))
        if count == 0:
            # A pure deletion has no new-side line to analyze.
            continue
        ranges.append((start, start + count - 1))
    return ranges


def changed_line_ranges(path: str, *, base: str, cwd: Path) -> list[tuple[int, int]]:
    """Lines `path` adds or rewrites relative to `base`.

    An empty list means the file differs from the base in no line -- it was
    listed as changed for its mode or its name -- and the caller should skip it
    rather than analyze the whole file.
    """
    completed = subprocess.run(
        ["git", "diff", "--unified=0", "--no-color", base, "--", path],
        cwd=cwd,
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode:
        raise DiffError(
            f"git diff {base} -- {path} failed: {completed.stderr.strip() or 'no output'}"
        )
    return parse_hunks(completed.stdout)


def merge_ranges(ranges: list[tuple[int, int]]) -> list[tuple[int, int]]:
    """Coalesce touching or overlapping ranges so a tool sees each line once."""
    merged: list[tuple[int, int]] = []
    for start, end in sorted(ranges):
        if merged and start <= merged[-1][1] + 1:
            merged[-1] = (merged[-1][0], max(merged[-1][1], end))
        else:
            merged.append((start, end))
    return merged
