"""Repair Python console-script launchers after a hardlinked virtualenv clone."""

from __future__ import annotations

import argparse
import os
import re
import stat
import tempfile
from pathlib import Path


class WorktreeRuntimeError(RuntimeError):
    """A venv launcher relocation request is unsafe or incomplete."""


PYTHON_EXECUTABLE = re.compile(r"python(?:\d+(?:\.\d+)?)?$")


def _python_shebangs(venv: Path) -> dict[bytes, bytes]:
    """Map an exact source-venv Python shebang to its executable name."""

    source_bin = venv.joinpath("bin")
    names = [
        path.name
        for path in source_bin.iterdir()
        if PYTHON_EXECUTABLE.fullmatch(path.name) and path.is_file()
    ]
    if not names:
        raise WorktreeRuntimeError(f"no Python executable under {source_bin}")
    return {
        os.fsencode(f"#!{source_bin.joinpath(name)}"): os.fsencode(name)
        for name in names
    }


def _rewritten_script(
    payload: bytes, replacements: dict[bytes, bytes], destination: Path
) -> bytes | None:
    """Return payload with an exact source Python shebang relocated, else ``None``."""

    first, separator, rest = payload.partition(b"\n")
    if separator:
        replacement_name = replacements.get(first.rstrip(b"\r"))
        if replacement_name is not None:
            return (
                b"#!"
                + os.fsencode(destination.joinpath("bin"))
                + b"/"
                + replacement_name
                + b"\n"
                + rest
            )


def relocate_console_scripts(source_venv: Path, destination_venv: Path) -> list[Path]:
    """Copy-on-write source-Python console scripts into ``destination_venv``.

    Source and destination must be separate existing venvs. Symlinks, binaries,
    non-regular entries, and scripts using another interpreter are ignored. A
    second invocation makes no changes.
    """

    source = Path(source_venv).resolve()
    destination = Path(destination_venv).resolve()
    if source == destination:
        raise WorktreeRuntimeError("source and destination virtualenvs must differ")
    source_bin = source.joinpath("bin")
    destination_bin = destination.joinpath("bin")
    if not source_bin.is_dir() or not destination_bin.is_dir():
        raise WorktreeRuntimeError("both virtualenvs must contain a bin directory")
    if source_bin.is_symlink() or destination_bin.is_symlink():
        raise WorktreeRuntimeError("virtualenv bin directories must not be symlinks")

    replacements = _python_shebangs(source)
    relocated: list[Path] = []
    for source_entry in source_bin.iterdir():
        destination_entry = destination_bin.joinpath(source_entry.name)
        if source_entry.is_symlink():
            continue
        if destination_entry.is_symlink():
            continue
        if not source_entry.is_file():
            continue
        if not destination_entry.is_file():
            continue
        destination_mode = destination_entry.stat().st_mode
        with destination_entry.open("rb") as script:
            first_line = script.readline(4097)
        if b"\0" in first_line:
            continue
        if first_line.rstrip(b"\r\n") not in replacements:
            continue
        rewritten = _rewritten_script(
            destination_entry.read_bytes(), replacements, destination
        )
        if rewritten is None:
            continue
        with tempfile.NamedTemporaryFile(dir=destination_bin, delete=False) as scratch:
            scratch.write(rewritten)
            scratch_path = Path(scratch.name)
        try:
            os.chmod(scratch_path, stat.S_IMODE(destination_mode))
            os.replace(scratch_path, destination_entry)
        finally:
            scratch_path.unlink(missing_ok=True)
        relocated.append(destination_entry)
    return relocated


def main(argv: list[str] | None = None) -> int:
    """Relocate eligible entrypoints from SOURCE_VENV to DESTINATION_VENV."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source_venv", type=Path)
    parser.add_argument("destination_venv", type=Path)
    args = parser.parse_args(argv)
    try:
        relocated = relocate_console_scripts(args.source_venv, args.destination_venv)
    except WorktreeRuntimeError as exc:
        parser.error(str(exc))
    print(f"relocated {len(relocated)} Python console script(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
