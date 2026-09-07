"""Run clang-format or clang-tidy over the lines a candidate changed.

The repository had no C-family formatter at all, and 125 host C/C++ files that
no base clang-format style matches. Adopting one whole-file would make the next
person to touch any of them inherit every prior line's drift; adopting it over
the changed lines converges the tree file by file, at the moment someone is
already editing it. That is the same "ahead of the commit, not after it" shape
the rest of the gate has, so it is what this does.

clang-tidy additionally needs a compile database. Where none exists the file is
skipped with a reason on stderr rather than analyzed against guessed include
paths, because a linter reporting missing headers as errors is noise that
trains people to ignore it.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path

from conductor.candidate_review.diff_ranges import (
    DiffError,
    changed_line_ranges,
    merge_ranges,
)

COMPILE_DB = "compile_commands.json"
TIDY_SUFFIXES = {".c", ".cc", ".cpp", ".cxx", ".h", ".hpp"}


def _tool(mode: str) -> str:
    """The clang binary for `mode`, preferring the one beside this interpreter.

    Both come from PyPI wheels pinned in uv.lock, so the copy in the venv is the
    version CI runs. Looking there first keeps the check independent of whether
    the review sandbox happened to put `.venv/bin` on PATH.
    """
    name = "clang-format" if mode == "format" else "clang-tidy"
    beside = Path(sys.executable).parent / name
    if beside.is_file():
        return str(beside)
    resolved = shutil.which(name)
    if resolved is None:
        raise FileNotFoundError(f"{name} is not beside {sys.executable} or on PATH")
    return resolved


def _ranges(path: str, *, base: str | None, root: Path) -> list[tuple[int, int]] | None:
    """Merged changed ranges, or None when the whole file should be analyzed."""
    if not base:
        return None
    return merge_ranges(changed_line_ranges(path, base=base, cwd=root))


def _compile_db(path: Path, *, root: Path) -> Path | None:
    """The nearest `compile_commands.json` above `path`, then `<root>/build`."""
    candidate = path.parent
    while candidate.is_relative_to(root):
        if (candidate / COMPILE_DB).is_file():
            return candidate / COMPILE_DB
        if candidate == root:
            break
        candidate = candidate.parent
    fallback = root / "build" / COMPILE_DB
    return fallback if fallback.is_file() else None


def _format_command(
    tool: str, path: str, ranges: list[tuple[int, int]] | None
) -> list[str]:
    command = [tool, "--style=file", "--dry-run", "--Werror"]
    for start, end in ranges or ():
        command.append(f"--lines={start}:{end}")
    command.append(path)
    return command


def _tidy_command(
    tool: str, path: str, ranges: list[tuple[int, int]] | None, database: Path
) -> list[str]:
    command = [tool, f"-p={database.parent}", "--warnings-as-errors=*"]
    if ranges:
        line_filter = [{"name": Path(path).name, "lines": [list(r) for r in ranges]}]
        command.append(f"-line-filter={json.dumps(line_filter)}")
    command.append(path)
    return command


def _analyzable(mode: str, path: str, *, root: Path) -> tuple[bool, Path | None]:
    """Whether `path` can be analyzed under `mode`, and its compile database."""
    if mode == "format":
        return True, None
    if Path(path).suffix.lower() not in TIDY_SUFFIXES:
        print(
            f"clang-tidy: skipping {path} (not a host C/C++ translation unit)",
            file=sys.stderr,
        )
        return False, None
    database = _compile_db((root / path).resolve(), root=root)
    if database is None:
        print(
            f"clang-tidy: skipping {path} (no {COMPILE_DB} above it or in build/; "
            "configure CMake with -DCMAKE_EXPORT_COMPILE_COMMANDS=ON)",
            file=sys.stderr,
        )
        return False, None
    return True, database


def _check_one(mode: str, tool: str, path: str, *, base: str | None, root: Path) -> int:
    analyzable, database = _analyzable(mode, path, root=root)
    if not analyzable:
        return 0
    ranges = _ranges(path, base=base, root=root)
    if ranges is not None and not ranges:
        return 0
    command = (
        _format_command(tool, path, ranges)
        if mode == "format"
        else _tidy_command(tool, path, ranges, database)  # type: ignore[arg-type]
    )
    return subprocess.run(command, check=False, cwd=root).returncode


def version(mode: str) -> int:
    return subprocess.run([_tool(mode), "--version"], check=False).returncode


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("format", "tidy"), required=True)
    parser.add_argument("--base", default="")
    parser.add_argument("--version", action="store_true")
    parser.add_argument("files", nargs="*")
    args = parser.parse_args(sys.argv[1:] if argv is None else argv)
    if args.version:
        return version(args.mode)

    root = Path.cwd().resolve()
    tool = _tool(args.mode)
    status = 0
    for path in args.files:
        try:
            status = (
                _check_one(args.mode, tool, path, base=args.base, root=root) or status
            )
        except DiffError as error:
            print(f"clang-{args.mode}: {error}", file=sys.stderr)
            return 1
    return status


if __name__ == "__main__":
    raise SystemExit(main())
