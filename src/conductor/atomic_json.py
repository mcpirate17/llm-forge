"""Atomic JSON writes shared by the workspace's small state files."""

from __future__ import annotations

import json
import os
import tempfile
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import IO, Any


@contextmanager
def atomic_write(path: Path) -> Iterator[IO[str]]:
    """Yield a handle whose block, completed, replaces ``path`` atomically.

    The scaffold every atomic writer needs -- parent directory, sibling
    temporary file, fsync, rename, and cleanup of the temporary on any exit --
    lives here once; writers only say what goes into the handle.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary: Path | None = None
    try:
        with tempfile.NamedTemporaryFile(
            "w",
            encoding="utf-8",
            dir=path.parent,
            prefix=f".{path.name}.",
            suffix=".tmp",
            delete=False,
        ) as handle:
            temporary = Path(handle.name)
            yield handle
            handle.flush()
            os.fsync(handle.fileno())
        temporary.replace(path)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def write_json_atomic(path: Path, payload: dict[str, Any]) -> None:
    """Write ``payload`` to ``path`` atomically, leaving no partial file behind."""
    with atomic_write(path) as handle:
        json.dump(payload, handle, indent=2, sort_keys=True)
        handle.write("\n")


def write_lines_atomic(path: Path, lines: Iterator[str]) -> Path:
    """Write ``lines`` to ``path`` atomically, returning ``path``.

    For streamed output (a JSONL index built row by row) that must not leave a
    half-written file behind when the process dies mid-write.
    """
    with atomic_write(path) as handle:
        for line in lines:
            handle.write(line.rstrip("\n") + "\n")
    return path
