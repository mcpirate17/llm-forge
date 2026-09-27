"""Compatibility entrypoints for native shell write-target extraction."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from native_legacy_bridge import call

OPAQUE_WRITE = "<opaque-interpreter-write>"


def split_commands(tokens: list[str]) -> list[list[str]]:
    return call("split-commands", tokens=tokens)


def write_targets(command: str, *, _relative: bool = True) -> list[str]:
    # The native source computes the same targets for both keyword values.
    return call("write-targets", command=command)


def working_directory(command: str, repo_root: Path) -> Path | None:
    result = call("working-directory", command=command, repo_root=str(repo_root))
    return Path(result) if result is not None else None


def repo_write_targets(command: str, repo_root: Path) -> list[str]:
    return call("repo-write-targets", command=command, repo_root=str(repo_root))
