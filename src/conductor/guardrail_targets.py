"""Root-relative audit configuration and pruned source discovery."""

from __future__ import annotations

import os
import tomllib
from collections.abc import Iterable
from pathlib import Path

SKIP_PARTS = frozenset(
    {
        "node_modules",
        ".venv",
        "venv",
        "__pycache__",
        ".git",
        "archive",
        ".pytest_cache",
        ".mypy_cache",
        ".ruff_cache",
        "dist",
        "build",
        "target",
        ".run",
        ".agents",
        ".code-review-graph",
    }
)


def resolve_targets(root: Path, explicit: list[str] | None) -> tuple[str, ...]:
    """CLI targets override the selected host's config; the default is its tree."""
    configured = explicit
    manifest = root / "pyproject.toml"
    if configured is None and manifest.is_file():
        document = tomllib.loads(manifest.read_text(encoding="utf-8"))
        tool = document.get("tool", {})
        if not isinstance(tool, dict) or not isinstance(
            tool.get("conductor", {}), dict
        ):
            raise ValueError("tool.conductor must be a table")
        configured = tool.get("conductor", {}).get("guardrail_targets")
    if configured is None:
        configured = ["."]
    if not isinstance(configured, list) or not configured:
        raise ValueError(
            "guardrail_targets must be a nonempty list of root-relative paths"
        )
    targets = []
    for value in configured:
        if not isinstance(value, str) or not value.strip():
            raise ValueError("guardrail_targets entries must be nonempty strings")
        path = Path(value)
        if path.is_absolute() or ".." in path.parts:
            raise ValueError(
                f"audit target must be root-relative and cannot escape: {value}"
            )
        resolved = root / path
        if not resolved.resolve().is_relative_to(root.resolve()):
            raise ValueError(f"audit target escapes the selected root: {value}")
        if not resolved.exists():
            raise ValueError(f"audit target does not exist in {root}: {value}")
        targets.append(path.as_posix())
    return tuple(dict.fromkeys(targets))


def walk_sources(root: Path, targets: Iterable[str]) -> Iterable[Path]:
    """Prune generated trees before descending, rather than after rglob."""
    for target in targets:
        base = root / target
        if base.is_symlink():
            continue
        if base.is_file():
            yield base
            continue
        for directory, directories, filenames in os.walk(base, followlinks=False):
            directories[:] = sorted(
                name
                for name in directories
                if name not in SKIP_PARTS and not (Path(directory) / name).is_symlink()
            )
            for name in sorted(filenames):
                path = Path(directory) / name
                if not path.is_symlink():
                    yield path
