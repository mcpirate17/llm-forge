"""The legacy Python entrypoint reaches the native guard verdict."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import _bash_guard as guard


def test_python_binding_uses_the_native_guard() -> None:
    assert guard.check("git reset --hard HEAD") == (
        "BLOCKED: git reset --hard destroys uncommitted work. Stash or commit first."
    )
    assert guard.check("git push --force-with-lease origin main") is None
    assert guard.check_command(["pip", "install", "numpy"]) == (
        "BLOCKED: Use 'uv pip install' instead of raw pip."
    )
