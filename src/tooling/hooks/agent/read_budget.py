"""Legacy read-budget hook entrypoints backed by forge."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parent))
from crg_gate import _read_payload, _state_dir

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from native_legacy_bridge import call

STEP_ENV = "READ_BUDGET_STEP_TOKENS"
DEFAULT_STEP = 30_000
CHARS_PER_TOKEN = 4
MAX_COUNTED_CHARS = 4_000_000


def response_chars(value: Any, limit: int = MAX_COUNTED_CHARS) -> int:
    return call("read-response-chars", payload=value, limit=limit)


def tally(state_dir: Path, key: str, tokens: int) -> tuple[int, int]:
    previous, total = call(
        "read-tally", state_dir=str(state_dir), key=key, tokens=tokens
    )
    return previous, total


def hook_output(payload: Any, state_dir: Path) -> dict[str, Any]:
    return call("read-budget", payload=payload, state_dir=str(state_dir))


def main() -> int:
    print(json.dumps(hook_output(_read_payload(), _state_dir())))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
