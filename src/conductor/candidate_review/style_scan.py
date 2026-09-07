"""Refuse effort-signalling comments, dead scaffolding and swallowed errors.

Three rules in CLAUDE.md had no detector reaching the gate. "No effort-signaling
code" is unenforced because no Python linter reads comments -- ruff discards them
before it starts. Dead scaffolding is unenforced for stubs, unreachable
statements and constant conditions, because ruff's dead-code rules stop at unused
*names*. "Fail loud -- no silent fallbacks, no swallowed exceptions" had a
classifier written for it, but only the repository audit ever called it.

A fourth rule is configuration rather than style: a URL, an address or a UUID
written into the call that uses it is a name for something outside this
repository that a reader cannot find and a deployment cannot override. The rule
asks only that it be bound once at the top of the file.

The detectors live in Rust. For Python, `slop_core.style_scan_files` reads
comments, dead code and wired-in addresses with tree-sitter and `slop_core.fallback_scan_files`
classifies exception handlers against CPython's own AST. For Rust,
`slop_core.rust_scan_files` reports `.unwrap()` and `todo!()` outside test
scope -- "fail loud" in the language this repository's compute is written in.
This module is argv, line scoping and printing for both.

Scoping is by changed line, not changed file. Measured over the 3303 tracked
Python files the rules report 720 pre-existing findings, 591 of them swallowed
errors and 16 hardcoded endpoints; over the 127 tracked Rust files, 38 across
four files. Reporting those
would red-gate a candidate for lines it never touched, which is how a check gets
bypassed. Reporting only the lines it wrote refuses the first *new* one at the
commit that writes it, and leaves the rest to the person who eventually edits
that line -- the same converging shape as clang-format's line scoping.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from conductor.candidate_review.diff_ranges import DiffError, changed_line_ranges

RULES = (
    "comment/change-meta",
    "comment/effort-narrative",
    "comment/trivial-restatement",
    "config/hardcoded-endpoint",
    "config/hardcoded-id",
    "dead/constant-condition",
    "dead/empty-function",
    "dead/unreachable-statement",
    "failure/silent-fallback",
)

RUST_RULES = (
    "dead/rust-stub",
    "failure/rust-unwrap",
)

# The suffixes each language's scanners can read. The gate hands a check every
# file in its classes, and `rust` carries the lockfile and the manifests too.
SUFFIXES = {"python": ".py", "rust": ".rs"}


def _scan(paths: list[str], language: str) -> list[dict]:
    """Findings for `paths`, from the native scanners for `language`.

    The import is deferred so `--version` answers on a machine where the
    extension has not been built, which is the one question the gate's tool
    preflight asks before it decides whether to schedule this check at all.
    """
    import slop_core

    if language == "rust":
        return list(slop_core.rust_scan_files(paths))
    findings = list(slop_core.style_scan_files(paths))
    findings += list(slop_core.fallback_scan_files(paths))
    # Two scanners, each sorted within itself, would otherwise report a file
    # twice over: every comment finding, then every swallowed error. One pass
    # down the file is the order the reader is going to read it in.
    findings.sort(key=lambda row: (row["path"], row["line"], row["rule"]))
    return findings


def in_range(line: int, ranges: tuple[tuple[int, int], ...]) -> bool:
    return any(start <= line <= end for start, end in ranges)


def scope(findings: list[dict], base: str, root: Path) -> tuple[list[dict], list[str]]:
    """Keep the findings that sit on lines this candidate changed.

    An empty `base` means there is nothing to diff against -- a full-tree run --
    and every finding is kept. A file with no changed lines at all is dropped
    whole: it was listed for a mode or rename change, not for its content.
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
        if in_range(finding["line"], ranges[path]):
            kept.append(finding)
    return kept, unreadable


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="")
    parser.add_argument("--language", choices=sorted(SUFFIXES), default="python")
    parser.add_argument("--version", action="store_true")
    parser.add_argument("files", nargs="*")
    args = parser.parse_args(sys.argv[1:] if argv is None else argv)
    if args.version:
        print("style-scan 4")
        return 0

    paths = [f for f in args.files if f.endswith(SUFFIXES[args.language])]
    if not paths:
        return 0

    root = Path.cwd().resolve()
    findings = _scan(paths, args.language)
    kept, unreadable = scope(findings, args.base, root)
    for message in unreadable:
        print(f"style-scan: {message}", file=sys.stderr)
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
