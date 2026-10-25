"""Report Python loops over tensors and arrays in the folders a host calls hot.

`perf/python-tensor-loop` is Rust (`slop_core.hot_loop_scan_files`, tree-sitter).
This module is the folder list; the CLI, changed-line scoping and printing are
`style_scan.run_python_scan`.

Like style_scan, the exit status is 1 when a finding sits on a changed line; the
policy row's severity ("medium") is what makes the gate treat it as advisory. A
repository without `[tool.forge.hot_paths]` gets no findings, and the first line
printed says which folders were used.
"""

from __future__ import annotations

import sys
from pathlib import Path

from conductor.candidate_review.style_scan import run_python_scan
from conductor.hot_paths_config import hot_paths


def _scan(paths: list[str]) -> list[dict]:
    folders = hot_paths(Path.cwd().resolve())
    if not folders:
        print(
            "hot-loop-scan: no [tool.forge.hot_paths] in pyproject.toml; nothing scanned",
            file=sys.stderr,
        )
        return []
    print(f"hot-loop-scan: hot folders: {', '.join(folders)}", file=sys.stderr)
    import slop_core

    return list(slop_core.hot_loop_scan_files(paths, list(folders)))


def main(argv: list[str] | None = None) -> int:
    return run_python_scan(
        argv, version="hot-loop-scan 1", description=__doc__, scan=_scan
    )


if __name__ == "__main__":
    raise SystemExit(main())
