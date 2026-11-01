"""Remaining compatibility accessor for native executable selection.

Host bootstrap is now `forge project`, `forge providers` and `forge mcp`.
This accessor remains only until the ledger and A2A Python adapters are retired.
"""

from __future__ import annotations

import os
import shutil
import sys
from pathlib import Path


def resolve_forge_binary(
    project_dir: Path, python: Path | None = None
) -> Path | None:
    """Return the executable selected by the native Forge binary policy."""
    from conductor._native import resolve_forge_binary_native

    selected = resolve_forge_binary_native(
        str(project_dir),
        str(python or sys.executable),
        os.environ.get("FORGE_BIN"),
        shutil.which("forge"),
    )
    return Path(selected) if selected is not None else None
