"""Refuse tests that cannot fail: no assertion, or an assert that is a constant.

The detector is `slop_core.test_value_scan_files`, tree-sitter in Rust. This
module is argv, line scoping and printing.

A finding spans the whole test function, so a candidate that edits any line of
an assert-less test is held to it -- editing the body of a test is the moment to
make it check something. `style_scan.scope` keeps a finding only when its first
line changed, which would let a body edit slip past a finding that sits on the
`def`; the overlap test here is the same diff, asked about a span.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from conductor.candidate_review.diff_ranges import DiffError, changed_line_ranges

RULES = ("test/always-true-assert", "test/no-assertion")


def _overlaps(finding: dict, ranges: tuple[tuple[int, int], ...]) -> bool:
    return any(
        start <= finding["end_line"] and end >= finding["line"] for start, end in ranges
    )


def scope_spans(
    findings: list[dict], base: str, root: Path
) -> tuple[list[dict], list[str]]:
    """Keep the findings whose line span contains a line this candidate changed.

    An empty `base` is a full-tree run and keeps everything.
    """
    if not base:
        return findings, []
    kept: list[dict] = []
    unreadable: list[str] = []
    ranges: dict[str, tuple[tuple[int, int], ...]] = {}
    for finding in findings:
        path = finding["path"]
        if path not in ranges:
            try:
                ranges[path] = tuple(changed_line_ranges(path, base=base, cwd=root))
            except DiffError as error:
                unreadable.append(str(error))
                ranges[path] = ()
        if _overlaps(finding, ranges[path]):
            kept.append(finding)
    return kept, unreadable


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="")
    parser.add_argument("--version", action="store_true")
    parser.add_argument("files", nargs="*")
    args = parser.parse_args(sys.argv[1:] if argv is None else argv)
    if args.version:
        print("test-value-scan 1")
        return 0

    paths = [f for f in args.files if f.endswith(".py")]
    if not paths:
        return 0

    # Deferred so `--version` answers where the extension is not built.
    import slop_core

    findings = list(slop_core.test_value_scan_files(paths))
    kept, unreadable = scope_spans(findings, args.base, Path.cwd().resolve())
    for message in unreadable:
        print(f"test-value-scan: {message}", file=sys.stderr)
    if unreadable:
        return 1
    for finding in kept:
        print(
            f"{finding['path']}:{finding['line']}: {finding['rule']}: "
            f"{finding['message']}",
            file=sys.stderr,
        )
    return 1 if kept else 0


if __name__ == "__main__":
    raise SystemExit(main())
