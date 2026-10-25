"""Report Python loops over tensors and arrays in the folders a host calls hot.

`perf/python-tensor-loop` is Rust (`slop_core.hot_loop_scan_files`, tree-sitter).
This module is argv, the folder list, changed-line scoping and printing.

Like style_scan, the exit status is 1 when a finding sits on a changed line; the
policy row's severity ("medium") is what makes the gate treat it as advisory. A repository without `[tool.forge.hot_paths]`
gets no findings, and the first line printed says which folders were used.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from conductor.candidate_review.style_scan import scope
from conductor.hot_paths_config import hot_paths


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="")
    parser.add_argument("--version", action="store_true")
    parser.add_argument("files", nargs="*")
    args = parser.parse_args(sys.argv[1:] if argv is None else argv)
    if args.version:
        print("hot-loop-scan 1")
        return 0

    root = Path.cwd().resolve()
    folders = hot_paths(root)
    if not folders:
        print(
            "hot-loop-scan: no [tool.forge.hot_paths] in pyproject.toml; nothing scanned",
            file=sys.stderr,
        )
        return 0
    print(f"hot-loop-scan: hot folders: {', '.join(folders)}", file=sys.stderr)
    paths = [f for f in args.files if f.endswith(".py")]
    if not paths:
        return 0

    import slop_core

    findings = list(slop_core.hot_loop_scan_files(paths, list(folders)))
    kept, unreadable = scope(findings, args.base, root)
    for message in unreadable:
        print(f"hot-loop-scan: {message}", file=sys.stderr)
    if unreadable:
        return 1
    for finding in kept:
        print(
            f"{finding['path']}:{finding['line']}: {finding['rule']}: "
            f"{finding['message']}",
            file=sys.stderr,
        )
    print(f"hot-loop-scan: {len(kept)} finding(s)", file=sys.stderr)
    return 1 if kept else 0


if __name__ == "__main__":
    raise SystemExit(main())
