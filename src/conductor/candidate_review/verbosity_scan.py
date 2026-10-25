"""Report Python functions whose comment lines outnumber their code lines.

The detector is `slop_core.verbosity_scan_files`; this module is argv, changed-line
scoping and printing. Advisory: findings print as `path:line: rule: message` and the
exit status is 0 unless `--strict` is given.

A finding carries the function's span, and a function counts as changed when any
line of that span changed, because adding comments to the body leaves the `def` line
alone.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from conductor.candidate_review.diff_ranges import DiffError, changed_line_ranges
from conductor.candidate_review.style_scan import scope


def _touched(
    findings: list[dict], base: str, root: Path
) -> tuple[list[dict], list[str]]:
    """Findings whose function span overlaps a changed line."""
    kept, unreadable = scope(findings, base, root)
    if not base:
        return kept, unreadable
    seen = {(row["path"], row["line"]) for row in kept}
    ranges: dict[str, tuple[tuple[int, int], ...]] = {}
    for row in findings:
        if (row["path"], row["line"]) in seen:
            continue
        path = row["path"]
        if path not in ranges:
            try:
                ranges[path] = tuple(changed_line_ranges(path, base=base, cwd=root))
            except DiffError as error:
                unreadable.append(str(error))
                ranges[path] = ()
        if any(s <= row["end_line"] and e >= row["line"] for s, e in ranges[path]):
            kept.append(row)
    return kept, unreadable


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="")
    parser.add_argument("--strict", action="store_true")
    parser.add_argument("--version", action="store_true")
    parser.add_argument("files", nargs="*")
    args = parser.parse_args(sys.argv[1:] if argv is None else argv)
    if args.version:
        print("verbosity-scan 1")
        return 0

    paths = [f for f in args.files if f.endswith(".py")]
    if not paths:
        return 0

    import slop_core

    findings = list(slop_core.verbosity_scan_files(paths))
    kept, unreadable = _touched(findings, args.base, Path.cwd().resolve())
    for message in unreadable:
        print(f"verbosity-scan: {message}", file=sys.stderr)
    if unreadable:
        return 1
    for row in sorted(kept, key=lambda r: (r["path"], r["line"])):
        print(f"{row['path']}:{row['line']}: {row['rule']}: {row['message']}")
    print(f"verbosity-scan: {len(kept)} finding(s)")
    return 1 if kept and args.strict else 0


if __name__ == "__main__":
    raise SystemExit(main())
