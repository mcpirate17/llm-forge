"""Forward old hook-body calls to the single native implementation.

These entrypoints remain for installed settings that still name Python files.
The bridge intentionally has no Python implementation of any hook decision.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path
from typing import Any

from conductor.project_init import resolve_forge_binary


def _binary() -> Path:
    configured = os.environ.get("FORGE_BIN")
    if configured is not None:
        if not configured:
            raise RuntimeError("FORGE_BIN is empty; legacy hook requires forge")
        found = shutil.which(configured)
        if found is None:
            raise RuntimeError(
                f"FORGE_BIN does not resolve to an executable: {configured}"
            )
        return Path(found)
    root = Path(
        os.environ.get("PROJECT_DIR")
        or os.environ.get("CLAUDE_PROJECT_DIR")
        or Path(__file__).resolve().parents[3]
    )
    found = resolve_forge_binary(root)
    if found is None:
        raise RuntimeError(f"forge binary unavailable for legacy hook in {root}")
    return found


def call(operation: str, **request: Any) -> Any:
    """Call one hidden native hook operation, surfacing a missing/stale binary."""
    try:
        process = subprocess.run(
            [str(_binary()), "legacy-hook", operation],
            input=json.dumps(request),
            text=True,
            capture_output=True,
            check=False,
            timeout=30,
        )
    except subprocess.TimeoutExpired as exc:
        raise RuntimeError(
            f"forge legacy-hook {operation} timed out after 30s"
        ) from exc
    if process.stderr:
        # Native warnings (for example an unrecognized response shape) are
        # part of the hook contract even on a successful call.
        import sys

        print(process.stderr, end="", file=sys.stderr)
    if process.returncode:
        raise RuntimeError(
            f"forge legacy-hook {operation} exited {process.returncode}: "
            f"{process.stderr.strip() or process.stdout.strip() or 'no output'}"
        )
    try:
        return json.loads(process.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(
            f"forge legacy-hook {operation} returned invalid JSON"
        ) from exc
