"""Legacy Read/Grep/MCP output bounding entrypoints backed by forge."""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
import _bash_quiet as _bq

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from native_legacy_bridge import call

CAP_DEFAULT_BYTES = 16000


def _cap() -> int:
    return int(os.environ.get("TOOL_OUTPUT_QUIET_BYTES", str(CAP_DEFAULT_BYTES)))


def _request(payload: Any) -> dict[str, Any]:
    cap = _cap()
    return {
        "payload": payload,
        "limit_bytes": max(cap, 0),
        "disabled": cap <= 0,
        "save_dir": str(_bq.SAVE_DIR),
        "repo_root": str(_bq.REPO_ROOT),
        "stamp": _bq._now_stamp(),
        "output_field": _bq.OUTPUT_FIELD,
    }


def bound_response(response: Any) -> Any | None:
    return call("tool-quiet-response", **_request(response))


def hook_output(payload: Any) -> dict[str, Any]:
    return call("tool-quiet-envelope", **_request(payload))


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, OSError):
        payload = None
    print(json.dumps(hook_output(payload)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
