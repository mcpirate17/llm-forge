"""Run cppcheck only on the C-family subset of native candidate files."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path
from typing import Final

C_FAMILY_SUFFIXES: Final = frozenset(
    {".c", ".cc", ".cpp", ".cxx", ".h", ".hh", ".hpp", ".hxx", ".cu", ".cuh"}
)


def main(argv: list[str] | None = None) -> int:
    paths = [
        value
        for value in (sys.argv[1:] if argv is None else argv)
        if Path(value).suffix.lower() in C_FAMILY_SUFFIXES
    ]
    if not paths:
        return 0
    completed = subprocess.run(
        [
            "cppcheck",
            "--enable=warning,performance,portability",
            "--error-exitcode=1",
            "--inline-suppr",
            "--language=c++",
            "--std=c++17",
            *paths,
        ],
        check=False,
    )
    return completed.returncode


if __name__ == "__main__":
    raise SystemExit(main())
