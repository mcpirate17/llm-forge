#!/usr/bin/env python3
"""PostToolUse read-token budget: tally what ``Read`` pulls into context.

Every Read response's text is counted (~4 chars/token) into a per-session
ledger next to the graph-gate state. Each time the running total crosses
another ``READ_BUDGET_STEP_TOKENS`` (default 30,000) the hook adds one
advisory line pointing at the cheaper graph tools. It never blocks; it exists
to make the delegation rule visible and to produce the numbers that decide
whether a hard limit is warranted. Harness-agnostic (Claude/Codex/Qwen/Grok
payload parsing shared with ``crg_gate.py``).
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path
from typing import Any, Final

HOOK_DIR: Final[Path] = Path(__file__).resolve().parent
sys.path.insert(0, str(HOOK_DIR))
from crg_gate import _read_payload, _state_dir, _state_key  # noqa: E402

STEP_ENV: Final[str] = "READ_BUDGET_STEP_TOKENS"
DEFAULT_STEP: Final[int] = 30_000
CHARS_PER_TOKEN: Final[int] = 4
MAX_COUNTED_CHARS: Final[int] = 4_000_000
ADVICE: Final[str] = (
    "Prefer locate_tool / ast_context_tool / symbol_source_tool / query_graph, "
    "Read with offset+limit, or delegate bulk reading to a subagent."
)


def response_chars(value: Any, limit: int = MAX_COUNTED_CHARS) -> int:
    """Total characters of text in a tool response (dict/list walked, bounded)."""
    if isinstance(value, str):
        return min(len(value), limit)
    total = 0
    if isinstance(value, dict):
        items: list[Any] = list(value.values())
    elif isinstance(value, list):
        items = value
    else:
        return 0
    for item in items:
        total += response_chars(item, limit - total)
        if total >= limit:
            return limit
    return total


def step_tokens() -> int:
    raw = os.environ.get(STEP_ENV, "").strip()
    return int(raw) if raw else DEFAULT_STEP


def tally(state_dir: Path, key: str, tokens: int) -> tuple[int, int]:
    """Add *tokens* to the session ledger; return (previous_total, new_total)."""
    path = state_dir / f"{key}.read-tokens"
    try:
        previous = int(path.read_text(encoding="utf-8").strip() or "0")
    except (OSError, ValueError):
        previous = 0
    new_total = previous + tokens
    path.write_text(f"{new_total}\n", encoding="utf-8")
    return previous, new_total


def hook_output(payload: Any, state_dir: Path) -> dict[str, Any]:
    out: dict[str, Any] = {"hookSpecificOutput": {"hookEventName": "PostToolUse"}}
    if not isinstance(payload, dict):
        return out
    key = _state_key(payload)
    tokens = response_chars(payload.get("tool_response")) // CHARS_PER_TOKEN
    if key is None or tokens == 0:
        return out
    previous, total = tally(state_dir, key, tokens)
    step = step_tokens()
    if total // step > previous // step:
        out["hookSpecificOutput"]["additionalContext"] = (
            f"READ BUDGET: {total:,} tokens pulled into context via Read this "
            f"session (crossed {step * (total // step):,}). {ADVICE}"
        )
    return out


def main() -> int:
    print(json.dumps(hook_output(_read_payload(), _state_dir())))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
