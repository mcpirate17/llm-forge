"""Legacy Bash output bounding entrypoints backed by forge."""

from __future__ import annotations

import os
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from native_legacy_bridge import call, run_main

REPO_ROOT = Path(
    os.environ.get("PROJECT_DIR") or Path(__file__).resolve().parents[3]
).resolve()
LIMIT_BYTES = int(os.environ.get("BASH_QUIET_LIMIT_BYTES", "8000"))
OUTPUT_FIELD = os.environ.get("BASH_QUIET_OUTPUT_FIELD", "updatedToolOutput")
HEAD_LINES = 60
TAIL_LINES = 30
TEXT_FIELDS = ("stdout", "stderr", "output")


def _save_dir() -> Path:
    configured = os.environ.get("BASH_QUIET_SAVE_DIR", "").strip()
    if not configured:
        return Path(tempfile.gettempdir()) / "agent-bash-output"
    path = Path(configured).expanduser()
    return path if path.is_absolute() else REPO_ROOT / path


SAVE_DIR = _save_dir()


def _now_stamp() -> str:
    return time.strftime("%Y%m%dT%H%M%S")


def _request(payload: Any, limit_bytes: int | None = None) -> dict[str, Any]:
    return {
        "payload": payload,
        "limit_bytes": LIMIT_BYTES if limit_bytes is None else limit_bytes,
        "save_dir": str(SAVE_DIR),
        "repo_root": str(REPO_ROOT),
        "stamp": _now_stamp(),
        "output_field": OUTPUT_FIELD,
    }


def bound_response(response: Any) -> Any | None:
    return call("bash-quiet-response", **_request(response))


def hook_output(payload: Any) -> dict[str, Any]:
    return call("bash-quiet-envelope", **_request(payload))


def main() -> int:
    return run_main(hook_output)


if __name__ == "__main__":
    raise SystemExit(main())
